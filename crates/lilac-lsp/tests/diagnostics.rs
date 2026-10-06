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
