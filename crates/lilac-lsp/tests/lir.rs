use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{DidOpenTextDocumentParams, PublishDiagnosticsParams, TextDocumentItem, Url};

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

fn lir_request(client: &Connection, uri: &Url, stage: &str) -> serde_json::Value {
    let id = RequestId::from(60);
    let params = serde_json::json!({ "uri": uri.as_str(), "stage": stage });
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "lilac/lir".to_owned(),
            params,
        }))
        .expect("lilac/lir");
    read_response(client, &id).response_result.expect("result")
}

#[test]
fn lir_request_returns_the_final_module() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    did_open(
        &client,
        &uri,
        "main: function () nothing {\n    x: @bit_native = @i_add(0x1, 0x2)\n}\n",
    );
    let _ = publish_for(&client, &uri);

    let value = lir_request(&client, &uri, "final");
    assert_eq!(value["stage"], "final");
    assert!(value["text"]
        .as_str()
        .expect("text")
        .contains("function @main"));
    let spans = value["spans"].as_array().expect("spans");
    assert!(!spans.is_empty());
    assert!(
        spans
            .iter()
            .all(|row| row[1].as_u64() != Some(u64::from(u32::MAX))),
        "dummy (compiler-generated runtime) spans must be omitted"
    );

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn lir_request_is_null_for_an_unreached_stage() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "boom: function () nothing {\n    @compile_error(\"live\")\n}\n\nmain: function () nothing {\n    boom()\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    assert!(lir_request(&client, &uri, "final").is_null());
    assert!(!lir_request(&client, &uri, "lower").is_null());

    shutdown(&client);
    handle.join().expect("server thread");
}
