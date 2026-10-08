use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    DidOpenTextDocumentParams, PublishDiagnosticsParams, SemanticTokens, TextDocumentItem, Url,
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

/// Initializes and returns the server's `initialize` result.
fn initialize(client: &Connection) -> serde_json::Value {
    let id = RequestId::from(1);
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "initialize".to_owned(),
            params: serde_json::json!({ "capabilities": {} }),
        }))
        .expect("initialize");
    let result = read_response(client, &id)
        .response_result
        .expect("initialize result");
    client
        .sender
        .send(Message::Notification(Notification {
            method: "initialized".to_owned(),
            params: serde_json::Value::Null,
        }))
        .expect("initialized");
    result
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

fn semantic_tokens_at(client: &Connection, uri: &Url) -> SemanticTokens {
    let id = RequestId::from(70);
    let params = serde_json::json!({ "textDocument": { "uri": uri.as_str() } });
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "textDocument/semanticTokens/full".to_owned(),
            params,
        }))
        .expect("semanticTokens/full");
    let value = read_response(client, &id)
        .response_result
        .expect("semantic tokens result");
    serde_json::from_value(value).expect("decode tokens")
}

/// One decoded token with absolute `(line, character)`.
#[derive(Debug, Clone, Copy)]
struct Decoded {
    line: u32,
    character: u32,
    length: u32,
    token_type: u32,
    modifiers: u32,
}

/// Absolute-decodes the delta-encoded `data`.
fn decode(tokens: &SemanticTokens) -> Vec<Decoded> {
    let mut out = Vec::new();
    let mut line = 0u32;
    let mut character = 0u32;
    for token in &tokens.data {
        if token.delta_line > 0 {
            line += token.delta_line;
            character = token.delta_start;
        } else {
            character += token.delta_start;
        }
        out.push(Decoded {
            line,
            character,
            length: token.length,
            token_type: token.token_type,
            modifiers: token.token_modifiers_bitset,
        });
    }
    out
}

/// 0-based line and UTF-16 column for a byte offset.
fn position(text: &str, byte: usize) -> (u32, u32) {
    let before = &text[..byte];
    let line = before.matches('\n').count();
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..byte]
        .chars()
        .map(char::len_utf16)
        .sum::<usize>();
    (
        u32::try_from(line).unwrap_or(u32::MAX),
        u32::try_from(character).unwrap_or(u32::MAX),
    )
}

/// The token that starts at the `offset`-th byte past the first `key`.
fn token_after(tokens: &[Decoded], text: &str, key: &str, offset: usize) -> Decoded {
    let byte = text.find(key).expect("key") + offset;
    let (line, character) = position(text, byte);
    tokens
        .iter()
        .copied()
        .find(|token| token.line == line && token.character == character)
        .unwrap_or_else(|| panic!("no token at {line}:{character} ({key:?}+{offset}); {tokens:?}"))
}

const FUNCTION: u32 = 0;
const TYPE: u32 = 1;
const PROPERTY: u32 = 3;
const VARIABLE: u32 = 5;
const PARAMETER: u32 = 6;
const DEFAULT_LIBRARY: u32 = 1 << 0;

#[test]
fn initialize_advertises_the_semantic_tokens_legend() {
    let (client, handle) = start_server();
    let result = initialize(&client);

    let provider = &result["capabilities"]["semanticTokensProvider"];
    let types: Vec<&str> = provider["legend"]["tokenTypes"]
        .as_array()
        .expect("tokenTypes")
        .iter()
        .map(|value| value.as_str().expect("string"))
        .collect();
    assert_eq!(
        types,
        vec![
            "function",
            "type",
            "enum",
            "property",
            "enumMember",
            "variable",
            "parameter"
        ]
    );
    let modifiers: Vec<&str> = provider["legend"]["tokenModifiers"]
        .as_array()
        .expect("tokenModifiers")
        .iter()
        .map(|value| value.as_str().expect("string"))
        .collect();
    assert_eq!(modifiers, vec!["defaultLibrary"]);
    assert_eq!(provider["full"], serde_json::json!(true));
    assert_eq!(provider["range"], serde_json::json!(false));

    shutdown(&client);
    handle.join().expect("server thread");
}

const FIXTURE: &str = "Obj: object {\n    member: @bit8\n}\npi: @bit32 = 0x2A\nfoo: function (p: @bit32) result: @bit32 {\n    result = p\n    return\n}\nmain: function () nothing {\n    x: @bit32 = pi\n    v: Obj = Obj {\n        member = 0x1\n    }\n    y: @bit8 = v.member\n    z: @bit32 = foo(x)\n    w: @bit32 = @i_add_32(z, 0x1)\n}\n";

#[test]
fn full_returns_tokens_for_every_kind() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    did_open(&client, &uri, FIXTURE);
    let _ = publish_for(&client, &uri);

    let decoded = decode(&semantic_tokens_at(&client, &uri));

    // Function declaration and call.
    let decl = token_after(&decoded, FIXTURE, "foo: function", 0);
    assert_eq!(
        (decl.token_type, decl.modifiers, decl.length),
        (FUNCTION, 0, 3)
    );
    let call = token_after(&decoded, FIXTURE, "= foo(x)", 2);
    assert_eq!((call.token_type, call.modifiers), (FUNCTION, 0));

    // Object type reference and declaration name (both `type`).
    let ty = token_after(&decoded, FIXTURE, "v: Obj =", 3);
    assert_eq!((ty.token_type, ty.modifiers, ty.length), (TYPE, 0, 3));
    let obj_decl = token_after(&decoded, FIXTURE, "Obj: object", 0);
    assert_eq!((obj_decl.token_type, obj_decl.modifiers), (TYPE, 0));

    // Field declaration and member access.
    let field = token_after(&decoded, FIXTURE, "member: @bit8", 0);
    assert_eq!(
        (field.token_type, field.modifiers, field.length),
        (PROPERTY, 0, 6)
    );
    let member = token_after(&decoded, FIXTURE, "v.member", 2);
    assert_eq!((member.token_type, member.modifiers), (PROPERTY, 0));

    // Const declaration and use.
    let pi = token_after(&decoded, FIXTURE, "pi: @bit32", 0);
    assert_eq!((pi.token_type, pi.modifiers), (VARIABLE, 0));
    let const_use = token_after(&decoded, FIXTURE, "= pi\n", 2);
    assert_eq!((const_use.token_type, const_use.modifiers), (VARIABLE, 0));

    // Local declaration and use.
    let x = token_after(&decoded, FIXTURE, "x: @bit32 = pi", 0);
    assert_eq!((x.token_type, x.modifiers), (VARIABLE, 0));
    let v = token_after(&decoded, FIXTURE, "= v.member", 2);
    assert_eq!((v.token_type, v.modifiers), (VARIABLE, 0));

    // Parameter declaration and use.
    let p = token_after(&decoded, FIXTURE, "(p: @bit32)", 1);
    assert_eq!((p.token_type, p.modifiers), (PARAMETER, 0));
    let param_use = token_after(&decoded, FIXTURE, "result = p\n", 9);
    assert_eq!((param_use.token_type, param_use.modifiers), (PARAMETER, 0));

    // Builtin type name carries `defaultLibrary`.
    let builtin_type_token = token_after(&decoded, FIXTURE, "y: @bit8", 3);
    assert_eq!(
        (
            builtin_type_token.token_type,
            builtin_type_token.modifiers,
            builtin_type_token.length
        ),
        (TYPE, DEFAULT_LIBRARY, 5)
    );

    // Builtin call name is a `function` with `defaultLibrary`.
    let builtin_call_token = token_after(&decoded, FIXTURE, "@i_add_32", 0);
    assert_eq!(
        (
            builtin_call_token.token_type,
            builtin_call_token.modifiers,
            builtin_call_token.length
        ),
        (FUNCTION, DEFAULT_LIBRARY, 9)
    );

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn ref_type_colors_only_the_name() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "Obj: object {\n    member: @bit8\n}\ntake: function (o: ref Obj) nothing {\n}\nmain: function () nothing {\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let decoded = decode(&semantic_tokens_at(&client, &uri));
    let ty = token_after(&decoded, text, "ref Obj", 4);
    assert_eq!((ty.token_type, ty.length), (TYPE, 3));
    // Nothing is emitted for the `ref` keyword itself.
    let (ref_line, ref_col) = position(text, text.find("ref Obj").expect("ref"));
    assert!(decoded
        .iter()
        .all(|token| !(token.line == ref_line && token.character == ref_col)));

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn syntax_error_still_returns_surviving_tokens() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "broken: function ( {\n}\nfoo: function () nothing {\n}\nmain: function () nothing {\n    foo()\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let decoded = decode(&semantic_tokens_at(&client, &uri));
    assert!(!decoded.is_empty(), "tokens vanished after a syntax error");
    let foo_decl = token_after(&decoded, text, "foo: function", 0);
    assert_eq!(foo_decl.token_type, FUNCTION);
    let foo_call = token_after(&decoded, text, "foo()", 0);
    assert_eq!(foo_call.token_type, FUNCTION);

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn clean_file_with_nothing_to_color_is_empty() {
    let (client, handle) = start_server();
    initialize(&client);
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    did_open(&client, &uri, "// only a comment\n");
    let _ = publish_for(&client, &uri);

    let tokens = semantic_tokens_at(&client, &uri);
    assert!(tokens.data.is_empty(), "{:?}", tokens.data);

    shutdown(&client);
    handle.join().expect("server thread");
}
