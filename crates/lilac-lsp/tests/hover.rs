use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    DidOpenTextDocumentParams, Hover, HoverContents, PublishDiagnosticsParams, TextDocumentItem,
    Url,
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

/// A file nested past the parser's limit gets its one `E001`, and the
/// server goes on answering for the rest of it.
#[test]
fn hover_survives_deep_nesting() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let n = 1000;
    let text = format!(
        "deep: function () nothing {{\n    x: @bit32 = {}0x1{}\n}}\nfoo: function (value: @bit8) result: @bit32 {{\n    result = 0x1\n    return\n}}\nmain: function () nothing {{\n    x: @bit32 = foo(0x1)\n}}\n",
        "(".repeat(n),
        ")".repeat(n)
    );
    did_open(&client, &uri, &text);
    let published = publish_for(&client, &uri);
    let messages: Vec<&str> = published
        .diagnostics
        .iter()
        .filter(|d| d.severity == Some(lsp_types::DiagnosticSeverity::ERROR))
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert!(messages[0].contains("nested too deeply"), "{messages:?}");

    let line = 8; // "    x: @bit32 = foo(0x1)"
    let character = 17; // inside "foo"
    let hover = hover_at(&client, &uri, line, character).expect("hover present");
    assert!(format!("{:?}", hover.contents).contains("foo"));

    shutdown(&client);
    handle.join().expect("server thread");
}

const DOCUMENTED: &str = "\
/// Two of a kind.
Pair[T]: object {
    first: T
    /// The other one.
    second: T
    /// The first of the two.
    get: function (self) value: T {
        // not part of any hover
        value = self.first
    }
}
/// How far to go.
limit: @bit8 => @i_add_8(0x10, 0x2)
magnitude: extern \"abs\" function (value: @bit32) result: @bit32
main: function () nothing {
    pair: Pair[@bit8] = Pair[@bit8] {
        first = limit
        second = 0x1
    }
    x: @bit8 = pair.get()
    y: @bit32 = magnitude(0x1)
    z: @bit8 = pair.second
}
";

fn hover_markdown(client: &Connection, uri: &Url, needle: &str, into: u32) -> String {
    let byte = DOCUMENTED.rfind(needle).expect("needle");
    let before = &DOCUMENTED[..byte];
    let line = u32::try_from(before.matches('\n').count()).expect("line");
    let column = byte - before.rfind('\n').map_or(0, |newline| newline + 1);
    let column = u32::try_from(column).expect("column") + into;
    let hover = hover_at(client, uri, line, column).expect("hover");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("markup: {hover:?}");
    };
    markup.value
}

/// A hover is the declaration as Lilac writes it, built from the syntax
/// tree (no comment or layout of the source), then its doc.
#[test]
fn hovers_render_declarations_with_their_docs() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    did_open(&client, &uri, DOCUMENTED);
    let _ = publish_for(&client, &uri);

    let get = hover_markdown(&client, &uri, "get()", 0);
    assert_eq!(
        get,
        "```lilac\nPair[T].get: function (self) value: T\n```\n\n---\n\nThe first of the two."
    );
    let pair = hover_markdown(&client, &uri, "Pair[@bit8] {", 0);
    assert_eq!(
        pair,
        "```lilac\nPair[T]: object {\n    first: T\n    second: T\n}\n```\n\n---\n\nTwo of a kind."
    );
    let second = hover_markdown(&client, &uri, "second\n", 0);
    assert_eq!(
        second,
        "```lilac\nsecond: T\n```\nin `Pair[T]`\n\n---\n\nThe other one."
    );
    let limit = hover_markdown(&client, &uri, "limit\n", 0);
    assert_eq!(
        limit,
        "```lilac\nlimit: @bit8 => @i_add_8(0x10, 0x2)\n```\n\n---\n\nHow far to go."
    );
    let magnitude = hover_markdown(&client, &uri, "magnitude(0x1)", 0);
    assert_eq!(
        magnitude,
        "```lilac\nmagnitude: extern \"abs\" function (value: @bit32) result: @bit32\n```"
    );

    shutdown(&client);
    handle.join().expect("server thread");
}

/// A member constant renders after its type, whatever the type's
/// arguments, on a use and on its declaration, with its doc.
#[test]
fn hover_on_a_member_constant_names_its_type() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "\
Gen[T]: object {
    /// How many there are.
    SIZE: @bit8 => 0x4
    item: T
}
main: function () nothing {
    x: @bit8 = Gen[@bit8].SIZE
}
";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let expected = "```lilac\nGen.SIZE: @bit8 => 0x4\n```\n\n---\n\nHow many there are.";
    for (line, character) in [(6, 27), (2, 5)] {
        let hover = hover_at(&client, &uri, line, character).expect("hover present");
        let HoverContents::Markup(markup) = hover.contents else {
            panic!("markup: {hover:?}");
        };
        assert_eq!(markup.value, expected);
    }

    shutdown(&client);
    handle.join().expect("server thread");
}
