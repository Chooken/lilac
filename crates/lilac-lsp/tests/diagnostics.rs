use std::fmt::Write as _;
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, NumberOrString,
    PublishDiagnosticsParams, TextDocumentContentChangeEvent, TextDocumentItem, Url,
    VersionedTextDocumentIdentifier,
};

fn start_server() -> (Connection, std::thread::JoinHandle<()>) {
    let (server, client) = Connection::memory();
    let handle = std::thread::spawn(move || {
        let _ = lilac_lsp::run(server);
    });
    (client, handle)
}

fn read_response(client: &Connection, id: &RequestId) {
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Response(response)) if &response.id == id => return,
            Ok(_) => {}
            Err(err) => panic!("no response for {id:?}: {err}"),
        }
    }
}

fn initialize(client: &Connection, folder: Option<&Url>) {
    let params = match folder {
        Some(uri) => serde_json::json!({
            "capabilities": {},
            "workspaceFolders": [{ "uri": uri.as_str(), "name": "ws" }],
        }),
        None => serde_json::json!({ "capabilities": {} }),
    };
    initialize_with(client, params);
}

fn initialize_with(client: &Connection, params: serde_json::Value) {
    let id = RequestId::from(1);
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "initialize".to_owned(),
            params,
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

fn did_change(client: &Connection, uri: &Url, version: i32, text: &str) {
    let params = DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version,
        },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None,
            range_length: None,
            text: text.to_owned(),
        }],
    };
    client
        .sender
        .send(Message::Notification(Notification {
            method: "textDocument/didChange".to_owned(),
            params: serde_json::to_value(params).expect("serialize"),
        }))
        .expect("didChange");
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

fn has_code(params: &PublishDiagnosticsParams, code: &str) -> bool {
    params.diagnostics.iter().any(|diagnostic| {
        matches!(&diagnostic.code, Some(NumberOrString::String(found)) if found == code)
    })
}

fn main_uri() -> Url {
    Url::parse("file:///c:/main.lilac").expect("uri")
}

#[test]
fn clean_file_publishes_empty() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = main_uri();
    did_open(&client, &uri, "main: function () nothing {\n}\n");
    let params = publish_for(&client, &uri);
    assert!(params.diagnostics.is_empty(), "{:?}", params.diagnostics);
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn parse_error_is_e001() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = main_uri();
    did_open(&client, &uri, "main: function ( {\n}\n");
    let params = publish_for(&client, &uri);
    assert!(has_code(&params, "E001"), "{:?}", params.diagnostics);
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn unknown_name_is_e021() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = main_uri();
    did_open(
        &client,
        &uri,
        "main: function () nothing {\n    x: @bit8 = nope\n}\n",
    );
    let params = publish_for(&client, &uri);
    assert!(has_code(&params, "E021"), "{:?}", params.diagnostics);
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn use_after_move_links_the_move() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = main_uri();
    let text = "Handle: structure {\n    id: @bit8\n\n    @on_drop: function (self) nothing {\n    }\n}\n\nmain: function () nothing {\n    a: Handle = Handle { id = 0x1 }\n    b: Handle = a\n    c: Handle = a\n}\n";
    did_open(&client, &uri, text);
    let params = publish_for(&client, &uri);
    let error = params
        .diagnostics
        .iter()
        .find(|diagnostic| {
            matches!(&diagnostic.code, Some(NumberOrString::String(code)) if code == "E063")
        })
        .unwrap_or_else(|| panic!("{:?}", params.diagnostics));
    assert_eq!(error.range.start.line, 10, "{error:?}");
    let related = error.related_information.as_ref().expect("the move");
    assert_eq!(related[0].message, "moved here: `Handle` cannot be copied");
    assert_eq!(related[0].location.range.start.line, 9);
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn publishes_all_errors_past_the_cli_cap() {
    // The CLI caps error-level diagnostics at 10; the editor must show them all.
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = main_uri();
    let mut src = String::from("main: function () nothing {\n");
    for i in 1..=15 {
        let _ = writeln!(src, "    x{i}: @bit8 = nope{i}");
    }
    src.push_str("}\n");
    did_open(&client, &uri, &src);
    let params = publish_for(&client, &uri);
    let e021 = params
        .diagnostics
        .iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(code)) if code == "E021"))
        .count();
    assert_eq!(e021, 15, "{:?}", params.diagnostics);
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn publishes_all_project_errors_past_the_cli_cap() {
    let dir = std::env::temp_dir().join(format!("lilac-lsp-cap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let main = dir.join("main.lilac");
    let mut src = String::from("main: function () nothing {\n");
    for i in 1..=15 {
        let _ = writeln!(src, "    x{i}: @bit8 = nope{i}");
    }
    src.push_str("}\n");
    std::fs::write(&main, &src).expect("write");
    let root = Url::from_directory_path(&dir).expect("root uri");

    let (client, handle) = start_server();
    initialize(&client, Some(&root));
    let uri = Url::from_file_path(&main).expect("uri");
    did_open(&client, &uri, &src);
    let params = publish_for(&client, &uri);
    let e021 = params
        .diagnostics
        .iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(code)) if code == "E021"))
        .count();
    assert_eq!(e021, 15, "{:?}", params.diagnostics);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stale_diagnostics_are_cleared() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = main_uri();

    did_open(&client, &uri, "main: function () nothing {\n}\n");
    assert!(publish_for(&client, &uri).diagnostics.is_empty());

    did_change(&client, &uri, 2, "main: function ( {\n}\n");
    let broken = publish_for(&client, &uri);
    assert!(has_code(&broken, "E001"), "{:?}", broken.diagnostics);

    did_change(&client, &uri, 3, "main: function () nothing {\n}\n");
    let clean = publish_for(&client, &uri);
    assert!(clean.diagnostics.is_empty(), "{:?}", clean.diagnostics);

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn missing_using_is_e025() {
    let dir = std::env::temp_dir().join(format!("lilac-lsp-e025-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let main = dir.join("main.lilac");
    std::fs::write(&main, "using missing\n").expect("write");
    let root = Url::from_directory_path(&dir).expect("root uri");

    let (client, handle) = start_server();
    initialize(&client, Some(&root));
    let uri = Url::from_file_path(&main).expect("uri");
    did_open(&client, &uri, "using missing\n");
    let params = publish_for(&client, &uri);
    assert!(has_code(&params, "E025"), "{:?}", params.diagnostics);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unreadable_root_is_reported_on_the_open_document() {
    // A project-load error with no source span (the root folder is gone) must
    // still reach the editor, not vanish.
    let dir = std::env::temp_dir().join(format!("lilac-lsp-missing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = Url::from_directory_path(&dir).expect("root uri");
    let main = dir.join("main.lilac");

    let (client, handle) = start_server();
    initialize(&client, Some(&root));
    let uri = Url::from_file_path(&main).expect("uri");
    did_open(&client, &uri, "main: function () nothing {\n}\n");
    let params = publish_for(&client, &uri);
    assert!(has_code(&params, "E025"), "{:?}", params.diagnostics);

    shutdown(&client);
    handle.join().expect("server thread");
}

/// A fresh, empty temporary folder for one test.
fn temp_project(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lilac-lsp-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

fn notify(client: &Connection, method: &str, params: serde_json::Value) {
    client
        .sender
        .send(Message::Notification(Notification {
            method: method.to_owned(),
            params,
        }))
        .expect("notify");
}

#[test]
fn diagnostics_carry_the_document_version() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = main_uri();
    did_open(&client, &uri, "main: function () nothing {\n}\n");
    assert_eq!(publish_for(&client, &uri).version, Some(1));
    did_change(&client, &uri, 7, "main: function ( {\n}\n");
    assert_eq!(publish_for(&client, &uri).version, Some(7));
    shutdown(&client);
    handle.join().expect("server thread");
}

/// Requests arriving faster than the debounce window do not postpone the
/// compile: the diagnostics come before the last request's reply.
#[test]
fn requests_do_not_postpone_diagnostics() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = main_uri();
    did_open(&client, &uri, "main: function () nothing {\n}\n");
    let _ = publish_for(&client, &uri);
    did_change(&client, &uri, 2, "main: function ( {\n}\n");
    let mut last = RequestId::from(0);
    for id in 100..120 {
        last = RequestId::from(id);
        client
            .sender
            .send(Message::Request(Request {
                id: last.clone(),
                method: "textDocument/hover".to_owned(),
                params: serde_json::json!({
                    "textDocument": { "uri": uri.as_str() },
                    "position": { "line": 0, "character": 0 },
                }),
            }))
            .expect("hover");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut published = false;
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Notification(notification))
                if notification.method == "textDocument/publishDiagnostics" =>
            {
                published = true;
            }
            Ok(Message::Response(response)) if response.id == last => break,
            Ok(_) => {}
            Err(err) => panic!("no reply: {err}"),
        }
    }
    assert!(published, "diagnostics waited for the requests to stop");
    shutdown(&client);
    handle.join().expect("server thread");
}

/// A spanless project error goes to `lilac.toml` when there is one.
#[test]
fn spanless_errors_go_to_lilac_toml() {
    let dir = temp_project("anchor-config");
    std::fs::write(dir.join("lilac.toml"), "[project]\n").expect("write");
    let main = dir.join("main.lilac");
    std::fs::write(&main, "main: function () nothing {\n}\n").expect("write");

    let (client, handle) = start_server();
    initialize_with(
        &client,
        serde_json::json!({
            "capabilities": {},
            "workspaceFolders": [{
                "uri": Url::from_directory_path(&dir).expect("root").as_str(),
                "name": "ws",
            }],
            "initializationOptions": { "libraries": { "Gone": "gone" } },
        }),
    );
    did_open(
        &client,
        &Url::from_file_path(&main).expect("uri"),
        "main: function () nothing {\n}\n",
    );
    let config = Url::from_file_path(dir.join("lilac.toml")).expect("uri");
    let params = publish_for(&client, &config);
    assert!(has_code(&params, "E025"), "{:?}", params.diagnostics);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without `lilac.toml`, a spanless error goes to the root's `main.lilac`,
/// not to whichever document is open.
#[test]
fn spanless_errors_go_to_the_root_main() {
    let dir = temp_project("anchor-main");
    let main = dir.join("main.lilac");
    std::fs::write(&main, "main: function () nothing {\n}\n").expect("write");
    let other = dir.join("other.lilac");
    std::fs::write(&other, "other: function () nothing {\n}\n").expect("write");

    let (client, handle) = start_server();
    initialize_with(
        &client,
        serde_json::json!({
            "capabilities": {},
            "workspaceFolders": [{
                "uri": Url::from_directory_path(&dir).expect("root").as_str(),
                "name": "ws",
            }],
            "initializationOptions": { "libraries": { "Gone": "gone" } },
        }),
    );
    let other_uri = Url::from_file_path(&other).expect("uri");
    did_open(&client, &other_uri, "other: function () nothing {\n}\n");
    let params = publish_for(&client, &Url::from_file_path(&main).expect("uri"));
    assert!(has_code(&params, "E025"), "{:?}", params.diagnostics);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file created on disk is picked up when the client reports it.
#[test]
fn watched_file_changes_recompile() {
    let dir = temp_project("watched");
    let main = dir.join("main.lilac");
    let text = "main: function () nothing {\n    helper()\n}\n";
    std::fs::write(&main, text).expect("write");

    let (client, handle) = start_server();
    initialize(
        &client,
        Some(&Url::from_directory_path(&dir).expect("root")),
    );
    let uri = Url::from_file_path(&main).expect("uri");
    did_open(&client, &uri, text);
    let broken = publish_for(&client, &uri);
    assert!(has_code(&broken, "E021"), "{:?}", broken.diagnostics);

    let helper = dir.join("helper.lilac");
    std::fs::write(&helper, "helper: function () nothing {\n}\n").expect("write");
    notify(
        &client,
        "workspace/didChangeWatchedFiles",
        serde_json::json!({
            "changes": [{
                "uri": Url::from_file_path(&helper).expect("uri").as_str(),
                "type": 1,
            }],
        }),
    );
    let fixed = publish_for(&client, &uri);
    assert!(fixed.diagnostics.is_empty(), "{:?}", fixed.diagnostics);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A `lilac.libraries` change applies without a restart.
#[test]
fn library_setting_changes_apply() {
    let dir = temp_project("setting");
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).expect("mkdir");
    std::fs::write(lib.join("lib.lilac"), "helper: function () nothing {\n}\n").expect("write");
    let app = dir.join("app");
    std::fs::create_dir_all(&app).expect("mkdir");
    let main = app.join("main.lilac");
    let text = "using Lib\n\nmain: function () nothing {\n    helper()\n}\n";
    std::fs::write(&main, text).expect("write");

    let (client, handle) = start_server();
    initialize(
        &client,
        Some(&Url::from_directory_path(&app).expect("root")),
    );
    let uri = Url::from_file_path(&main).expect("uri");
    did_open(&client, &uri, text);
    let broken = publish_for(&client, &uri);
    assert!(has_code(&broken, "E025"), "{:?}", broken.diagnostics);

    notify(
        &client,
        "workspace/didChangeConfiguration",
        serde_json::json!({
            "settings": { "lilac": { "libraries": { "Lib": lib.to_string_lossy() } } },
        }),
    );
    let fixed = publish_for(&client, &uri);
    assert!(fixed.diagnostics.is_empty(), "{:?}", fixed.diagnostics);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A client that can register watchers is asked to watch `.lilac` files
/// and `lilac.toml`.
#[test]
fn file_watchers_are_registered() {
    let (client, handle) = start_server();
    initialize_with(
        &client,
        serde_json::json!({
            "capabilities": {
                "workspace": { "didChangeWatchedFiles": { "dynamicRegistration": true } },
            },
        }),
    );
    let request = loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Request(request)) => break request,
            Ok(_) => {}
            Err(err) => panic!("no registration: {err}"),
        }
    };
    assert_eq!(request.method, "client/registerCapability");
    let text = request.params.to_string();
    assert!(text.contains("workspace/didChangeWatchedFiles"), "{text}");
    assert!(text.contains("**/lilac.toml"), "{text}");
    shutdown(&client);
    handle.join().expect("server thread");
}

/// A folder nothing `using`s is still part of the project, so its files
/// resolve root-relative `using`s instead of compiling as their own folder.
#[test]
fn an_unused_folder_compiles_in_its_project() {
    let dir = temp_project("unused-folder");
    std::fs::write(dir.join("main.lilac"), "main: function () nothing {\n}\n").expect("write");
    std::fs::create_dir_all(dir.join("Util")).expect("mkdir");
    std::fs::write(
        dir.join("Util").join("util.lilac"),
        "helper: function () nothing {\n}\n",
    )
    .expect("write");
    std::fs::create_dir_all(dir.join("Geo")).expect("mkdir");
    let geo = dir.join("Geo").join("geo.lilac");
    let text = "using Util\n\narea: function () nothing {\n    helper()\n}\n";
    std::fs::write(&geo, text).expect("write");

    let (client, handle) = start_server();
    initialize(
        &client,
        Some(&Url::from_directory_path(&dir).expect("root")),
    );
    let uri = Url::from_file_path(&geo).expect("uri");
    did_open(&client, &uri, text);
    let params = publish_for(&client, &uri);
    assert!(params.diagnostics.is_empty(), "{:?}", params.diagnostics);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The first diagnostics published for `uri` with document version
/// `version`, waiting up to `limit` (earlier versions are skipped).
fn publish_of_version(
    client: &Connection,
    uri: &Url,
    version: i32,
    limit: Duration,
) -> PublishDiagnosticsParams {
    let deadline = std::time::Instant::now() + limit;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match client.receiver.recv_timeout(left) {
            Ok(Message::Notification(notification))
                if notification.method == "textDocument/publishDiagnostics" =>
            {
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(notification.params).expect("decode");
                if &params.uri == uri && params.version == Some(version) {
                    return params;
                }
            }
            Ok(_) => {}
            Err(err) => panic!("no diagnostics for version {version} of {uri}: {err}"),
        }
    }
}

/// Robustness plan problem 1: `$"a}` lexed forever, so the compile worker
/// hung and no later edit got diagnostics. It is an error, and the fix
/// that follows gets fresh diagnostics.
#[test]
fn an_edit_after_a_bad_fstring_gets_fresh_diagnostics() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = main_uri();
    did_open(
        &client,
        &uri,
        "main: function () nothing {\n    s: @bit8 = $\"a}\n}\n",
    );
    let params = publish_of_version(&client, &uri, 1, Duration::from_secs(10));
    assert!(has_code(&params, "E000"), "{:?}", params.diagnostics);
    did_change(&client, &uri, 2, "main: function () nothing {\n}\n");
    let params = publish_of_version(&client, &uri, 2, Duration::from_secs(10));
    assert!(params.diagnostics.is_empty(), "{:?}", params.diagnostics);
    shutdown(&client);
    handle.join().expect("server thread");
}

/// A valid program that takes seconds to check in a debug build: 15 levels
/// of generics, each instantiating the next at two new argument types, so
/// 32,767 instances (below typeck's instance budget), each body copying its
/// argument 40 times.
fn slow_program() -> String {
    let mut src = String::from(
        "A[T]: structure {\n    value: T\n}\n\nB[T]: structure {\n    value: T\n}\n\n",
    );
    for level in 1..15 {
        let _ = writeln!(src, "f{level}[T]: function (x0: T) nothing {{");
        for copy in 1..=40 {
            let _ = writeln!(src, "    x{copy}: T = x{}", copy - 1);
        }
        let next = level + 1;
        let _ = writeln!(src, "    f{next}[A[T]](A[T] {{ value = x40 }})");
        let _ = writeln!(src, "    f{next}[B[T]](B[T] {{ value = x40 }})\n}}\n");
    }
    src.push_str("f15[T]: function (x0: T) nothing {\n}\n\n");
    src.push_str("main: function () nothing {\n    f1[@bit32](0x1)\n}\n");
    src
}

/// A slow compile ([`slow_program`]) is cancelled by the next edit, whose
/// diagnostics come right after; the cancelled compile publishes nothing.
#[test]
fn an_edit_cancels_a_slow_compile() {
    let (client, handle) = start_server();
    initialize(&client, None);
    let uri = main_uri();
    did_open(&client, &uri, &slow_program());
    // Past the debounce, so the slow compile is running.
    std::thread::sleep(Duration::from_millis(600));
    did_change(
        &client,
        &uri,
        2,
        "main: function () nothing {\n    x: @bit8 = nope\n}\n",
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let params = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match client.receiver.recv_timeout(left) {
            Ok(Message::Notification(notification))
                if notification.method == "textDocument/publishDiagnostics" =>
            {
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(notification.params).expect("decode");
                if params.uri == uri {
                    break params;
                }
            }
            Ok(_) => {}
            Err(err) => panic!("the edit got no diagnostics within 5 s: {err}"),
        }
    };
    assert_eq!(params.version, Some(2), "{params:?}");
    assert!(has_code(&params, "E021"), "{:?}", params.diagnostics);
    shutdown(&client);
    handle.join().expect("server thread");
}

/// A library vendored inside the workspace and mounted by `lilac.toml`
/// (`Util = "vendor/Util"`) loads once, under its mount name: no file,
/// `lilac.toml` included, gets an `E025` or any other diagnostic.
#[test]
fn a_vendored_library_from_lilac_toml_checks_clean() {
    let dir = temp_project("vendored");
    let util = dir.join("vendor").join("Util");
    std::fs::create_dir_all(&util).expect("mkdir");
    std::fs::write(
        dir.join("lilac.toml"),
        "[libraries]\nUtil = \"vendor/Util\"\n",
    )
    .expect("write");
    std::fs::write(
        util.join("util.lilac"),
        "helper: function () nothing {\n}\n",
    )
    .expect("write");
    let main = dir.join("main.lilac");
    let text = "using Util\n\nmain: function () nothing {\n    helper()\n}\n";
    std::fs::write(&main, text).expect("write");

    let (client, handle) = start_server();
    initialize(
        &client,
        Some(&Url::from_directory_path(&dir).expect("root")),
    );
    let main_uri = Url::from_file_path(&main).expect("uri");
    did_open(&client, &main_uri, text);
    // Every publish of this compile is empty (a spanless error would go to
    // `lilac.toml`), up to `main.lilac`'s and for a second after it.
    let mut seen_main = false;
    loop {
        let message = match client
            .receiver
            .recv_timeout(Duration::from_secs(if seen_main { 1 } else { 5 }))
        {
            Ok(message) => message,
            Err(_) if seen_main => break,
            Err(err) => panic!("no diagnostics for {main_uri}: {err}"),
        };
        if let Message::Notification(notification) = message {
            if notification.method == "textDocument/publishDiagnostics" {
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(notification.params).expect("decode");
                assert!(
                    params.diagnostics.is_empty(),
                    "{}: {:?}",
                    params.uri,
                    params.diagnostics
                );
                seen_main |= params.uri == main_uri;
            }
        }
    }

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}
