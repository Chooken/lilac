use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    DidOpenTextDocumentParams, GotoDefinitionResponse, Location, PublishDiagnosticsParams,
    TextDocumentItem, Url,
};

fn start_server() -> (Connection, std::thread::JoinHandle<()>) {
    let (server, client) = Connection::memory();
    let handle = std::thread::spawn(move || {
        let _ = lilac_lsp::run(server);
    });
    (client, handle)
}

fn read_response(client: &Connection, id: &RequestId) -> lsp_server::Response {
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Response(response)) if &response.id == id => return response,
            Ok(_) => {}
            Err(err) => panic!("no response for {id:?}: {err}"),
        }
    }
}

fn initialize(client: &Connection) {
    let id = RequestId::from(1);
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "initialize".to_owned(),
            params: serde_json::json!({ "capabilities": {} }),
        }))
        .expect("initialize");
    read_response(client, &id);
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("initialized");
}

fn initialize_root(client: &Connection, root: &Url) {
    let id = RequestId::from(1);
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "initialize".to_owned(),
            params: serde_json::json!({
                "capabilities": {},
                "workspaceFolders": [{ "uri": root.as_str(), "name": "ws" }],
            }),
        }))
        .expect("initialize");
    read_response(client, &id);
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("initialized");
}

fn shutdown(client: &Connection) {
    let id = RequestId::from(2);
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "shutdown".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("shutdown");
    read_response(client, &id);
    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("exit");
}

fn did_open(client: &Connection, uri: &Url, text: &str) {
    let params = DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri.clone(),
            language_id: "lilac".to_owned(),
            version: 1,
            text: text.to_owned(),
        },
    };
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didOpen".to_owned(),
            params: serde_json::to_value(params).expect("serialize"),
        }))
        .expect("didOpen");
}

fn publish_for(client: &Connection, uri: &Url) -> PublishDiagnosticsParams {
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Notification(notification))
                if notification.method == "textDocument/publishDiagnostics" =>
            {
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(notification.params).expect("decode");
                if &params.uri == uri {
                    return params;
                }
            }
            Ok(_) => {}
            Err(err) => panic!("no diagnostics for {uri}: {err}"),
        }
    }
}

fn definition_at(client: &Connection, uri: &Url, line: u32, character: u32) -> Option<Location> {
    let id = RequestId::from(60);
    let params = serde_json::json!({
        "textDocument": { "uri": uri.as_str() },
        "position": { "line": line, "character": character },
    });
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "textDocument/definition".to_owned(),
            params,
        }))
        .expect("definition");
    let response = read_response(client, &id);
    match response.response_result {
        Ok(value) if !value.is_null() => serde_json::from_value::<GotoDefinitionResponse>(value)
            .ok()
            .and_then(|response| match response {
                GotoDefinitionResponse::Scalar(location) => Some(location),
                GotoDefinitionResponse::Array(mut locations) => locations.pop(),
                GotoDefinitionResponse::Link(_) => None,
            }),
        _ => None,
    }
}

#[test]
fn definition_jumps_to_the_callee_declaration() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "foo: function () nothing {\n}\nmain: function () nothing {\n    foo()\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let location = definition_at(&client, &uri, 3, 5).expect("definition");
    assert_eq!(location.uri, uri);
    assert_eq!(location.range.start.line, 0);

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn definition_crosses_files() {
    // project root with two files; main calls a function declared in lib
    let dir = std::env::temp_dir().join(format!("lilac-lsp-def-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("lib.lilac"), "helper: function () nothing {\n}\n").expect("write lib");
    let main = dir.join("main.lilac");
    let main_text = "main: function () nothing {\n    helper()\n}\n";
    std::fs::write(&main, main_text).expect("write main");
    let root = Url::from_directory_path(&dir).expect("root uri");
    let main_uri = Url::from_file_path(&main).expect("uri");

    let (client, handle) = start_server();
    initialize_root(&client, &root);
    did_open(&client, &main_uri, main_text);
    let _ = publish_for(&client, &main_uri);

    let location = definition_at(&client, &main_uri, 1, 5).expect("definition");
    assert!(
        location.uri.path().ends_with("lib.lilac"),
        "{}",
        location.uri
    );
    assert_eq!(location.range.start.line, 0);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `file:` URI formatted the way `VSCode` sends it on Windows: lowercase drive
/// letter and a percent-encoded colon (`file:///c%3A/...`).
fn vscode_file_uri(path: &std::path::Path) -> Url {
    let normalized = path.to_string_lossy().replace('\\', "/");
    let drive = normalized[..1].to_lowercase();
    let rest = normalized[1..].replace(':', "%3A");
    Url::parse(&format!("file:///{drive}{rest}")).expect("vscode uri")
}

/// Waits for any `publishDiagnostics` (the server publishes under its own URI
/// format, which may differ from the client's).
fn wait_any_publish(client: &Connection) {
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Notification(notification))
                if notification.method == "textDocument/publishDiagnostics" =>
            {
                return;
            }
            Ok(_) => {}
            Err(err) => panic!("no publish: {err}"),
        }
    }
}

#[test]
fn definition_in_project_mode_with_vscode_style_uri() {
    let dir = std::env::temp_dir().join(format!("lilac-lsp-def-vsc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let main = dir.join("main.lilac");
    let text = "foo: function () nothing {\n}\nmain: function () nothing {\n    foo()\n}\n";
    std::fs::write(&main, text).expect("write");
    let root = Url::from_directory_path(&dir).expect("root uri");
    let main_uri = vscode_file_uri(&main);

    let (client, handle) = start_server();
    initialize_root(&client, &root);
    did_open(&client, &main_uri, text);
    wait_any_publish(&client);

    let location = definition_at(&client, &main_uri, 3, 5).expect("definition");
    assert_eq!(location.range.start.line, 0);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn definition_uses_the_lilac_toml_root() {
    // The workspace root is `dir`, but `lilac.toml` points the project at
    // `dir/proj`. Cross-file definition proves the project (not a per-file
    // fallback) compiled, since a standalone `main.lilac` could not resolve
    // `helper` in `lib.lilac`.
    let dir = std::env::temp_dir().join(format!("lilac-lsp-def-cfg-{}", std::process::id()));
    let proj = dir.join("proj");
    std::fs::create_dir_all(&proj).expect("mkdir");
    std::fs::write(proj.join("lib.lilac"), "helper: function () nothing {\n}\n").expect("lib");
    let main = proj.join("main.lilac");
    let text = "main: function () nothing {\n    helper()\n}\n";
    std::fs::write(&main, text).expect("main");
    std::fs::write(dir.join("lilac.toml"), "[project]\nroot = \"proj\"\n").expect("config");
    let root = Url::from_directory_path(&dir).expect("root uri");
    let main_uri = vscode_file_uri(&main);

    let (client, handle) = start_server();
    initialize_root(&client, &root);
    did_open(&client, &main_uri, text);
    wait_any_publish(&client);

    let location = definition_at(&client, &main_uri, 1, 5).expect("definition");
    assert!(
        location.uri.path().ends_with("lib.lilac"),
        "{}",
        location.uri
    );

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn definition_falls_back_for_a_file_outside_the_project() {
    // The workspace root has no `.lilac` files, so the project is empty and the
    // open file must fall back to a standalone compile.
    let dir = std::env::temp_dir().join(format!("lilac-lsp-def-fb-{}", std::process::id()));
    let sub = dir.join("scratch");
    std::fs::create_dir_all(&sub).expect("mkdir");
    let main = sub.join("main.lilac");
    let text = "foo: function () nothing {\n}\nmain: function () nothing {\n    foo()\n}\n";
    std::fs::write(&main, text).expect("write");
    let root = Url::from_directory_path(&dir).expect("root uri");
    let main_uri = vscode_file_uri(&main);

    let (client, handle) = start_server();
    initialize_root(&client, &root);
    did_open(&client, &main_uri, text);
    wait_any_publish(&client);

    let location = definition_at(&client, &main_uri, 3, 5).expect("definition");
    assert_eq!(location.range.start.line, 0);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn definition_survives_a_type_error() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "pi: @bit32 = 0x2A\nmain: function () nothing {\n    y: @bit8 = pi\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let location = definition_at(&client, &uri, 2, 15).expect("definition");
    assert_eq!(location.range.start.line, 0);

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn definition_survives_a_syntax_error() {
    // A syntax error in one item must not clear the index: the valid `foo`
    // declaration and its call site in `main` still resolve.
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "broken: function ( {\n}\nfoo: function () nothing {\n}\nmain: function () nothing {\n    foo()\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let location = definition_at(&client, &uri, 5, 5).expect("definition");
    assert_eq!(location.uri, uri);
    assert_eq!(location.range.start.line, 2);

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn definition_survives_a_syntax_error_in_project() {
    // Same as above, but through the project compile path (`main` declares a
    // valid `foo` and calls it, with a sibling syntax error in the folder).
    let dir = std::env::temp_dir().join(format!("lilac-lsp-def-syn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let main = dir.join("main.lilac");
    let text = "broken: function ( {\n}\nfoo: function () nothing {\n}\nmain: function () nothing {\n    foo()\n}\n";
    std::fs::write(&main, text).expect("write");
    let root = Url::from_directory_path(&dir).expect("root uri");
    let main_uri = Url::from_file_path(&main).expect("uri");

    let (client, handle) = start_server();
    initialize_root(&client, &root);
    did_open(&client, &main_uri, text);
    let _ = publish_for(&client, &main_uri);

    let location = definition_at(&client, &main_uri, 5, 5).expect("definition");
    assert_eq!(location.range.start.line, 2);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn definition_on_a_member_field() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    v: Object = Object {\n        member = 0x1\n    }\n    x: @bit8 = v.member\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let line = 7; // "    x: @bit8 = v.member"
    let character = 18; // inside "member"
    let location = definition_at(&client, &uri, line, character).expect("definition");
    assert_eq!(location.range.start.line, 1); // "    member: @bit8"

    shutdown(&client);
    handle.join().expect("server thread");
}
