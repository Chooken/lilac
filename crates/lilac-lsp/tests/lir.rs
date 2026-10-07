use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    DidChangeTextDocumentParams, DidOpenTextDocumentParams, PublishDiagnosticsParams,
    TextDocumentContentChangeEvent, TextDocumentItem, Url, VersionedTextDocumentIdentifier,
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

fn did_change(client: &Connection, uri: &Url, text: &str) {
    let params = DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version: 2,
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
    let text = "main: function () nothing {\n    x: @bit_native = @i_add(0x1, 0x2)\n}\n";
    did_open(&client, &uri, text);
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

    // The final module folds `@i_add(0x1, 0x2)` into a constant, so the live
    // `i.add` instruction only survives in the `lower` stage. Bind that
    // instruction's physical line to the exact byte range of its source
    // expression: an off-by-one line map or a swapped span field would
    // otherwise pass the suite unnoticed.
    let expr = "@i_add(0x1, 0x2)";
    let lo = text.find(expr).expect("expression offset") as u64;
    let hi = lo + expr.len() as u64;
    let lower = lir_request(&client, &uri, "lower");
    let lower_text = lower["text"].as_str().expect("lower text");
    let lower_lines: Vec<&str> = lower_text.lines().collect();
    let arith_line = lower_lines
        .iter()
        .position(|line| line.contains("i.add"))
        .expect("i.add line");
    let arith_row = lower["spans"]
        .as_array()
        .expect("lower spans")
        .iter()
        .find(|row| row[0].as_u64() == Some(arith_line as u64))
        .expect("span row for the i.add line");
    assert_eq!(
        arith_row,
        &serde_json::json!([arith_line as u64, 0, lo, hi]),
        "line {arith_line} `{}` must carry its expression's span",
        lower_lines[arith_line]
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

fn next_invalidation(client: &Connection) {
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Notification(n)) if n.method == "lilac/lirInvalidated" => return,
            Ok(_) => {}
            Err(err) => panic!("no lilac/lirInvalidated: {err}"),
        }
    }
}

#[test]
fn an_edit_sends_lir_invalidated() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    did_open(&client, &uri, "main: function () nothing {\n}\n");
    let _ = publish_for(&client, &uri);
    // Drain the invalidation from the initial publish; otherwise the
    // assertion below would be satisfied by the leftover message even if
    // `did_change` were ignored entirely.
    next_invalidation(&client);

    did_change(
        &client,
        &uri,
        "main: function () nothing {\n    x: @bit32 = 0x1\n}\n",
    );
    next_invalidation(&client);

    shutdown(&client);
    handle.join().expect("server thread");
}
