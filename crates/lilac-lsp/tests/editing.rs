//! Phase 4 editing assistance: completion, signature help and quick fixes.

use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, ResponseError};
use lsp_types::{
    CodeActionOrCommand, CompletionItem, CompletionResponse, CompletionTextEdit,
    DiagnosticSeverity, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
    PublishDiagnosticsParams, SignatureHelp, TextDocumentContentChangeEvent, TextDocumentItem, Url,
    VersionedTextDocumentIdentifier,
};

const PROGRAM: &str = "\
Point: object {
    x: @bit8
    y: @bit8
    sum: function (self) total: @bit8 {
        total = @i_add_8(self.x, self.y)
    }
}
Shape: enum {
    Dot
    Line: @bit8
}
limit: @bit8 = 0x10
add: function (a: @bit8, b: @bit8) total: @bit8 {
    total = @i_add_8(a, b)
}
main: function () nothing {
    first: @bit8 = add(0x1, limit)
    {
        inner: @bit8 = first
    }
    point: Point = Point {
        x = first
        y = first
    }
    // EDIT
    later: @bit8 = first
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
            .all(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::HINT)),
        "{:?}",
        published.diagnostics
    );
    (client, handle, uri)
}

/// Replaces the `// EDIT` line with `edit` and asks right away (before the
/// edit is compiled) for completions at the end of `edit`.
fn complete_after(client: &Connection, uri: &Url, version: i32, edit: &str) -> Vec<CompletionItem> {
    let text = PROGRAM.replace("    // EDIT", edit);
    did_change(client, uri, version, &text);
    let result = request(
        client,
        "textDocument/completion",
        position(uri, after(&text, edit)),
    )
    .expect("completion");
    match serde_json::from_value::<Option<CompletionResponse>>(result).expect("completions") {
        Some(CompletionResponse::Array(items)) => items,
        Some(CompletionResponse::List(list)) => list.items,
        None => Vec::new(),
    }
}

fn labels(items: &[CompletionItem]) -> Vec<&str> {
    items.iter().map(|item| item.label.as_str()).collect()
}

#[test]
fn capabilities_advertise_editing() {
    let (client, handle) = start_server();
    let result = initialize(&client, serde_json::json!({ "capabilities": {} }));
    let capabilities = &result["capabilities"];
    assert_eq!(
        capabilities["completionProvider"]["triggerCharacters"],
        serde_json::json!([".", "@"])
    );
    assert_eq!(
        capabilities["signatureHelpProvider"]["triggerCharacters"],
        serde_json::json!(["(", ","])
    );
    assert_eq!(
        capabilities["codeActionProvider"]["codeActionKinds"],
        serde_json::json!(["quickfix"])
    );
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn names_in_scope_complete_mid_edit() {
    let (client, handle, uri) = open_program();
    let items = complete_after(&client, &uri, 2, "    fi");
    let labels = labels(&items);
    for expected in [
        "first", "point", "add", "limit", "Point", "Shape", "loop", "@bit8",
    ] {
        if expected.starts_with('@') {
            assert!(!labels.contains(&expected), "{expected}: {labels:?}");
        } else {
            assert!(labels.contains(&expected), "{expected}: {labels:?}");
        }
    }
    // `inner`'s block is closed and `later` is declared further down.
    assert!(!labels.contains(&"inner"), "{labels:?}");
    assert!(!labels.contains(&"later"), "{labels:?}");
    let first = items
        .iter()
        .find(|item| item.label == "first")
        .expect("first");
    assert_eq!(first.detail.as_deref(), Some("@bit8"));
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn members_complete_after_a_dot() {
    let (client, handle, uri) = open_program();
    let value = complete_after(&client, &uri, 2, "    point.");
    assert_eq!(sorted(labels(&value)), vec!["sum", "x", "y"]);
    let field = value.iter().find(|item| item.label == "x").expect("x");
    assert_eq!(field.detail.as_deref(), Some("@bit8"));

    let ty = complete_after(&client, &uri, 3, "    Point.");
    assert_eq!(labels(&ty), vec!["sum"]);

    let variants = complete_after(&client, &uri, 4, "    shape: Shape = .");
    assert_eq!(sorted(labels(&variants)), vec!["Dot", "Line"]);
    shutdown(&client);
    handle.join().expect("server thread");
}

fn sorted(mut labels: Vec<&str>) -> Vec<&str> {
    labels.sort_unstable();
    labels
}

#[test]
fn members_complete_after_the_broken_buffer_compiles() {
    let (client, handle, uri) = open_program();
    let text = PROGRAM.replace("    // EDIT", "    point.");
    did_change(&client, &uri, 2, &text);
    let _ = publish_for(&client, &uri);
    let result = request(
        &client,
        "textDocument/completion",
        position(&uri, after(&text, "    point.")),
    )
    .expect("completion");
    let Some(CompletionResponse::Array(items)) = serde_json::from_value(result).expect("items")
    else {
        panic!("completions");
    };
    assert_eq!(sorted(labels(&items)), vec!["sum", "x", "y"]);
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn intrinsics_complete_after_an_at_sign() {
    let (client, handle, uri) = open_program();
    let items = complete_after(&client, &uri, 2, "    @i_ad");
    let add = items
        .iter()
        .find(|item| item.label == "@i_add_8")
        .expect("@i_add_8");
    assert_eq!(add.detail.as_deref(), Some("@i_add_8(@bit8, @bit8) @bit8"));
    // The edit replaces the `@` too.
    let Some(CompletionTextEdit::Edit(edit)) = &add.text_edit else {
        panic!("a text edit");
    };
    assert_eq!(edit.range.start.character, 4);
    assert!(labels(&items).contains(&"@builtin"));
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn library_namespaces_complete() {
    let dir = std::env::temp_dir().join(format!("lilac-lsp-complete-lib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    let app = dir.join("app");
    std::fs::create_dir_all(&lib).expect("mkdir");
    std::fs::create_dir_all(&app).expect("mkdir");
    std::fs::write(lib.join("lib.lilac"), "helper: function () nothing {\n}\n").expect("write");
    let main = app.join("main.lilac");
    let text = "main: function () nothing {\n    Lib.helper()\n}\n";
    std::fs::write(&main, text).expect("write");

    let (client, handle) = start_server();
    initialize(
        &client,
        serde_json::json!({
            "capabilities": {},
            "rootUri": Url::from_directory_path(&app).expect("root").as_str(),
            "initializationOptions": { "libraries": { "Lib": lib.to_string_lossy() } },
        }),
    );
    let uri = Url::from_file_path(&main).expect("uri");
    did_open(&client, &uri, text);
    let published = publish_for(&client, &uri);
    assert!(
        published
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::HINT)),
        "{:?}",
        published.diagnostics
    );

    let result = request(
        &client,
        "textDocument/completion",
        position(&uri, after(text, "Lib.")),
    )
    .expect("completion");
    let Some(CompletionResponse::Array(items)) = serde_json::from_value(result).expect("items")
    else {
        panic!("completions");
    };
    assert_eq!(labels(&items), vec!["helper"]);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn aliases_navigate_complete_and_rename() {
    let dir = std::env::temp_dir().join(format!("lilac-lsp-alias-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    let app = dir.join("app");
    std::fs::create_dir_all(&lib).expect("mkdir");
    std::fs::create_dir_all(&app).expect("mkdir");
    std::fs::write(
        lib.join("lib.lilac"),
        "helper: function () nothing {\n}\nThing: object {\n    v: @bit8\n}\n",
    )
    .expect("write");
    let main = app.join("main.lilac");
    let text = "using Lib as L\nmain: function () nothing {\n    t: L.Thing = L.Thing { v = 0x1 }\n    L.helper()\n}\n";
    std::fs::write(&main, text).expect("write");

    let (client, handle) = start_server();
    initialize(
        &client,
        serde_json::json!({
            "capabilities": {},
            "rootUri": Url::from_directory_path(&app).expect("root").as_str(),
            "initializationOptions": { "libraries": { "Lib": lib.to_string_lossy() } },
        }),
    );
    let uri = Url::from_file_path(&main).expect("uri");
    did_open(&client, &uri, text);
    let published = publish_for(&client, &uri);
    assert!(
        published
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::HINT)),
        "{:?}",
        published.diagnostics
    );
    // The alias in `L.helper()`, and its declaration in the `using`.
    let call = position(&uri, after(text, "\n    "));
    let declared = serde_json::json!({
        "start": { "line": 0, "character": 13 },
        "end": { "line": 0, "character": 14 },
    });

    let result = request(
        &client,
        "textDocument/completion",
        position(&uri, after(text, "L.")),
    )
    .expect("completion");
    let Some(CompletionResponse::Array(items)) = serde_json::from_value(result).expect("items")
    else {
        panic!("completions");
    };
    let mut names = labels(&items);
    names.sort_unstable();
    assert_eq!(names, vec!["Thing", "helper"]);

    let hover = request(&client, "textDocument/hover", call.clone()).expect("hover");
    assert!(hover.to_string().contains("namespace Lib"), "{hover}");

    let definition = request(&client, "textDocument/definition", call.clone()).expect("definition");
    assert!(
        definition["uri"]
            .as_str()
            .is_some_and(|target| target.ends_with("/app/main.lilac")),
        "{definition}"
    );
    assert_eq!(definition["range"], declared, "{definition}");

    let mut params = call.clone();
    params["context"] = serde_json::json!({ "includeDeclaration": true });
    let references = request(&client, "textDocument/references", params).expect("references");
    let references = references.as_array().expect("locations");
    // The declaration, the annotation, the struct literal and the call.
    assert_eq!(references.len(), 4, "{references:?}");
    assert_eq!(references[0]["range"], declared);

    let mut params = call;
    params["newName"] = serde_json::json!("Library");
    let edit = request(&client, "textDocument/rename", params).expect("rename");
    let changes = edit["changes"].as_object().expect("changes");
    assert_eq!(changes.len(), 1, "{edit}");
    let edits = changes
        .values()
        .next()
        .and_then(serde_json::Value::as_array)
        .expect("edits in main.lilac");
    let mut starts: Vec<(u64, u64)> = edits
        .iter()
        .map(|edit| {
            assert_eq!(edit["newText"], "Library");
            let start = &edit["range"]["start"];
            (
                start["line"].as_u64().expect("line"),
                start["character"].as_u64().expect("character"),
            )
        })
        .collect();
    starts.sort_unstable();
    assert_eq!(starts, vec![(0, 13), (2, 7), (2, 17), (3, 4)]);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

fn signature_after(client: &Connection, uri: &Url, version: i32, edit: &str) -> SignatureHelp {
    let text = PROGRAM.replace("    // EDIT", edit);
    did_change(client, uri, version, &text);
    let result = request(
        client,
        "textDocument/signatureHelp",
        position(uri, after(&text, edit)),
    )
    .expect("signature help");
    serde_json::from_value(result).expect("a signature")
}

#[test]
fn signature_help_tracks_the_argument() {
    let (client, handle, uri) = open_program();

    let help = signature_after(&client, &uri, 2, "    x: @bit8 = add(first, ");
    assert_eq!(
        help.signatures[0].label,
        "add(a: @bit8, b: @bit8) total: @bit8"
    );
    assert_eq!(help.active_parameter, Some(1));

    // A method called on a value takes no `self` argument.
    let help = signature_after(&client, &uri, 3, "    x: @bit8 = point.sum(");
    assert_eq!(help.signatures[0].label, "Point.sum() total: @bit8");

    let help = signature_after(&client, &uri, 4, "    x: @bit8 = @i_add_8(first, ");
    assert_eq!(help.signatures[0].label, "@i_add_8(@bit8, @bit8) @bit8");
    assert_eq!(help.active_parameter, Some(1));

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn did_you_mean_is_a_quick_fix() {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "main: function () nothing {\n    count: @bit8 = 0x1\n    other: @bit8 = cuont\n}\n";
    did_open(&client, &uri, text);
    let published = publish_for(&client, &uri);
    assert_eq!(
        published.diagnostics.len(),
        1,
        "{:?}",
        published.diagnostics
    );

    let result = request(
        &client,
        "textDocument/codeAction",
        serde_json::json!({
            "textDocument": { "uri": uri.as_str() },
            "range": published.diagnostics[0].range,
            "context": { "diagnostics": published.diagnostics },
        }),
    )
    .expect("code actions");
    let actions: Vec<CodeActionOrCommand> = serde_json::from_value(result).expect("actions");
    let [CodeActionOrCommand::CodeAction(action)] = actions.as_slice() else {
        panic!("one quick fix: {actions:?}");
    };
    assert_eq!(action.title, "Change to `count`");
    let edits = &action
        .edit
        .as_ref()
        .expect("edit")
        .changes
        .as_ref()
        .expect("changes")[&uri];
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "count");
    assert_eq!(edits[0].range.start, lsp_types::Position::new(2, 19));
    assert_eq!(edits[0].range.end, lsp_types::Position::new(2, 24));

    shutdown(&client);
    handle.join().expect("server thread");
}
