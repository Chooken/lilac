use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    InitializeParams, InitializeResult, TextDocumentSyncCapability, TextDocumentSyncKind,
};

fn read_response(client: &Connection, id: &RequestId) -> lsp_server::Response {
    loop {
        match client.receiver.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Message::Response(response)) if &response.id == id => return response,
            Ok(_) => continue,
            Err(err) => panic!("no response for {id:?}: {err}"),
        }
    }
}

#[test]
fn initializes_shuts_down_and_exits() {
    let (server, client) = Connection::memory();
    let handle = std::thread::spawn(move || lilac_lsp::run(server).expect("server runs"));

    let params = serde_json::from_value::<InitializeParams>(
        serde_json::json!({ "capabilities": {} }),
    )
    .expect("params");
    let init_id = RequestId::from(1);
    client
        .sender
        .send(Message::Request(Request {
            id: init_id.clone(),
            method: "initialize".to_owned(),
            params: serde_json::to_value(params).expect("serialize"),
        }))
        .expect("send initialize");

    let response = read_response(&client, &init_id);
    let result: InitializeResult =
        serde_json::from_value(response.response_result.expect("initialize result"))
            .expect("decode");
    assert_eq!(
        result.capabilities.text_document_sync,
        Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL))
    );
    assert_eq!(result.server_info.expect("server info").name, "lilac-lsp");

    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("send initialized");

    let shutdown_id = RequestId::from(2);
    client
        .sender
        .send(Message::Request(Request {
            id: shutdown_id.clone(),
            method: "shutdown".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("send shutdown");
    assert!(read_response(&client, &shutdown_id).response_result.is_ok());

    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("send exit");
    handle.join().expect("server thread joins");
}

#[test]
fn exit_without_shutdown_is_an_error() {
    let (server, client) = Connection::memory();
    let handle = std::thread::spawn(move || lilac_lsp::run(server));

    let init_id = RequestId::from(1);
    client
        .sender
        .send(Message::Request(Request {
            id: init_id.clone(),
            method: "initialize".to_owned(),
            params: serde_json::json!({ "capabilities": {} }),
        }))
        .expect("send initialize");
    let _ = read_response(&client, &init_id);

    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("send initialized");

    client
        .sender
        .send(Message::Notification(Notification {
            method: "exit".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("send exit");
    assert!(handle.join().expect("server thread joins").is_err());
}
