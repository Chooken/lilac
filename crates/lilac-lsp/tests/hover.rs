use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    DidOpenTextDocumentParams, Hover, PublishDiagnosticsParams, TextDocumentItem, Url,
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

fn hover_at(client: &Connection, uri: &Url, line: u32, character: u32) -> Option<Hover> {
    let id = RequestId::from(50);
    let params = serde_json::json!({
        "textDocument": { "uri": uri.as_str() },
        "position": { "line": line, "character": character },
    });
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "textDocument/hover".to_owned(),
            params,
        }))
        .expect("hover");
    let response = read_response(client, &id);
    match response.response_result {
        Ok(value) if !value.is_null() => serde_json::from_value(value).ok(),
        _ => None,
    }
}

#[test]
fn hover_on_a_call_shows_the_signature() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "foo: function (value: @bit8) result: @bit32 {\n    result = 0x1\n    return\n}\n\nmain: function () nothing {\n    x: @bit32 = foo(0x1)\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let line = 6; // "    x: @bit32 = foo(0x1)"
    let character = 17; // inside "foo"
    let hover = hover_at(&client, &uri, line, character).expect("hover present");
    let contents = format!("{:?}", hover.contents);
    assert!(contents.contains("foo"), "{contents}");
    assert!(contents.contains("@bit32"), "{contents}");

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn hover_on_whitespace_is_null() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "main: function () nothing {\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    assert!(hover_at(&client, &uri, 1, 0).is_none());

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn hover_survives_a_syntax_error() {
    // A syntax error in one item must not clear the index: the valid `foo`
    // declaration still backs the signature hover at its call site.
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "broken: function ( {\n}\nfoo: function (value: @bit8) result: @bit32 {\n    result = 0x1\n    return\n}\nmain: function () nothing {\n    x: @bit32 = foo(0x1)\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let line = 7; // "    x: @bit32 = foo(0x1)"
    let character = 17; // inside "foo"
    let hover = hover_at(&client, &uri, line, character).expect("hover present");
    let contents = format!("{:?}", hover.contents);
    assert!(contents.contains("foo"), "{contents}");
    assert!(contents.contains("@bit32"), "{contents}");

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn hover_on_a_const_use_shows_its_type() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "pi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let line = 2; // "    x: @bit32 = pi"
    let character = 17; // inside "pi"
    let hover = hover_at(&client, &uri, line, character).expect("hover present");
    let contents = format!("{:?}", hover.contents);
    assert!(contents.contains("pi"), "{contents}");
    assert!(contents.contains("@bit32"), "{contents}");

    shutdown(&client);
    handle.join().expect("server thread");
}
