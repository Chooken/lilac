//! Phase 7 of the f-string plan: holes, their hooks, and `E071`'s fix.

use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, ResponseError};
use lsp_types::{
    CodeActionOrCommand, CompletionItem, CompletionResponse, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, GotoDefinitionResponse, Hover, HoverContents, Location,
    PublishDiagnosticsParams, TextDocumentContentChangeEvent, TextDocumentItem, Url,
    VersionedTextDocumentIdentifier,
};

const PROGRAM: &str = "\
Out: object {}
@format: function (value: ref @bit8, out: ref Out) nothing {
}
Point: object {
    x: @bit8
    @format: function (self, out: ref Out) nothing {
    }
}
Bare: object {
    y: @bit8
}
consume: function (text: @format_args(Out)) nothing {
}
main: function () nothing {
    count: @bit8 = 0x1
    point: Point = Point { x = count }
    consume($\"n {count} at { point }\")
    // EDIT
}
";
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

fn initialize(client: &Connection, params: serde_json::Value) -> serde_json::Value {
    let id = RequestId::from(1);
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "initialize".to_owned(),
            params,
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

fn notify(client: &Connection, method: &str, params: serde_json::Value) {
    client
        .sender
        .send(Message::Notification(Notification {
            method: method.to_owned(),
            params,
        }))
        .expect("notify");
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
    notify(
        client,
        "textDocument/didOpen",
        serde_json::to_value(params).expect("serialize"),
    );
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
    notify(
        client,
        "textDocument/didChange",
        serde_json::to_value(params).expect("serialize"),
    );
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

fn request(
    client: &Connection,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, ResponseError> {
    let id = RequestId::from(format!("{method}-{params}"));
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: method.to_owned(),
            params,
        }))
        .expect("request");
    read_response(client, &id).response_result
}

/// The `(line, character)` just after the last `needle` in `text`.
fn after(text: &str, needle: &str) -> (u32, u32) {
    let byte = text.rfind(needle).expect("needle") + needle.len();
    let before = &text[..byte];
    let line = before.matches('\n').count();
    let character = before.len() - before.rfind('\n').map_or(0, |newline| newline + 1);
    (
        u32::try_from(line).expect("line"),
        u32::try_from(character).expect("character"),
    )
}

fn position(uri: &Url, (line, character): (u32, u32)) -> serde_json::Value {
    serde_json::json!({
        "textDocument": { "uri": uri.as_str() },
        "position": { "line": line, "character": character },
    })
}

/// The `(line, character)` of the start of the last `needle` in `text`.
fn at(text: &str, needle: &str) -> (u32, u32) {
    let (line, character) = after(text, needle);
    (
        line,
        character - u32::try_from(needle.len()).expect("length"),
    )
}

/// A server with `PROGRAM` open and checked as a single file.
fn open_program() -> (Connection, std::thread::JoinHandle<()>, Url) {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    did_open(&client, &uri, PROGRAM);
    let published = publish_for(&client, &uri);
    assert!(
        published
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Some(lsp_types::DiagnosticSeverity::ERROR)),
        "{:?}",
        published.diagnostics
    );
    (client, handle, uri)
}

fn hover_text(client: &Connection, uri: &Url, at: (u32, u32)) -> String {
    let result = request(client, "textDocument/hover", position(uri, at)).expect("hover");
    let hover: Hover = serde_json::from_value(result).expect("a hover");
    match hover.contents {
        HoverContents::Markup(markup) => markup.value,
        other => panic!("markup: {other:?}"),
    }
}

fn definition(client: &Connection, uri: &Url, at: (u32, u32)) -> Location {
    let result = request(client, "textDocument/definition", position(uri, at)).expect("definition");
    match serde_json::from_value(result).expect("a definition") {
        GotoDefinitionResponse::Scalar(location) => location,
        other => panic!("one location: {other:?}"),
    }
}

#[test]
fn a_hole_hovers_with_its_type_and_hook() {
    let (client, handle, uri) = open_program();
    // On the braces: the type and the hook's signature.
    let text = hover_text(&client, &uri, at(PROGRAM, "{ point }"));
    assert!(text.contains("```lilac\nPoint\n```"), "{text}");
    assert!(text.contains("formatted by"), "{text}");
    assert!(
        text.contains("function Point.@format(ref Point, ref Out) nothing"),
        "{text}"
    );
    // On the value: its type, then the hook.
    let text = hover_text(&client, &uri, after(PROGRAM, "{cou"));
    assert!(text.starts_with("```lilac\n@bit8\n```"), "{text}");
    assert!(text.contains("formatted by `@format`"), "{text}");
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn a_hole_navigates_to_its_hook() {
    let (client, handle, uri) = open_program();
    // From the braces to the hook's declaration.
    let member = definition(&client, &uri, after(PROGRAM, "{ point "));
    assert_eq!(member.range.start, lsp_types::Position::new(5, 4));
    let top = definition(&client, &uri, at(PROGRAM, "{count}"));
    assert_eq!(top.range.start, lsp_types::Position::new(1, 0));

    // The hook's references list the hole.
    let result = request(
        &client,
        "textDocument/references",
        serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "position": { "line": 5, "character": 5 },
            "context": { "includeDeclaration": false },
        }),
    )
    .expect("references");
    let locations: Vec<Location> = serde_json::from_value(result).expect("locations");
    let (line, character) = at(PROGRAM, "{ point }");
    assert_eq!(locations.len(), 1, "{locations:?}");
    assert_eq!(
        locations[0].range.start,
        lsp_types::Position::new(line, character)
    );
    assert_eq!(
        locations[0].range.end,
        lsp_types::Position::new(line, character + 9)
    );

    // A hook's name is fixed.
    let refused = request(
        &client,
        "textDocument/prepareRename",
        position(&uri, at(PROGRAM, "{ point }")),
    );
    assert!(refused.is_err(), "{refused:?}");
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn a_missing_format_names_the_hook_without_a_fix() {
    let (client, handle, uri) = open_program();
    let text = PROGRAM.replace(
        "    // EDIT",
        "    bare: Bare = Bare { y = count }\n    consume($\"{bare}\")",
    );
    did_change(&client, &uri, 2, &text);
    let published = loop {
        let published = publish_for(&client, &uri);
        if published.version == Some(2) {
            break published;
        }
    };
    let errors: Vec<_> = published
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Some(lsp_types::DiagnosticSeverity::ERROR))
        .cloned()
        .collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0]
            .message
            .contains("add `@format: function (self, out: ref Out) nothing` to `Bare`"),
        "{errors:?}"
    );

    // Inserting a hook needs layout the compiler does not keep, so no fix.
    let result = request(
        &client,
        "textDocument/codeAction",
        serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "range": errors[0].range,
            "context": { "diagnostics": errors },
        }),
    )
    .expect("code actions");
    let actions: Vec<CodeActionOrCommand> = serde_json::from_value(result).expect("actions");
    assert!(actions.is_empty(), "{actions:?}");
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn format_args_completes_and_hovers() {
    let (client, handle, uri) = open_program();
    let text = PROGRAM.replace("    // EDIT", "    later: @format_a");
    did_change(&client, &uri, 2, &text);
    let result = request(
        &client,
        "textDocument/completion",
        position(&uri, after(&text, "@format_a")),
    )
    .expect("completion");
    let items: Vec<CompletionItem> =
        match serde_json::from_value::<Option<CompletionResponse>>(result).expect("completions") {
            Some(CompletionResponse::Array(items)) => items,
            Some(CompletionResponse::List(list)) => list.items,
            None => Vec::new(),
        };
    let detail = |label: &str| {
        items
            .iter()
            .find(|item| item.label == label)
            .and_then(|item| item.detail.clone())
            .unwrap_or_else(|| panic!("{label}"))
    };
    assert!(detail("@format_args").contains("@format_args(W)"));
    assert!(detail("@array").contains("@array(T, N)"));
    assert_eq!(
        detail("@format_args_hole"),
        "@format_args_hole(@format_args(W), index: @bit_native, out: ref W) nothing"
    );

    // The last compile is of `PROGRAM`, whose `consume` names the type.
    let hover = hover_text(&client, &uri, after(PROGRAM, "@format_ar"));
    assert!(
        hover.starts_with("```lilac\n@format_args(W)\n```"),
        "{hover}"
    );
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn a_generic_hole_has_a_hook_per_instance() {
    let (client, handle, uri) = open_program();
    let text = format!(
        "{PROGRAM}show[T]: function (value: ref T) nothing {{\n    consume($\"[{{value}}]\")\n}}\n\
         both: function (p: Point) nothing {{\n    n: @bit8 = 0x2\n    show[@bit8](ref n)\n    show[Point](ref p)\n}}\n"
    );
    did_change(&client, &uri, 2, &text);
    loop {
        if publish_for(&client, &uri).version == Some(2) {
            break;
        }
    }
    let result = request(
        &client,
        "textDocument/definition",
        position(&uri, at(&text, "{value}")),
    )
    .expect("definition");
    let GotoDefinitionResponse::Array(locations) =
        serde_json::from_value(result).expect("a definition")
    else {
        panic!("two hooks");
    };
    let mut lines: Vec<u32> = locations.iter().map(|l| l.range.start.line).collect();
    lines.sort_unstable();
    assert_eq!(lines, vec![1, 5]);
    let hover = hover_text(&client, &uri, at(&text, "{value}"));
    assert!(hover.contains("```lilac\n@bit8\n```"), "{hover}");
    assert!(hover.contains("```lilac\nPoint\n```"), "{hover}");
    shutdown(&client);
    handle.join().expect("server thread");
}
