//! Phase 4 editing assistance: completion, signature help and quick fixes.

use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, ResponseError};
use lsp_types::{
    CodeActionOrCommand, CompletionItem, CompletionResponse, CompletionTextEdit,
    DiagnosticSeverity, DidChangeTextDocumentParams, DidOpenTextDocumentParams, Documentation,
    PublishDiagnosticsParams, SignatureHelp, TextDocumentContentChangeEvent, TextDocumentItem, Url,
    VersionedTextDocumentIdentifier,
};

const PROGRAM: &str = "\
Point: structure {
    x: @bit8
    y: @bit8
    sum: function (self) total: @bit8 {
        total = @i_add_8(self.x, self.y)
    }
}
Shape: either {
    Dot
    Line: @bit8
}
limit: @bit8 = 0x10
add: function (a: @bit8, b: @bit8) total: @bit8 {
    total = @i_add_8(a, b)
}
paint: function (shape: Shape) nothing {
}
origin: function () p: Point {
    p = Point {
        x = 0x0
        y = 0x0
    }
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

/// A change: the range's start and end `(line, character)`s, and its text.
type RangedChange<'a> = ((u32, u32, u32, u32), &'a str);

/// Sends one change per `(range, text)` pair, each a ranged edit of the
/// text the ones before it left.
fn did_change_ranges(client: &Connection, uri: &Url, version: i32, changes: &[RangedChange]) {
    let content_changes = changes
        .iter()
        .map(
            |&((line, character, end_line, end_character), text)| TextDocumentContentChangeEvent {
                range: Some(lsp_types::Range::new(
                    lsp_types::Position::new(line, character),
                    lsp_types::Position::new(end_line, end_character),
                )),
                range_length: None,
                text: text.to_owned(),
            },
        )
        .collect();
    let params = DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier {
            uri: uri.clone(),
            version,
        },
        content_changes,
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
fn ranged_edits_map_the_cursor_to_the_compiled_text() {
    let (client, handle, uri) = open_program();
    // Two notifications, the second with two changes: the `// EDIT` line
    // (32) becomes `    fi`, then lines are added above it, so the
    // cursor sits on line 34 of a text the last compile never saw.
    did_change_ranges(&client, &uri, 2, &[((32, 4, 32, 11), "fi")]);
    did_change_ranges(
        &client,
        &uri,
        3,
        &[((0, 0, 0, 0), "// one\n"), ((1, 0, 1, 0), "// two\n")],
    );
    let result =
        request(&client, "textDocument/completion", position(&uri, (34, 6))).expect("completion");
    let items =
        match serde_json::from_value::<Option<CompletionResponse>>(result).expect("completions") {
            Some(CompletionResponse::Array(items)) => items,
            Some(CompletionResponse::List(list)) => list.items,
            None => Vec::new(),
        };
    let labels = labels(&items);
    assert!(labels.contains(&"first"), "{labels:?}");
    assert!(labels.contains(&"point"), "{labels:?}");
    assert!(!labels.contains(&"inner"), "{labels:?}");
    assert!(!labels.contains(&"later"), "{labels:?}");
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

/// Replaces the `// EDIT` line with `edit` and asks right away for
/// completions where `<|>` stands in it (the marker itself is removed).
fn complete_marked(
    client: &Connection,
    uri: &Url,
    version: i32,
    edit: &str,
) -> Vec<CompletionItem> {
    let (before, after_marker) = edit.split_once("<|>").expect("marker");
    let text = PROGRAM.replace("    // EDIT", &format!("{before}{after_marker}"));
    did_change(client, uri, version, &text);
    let prefix = PROGRAM[..PROGRAM.find("    // EDIT").expect("edit line")].to_owned() + before;
    let result = request(
        client,
        "textDocument/completion",
        position(uri, after(&prefix, before)),
    )
    .expect("completion");
    match serde_json::from_value::<Option<CompletionResponse>>(result).expect("completions") {
        Some(CompletionResponse::Array(items)) => items,
        Some(CompletionResponse::List(list)) => list.items,
        None => Vec::new(),
    }
}

/// A bare `.` lists the variants of the enum the compiler expects there,
/// and nothing where it expects none.
#[test]
fn tags_complete_from_the_expected_enum() {
    let (client, handle, uri) = open_program();
    let arm = complete_marked(
        &client,
        &uri,
        2,
        "    s: Shape = .Dot\n    match s {\n        .Line => {}\n        .<|>\n    }",
    );
    assert_eq!(labels(&arm), vec!["Dot"]);
    let line = complete_marked(&client, &uri, 3, "    paint(.<|>)");
    assert_eq!(sorted(labels(&line)), vec!["Dot", "Line"]);
    let payload = line.iter().find(|item| item.label == "Line").expect("Line");
    assert_eq!(payload.detail.as_deref(), Some("@bit8"));
    let os = complete_marked(
        &client,
        &uri,
        4,
        "    b: @bit8 = @builtin.Target.os == .<|>",
    );
    let os = labels(&os);
    assert!(os.contains(&"Linux") && os.contains(&"Windows"), "{os:?}");
    let partial = complete_marked(&client, &uri, 5, "    s: Shape = .Li<|>");
    assert_eq!(sorted(labels(&partial)), vec!["Dot", "Line"]);
    let none = complete_marked(&client, &uri, 6, "    v: @bit8 = .<|>");
    assert_eq!(labels(&none), Vec::<&str>::new());
    let statement = complete_marked(&client, &uri, 7, "    .<|>");
    assert_eq!(labels(&statement), Vec::<&str>::new());
    shutdown(&client);
    handle.join().expect("server thread");
}

/// A top-level `match` is decided before the final check, which never sees
/// its patterns; their tags still complete.
#[test]
fn top_level_match_tags_complete() {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/top.lilac").expect("uri");
    let text = "match @builtin.Target.os {\n    .Windows => {\n        a: @bit8 = 0x1\n    }\n    .\n}\nmain: function () nothing {\n}\n";
    did_open(&client, &uri, text);
    let result =
        request(&client, "textDocument/completion", position(&uri, (4, 5))).expect("completion");
    let Some(CompletionResponse::Array(items)) = serde_json::from_value(result).expect("items")
    else {
        panic!("completions");
    };
    let labels = labels(&items);
    assert!(labels.contains(&"Linux"), "{labels:?}");
    assert!(!labels.contains(&"Windows"), "{labels:?}");

    // An undecided top-level condition adds no error of its own.
    let text = format!(
        "{text}if @builtin.Target.os == . {{
}}
"
    );
    did_change(&client, &uri, 2, &text);
    let published = publish_for(&client, &uri);
    assert_eq!(published.version, Some(2));
    let errors: Vec<&str> = published
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::ERROR))
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert_eq!(
        errors,
        [
            "expected match tag after `.`, found `}`",
            "expected tag name after `.`, found `{`",
        ]
    );
    shutdown(&client);
    handle.join().expect("server thread");
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

/// After `x.` the members are those of what the compiler looked the name
/// up in: a call's result, a partly typed name, a namespace path in a type
/// or an expression; nothing when it is a value with no members.
#[test]
fn members_complete_from_what_the_compiler_looked_up() {
    let (client, handle, uri) = open_program();
    let call = complete_marked(&client, &uri, 2, "    origin().<|>");
    assert_eq!(sorted(labels(&call)), vec!["sum", "x", "y"]);
    let partial = complete_marked(&client, &uri, 3, "    v: @bit8 = point.s<|>");
    assert_eq!(sorted(labels(&partial)), vec!["sum", "x", "y"]);
    let method = complete_marked(&client, &uri, 4, "    point.s<|>()");
    assert_eq!(sorted(labels(&method)), vec!["sum", "x", "y"]);
    let ty = complete_marked(&client, &uri, 5, "    q: @builtin.<|> = 0x1");
    assert!(labels(&ty).contains(&"Target"), "{:?}", labels(&ty));
    let path = complete_marked(&client, &uri, 6, "    b: @bit8 = @builtin.Target.<|>");
    assert!(labels(&path).contains(&"os"), "{:?}", labels(&path));
    let number = complete_marked(&client, &uri, 7, "    first.<|>");
    assert_eq!(labels(&number), Vec::<&str>::new());
    shutdown(&client);
    handle.join().expect("server thread");
}

/// A top-level condition's namespace paths complete, though the final
/// check never sees the condition.
#[test]
fn top_level_condition_members_complete() {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/top.lilac").expect("uri");
    let text = "if @builtin.Target. == .Linux {\n}\nmain: function () nothing {\n}\n";
    did_open(&client, &uri, text);
    let result = request(
        &client,
        "textDocument/completion",
        position(&uri, after(text, "Target.")),
    )
    .expect("completion");
    let Some(CompletionResponse::Array(items)) = serde_json::from_value(result).expect("items")
    else {
        panic!("completions");
    };
    let labels = labels(&items);
    assert!(labels.contains(&"os"), "{labels:?}");
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
    assert_eq!(
        add.detail.as_deref(),
        Some("@i_add_8: function (left: @bit8, right: @bit8) @bit8")
    );
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
        "helper: function () nothing {\n}\nThing: structure {\n    v: @bit8\n}\n",
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
        "add: function (a: @bit8, b: @bit8) total: @bit8"
    );
    assert_eq!(help.active_parameter, Some(1));

    // A method called on a value takes no `self` argument.
    let help = signature_after(&client, &uri, 3, "    x: @bit8 = point.sum(");
    assert_eq!(
        help.signatures[0].label,
        "Point.sum: function () total: @bit8"
    );

    let help = signature_after(&client, &uri, 4, "    x: @bit8 = @i_add_8(first, ");
    assert_eq!(
        help.signatures[0].label,
        "@i_add_8: function (left: @bit8, right: @bit8) @bit8"
    );
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

/// A `.` with no name mid-edit (a tag, a member, a path segment) is one
/// parse error; the rest of its function is still checked.
#[test]
fn a_bare_dot_keeps_its_function_checked() {
    let (client, handle, uri) = open_program();
    let text = PROGRAM.replace(
        "    // EDIT",
        "    s: Shape = .Dot\n    match s {\n        .\n    }\n    t: Shape = .\n    point.\n    \
         origin().\n    q: @builtin. = 0x1\n    paint(@builtin.Target.)\n    u: @bit8 = missing",
    );
    did_change(&client, &uri, 2, &text);
    let published = publish_for(&client, &uri);
    let errors: Vec<&str> = published
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::ERROR))
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert_eq!(
        errors,
        [
            "expected match tag after `.`, found `}`",
            "expected tag name after `.`, found `point`",
            "expected member name after `.`, found `origin`",
            "expected member name after `.`, found `q`",
            "expected a name after `.`, found `=`",
            "expected member name after `.`, found `)`",
            "unresolved name `missing`",
        ]
    );
    shutdown(&client);
    handle.join().expect("server thread");
}

/// After `name: Type.` the declaration shorthand's static functions of
/// `Type` complete (not its methods), mid-edit or with the call written,
/// also after a generic type's arguments.
#[test]
fn static_functions_complete_in_a_declaration() {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/shorthand.lilac").expect("uri");
    let program = "\
Pair: structure {
    a: @bit8
    create: function (a: @bit8) pair: Pair {
        pair = Pair { a = a }
    }
    zero: function () pair: Pair {
        pair = Pair { a = 0x0 }
    }
    get: function (self) a: @bit8 {
        a = self.a
    }
}
Box[T]: structure {
    value: T
    create: function (value: T) box: Box[T] {
        box = Box[T] { value = value }
    }
    get: function (self) value: T {
        value = self.value
    }
}
main: function () nothing {
    // EDIT
}
";
    let mut version = 1;
    let mut complete = |edit: &str, marker: &str| -> Vec<String> {
        let text = program.replace("    // EDIT", edit);
        if version == 1 {
            did_open(&client, &uri, &text);
        } else {
            did_change(&client, &uri, version, &text);
        }
        version += 1;
        let result = request(
            &client,
            "textDocument/completion",
            position(&uri, after(&text, marker)),
        )
        .expect("completion");
        let items = match serde_json::from_value::<Option<CompletionResponse>>(result)
            .expect("completions")
        {
            Some(CompletionResponse::Array(items)) => items,
            Some(CompletionResponse::List(list)) => list.items,
            None => Vec::new(),
        };
        let mut labels: Vec<String> = items.into_iter().map(|item| item.label).collect();
        labels.sort_unstable();
        labels
    };
    assert_eq!(complete("    p: Pair.", "Pair."), ["create", "zero"]);
    assert_eq!(
        complete("    p: Pair.cr(0x1)", "Pair.cr"),
        ["create", "zero"]
    );
    assert_eq!(complete("    b: Box[@bit8].", "Box[@bit8]."), ["create"]);
    shutdown(&client);
    handle.join().expect("server thread");
}

/// After `Type.` a type's constants complete beside its static functions
/// and variants, also on a generic type with or without arguments; a
/// value's members leave them out.
#[test]
fn member_constants_complete_on_the_type() {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/constants.lilac").expect("uri");
    let program = "\
Limits: structure {
    LOW: @bit8 => 0x1
    HIGH: @bit8 => 0x9
    value: @bit8
    make: function () limits: Limits {
        limits = Limits { value = Limits.LOW }
    }
    get: function (self) value: @bit8 {
        value = self.value
    }
}
Level: either {
    Off
    On
    COUNT: @bit8 => 0x2
}
Gen[T]: structure {
    SIZE: @bit8 => 0x4
    item: T
}
main: function () nothing {
    limits: Limits = Limits.make()
    // EDIT
}
";
    let mut version = 1;
    let mut complete = |edit: &str| -> Vec<String> {
        let text = program.replace("    // EDIT", edit);
        if version == 1 {
            did_open(&client, &uri, &text);
        } else {
            did_change(&client, &uri, version, &text);
        }
        version += 1;
        let result = request(
            &client,
            "textDocument/completion",
            position(&uri, after(&text, edit)),
        )
        .expect("completion");
        let items = match serde_json::from_value::<Option<CompletionResponse>>(result)
            .expect("completions")
        {
            Some(CompletionResponse::Array(items)) => items,
            Some(CompletionResponse::List(list)) => list.items,
            None => Vec::new(),
        };
        let mut labels: Vec<String> = items.into_iter().map(|item| item.label).collect();
        labels.sort_unstable();
        labels
    };
    assert_eq!(
        complete("    x: @bit8 = Limits."),
        ["HIGH", "LOW", "get", "make"]
    );
    assert_eq!(complete("    y: @bit8 = Level."), ["COUNT", "Off", "On"]);
    assert_eq!(complete("    z: @bit8 = Gen."), ["SIZE"]);
    assert_eq!(complete("    w: @bit8 = Gen[@bit8]."), ["SIZE"]);
    assert_eq!(complete("    limits."), ["get", "make", "value"]);
    shutdown(&client);
    handle.join().expect("server thread");
}

/// Inside a type's body, its constants and static functions complete by
/// their bare names; its fields and methods do not (they go through
/// `self`), and outside the type none of them do.
#[test]
fn own_names_complete_inside_the_type() {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/own.lilac").expect("uri");
    let program = "\
Limits: structure {
    LOW: @bit8 => 0x1
    value: @bit8
    make: function () limits: Limits {
        limits = Limits { value = 0x1 }
    }
    get: function (self) result: @bit8 {
        // INSIDE
        result = self.value
    }
}
main: function () nothing {
    // OUTSIDE
}
";
    did_open(&client, &uri, program);
    let _ = publish_for(&client, &uri);
    let mut version = 2;
    let mut complete = |marker: &str, edit: &str| -> Vec<String> {
        let text = program.replace(marker, edit);
        did_change(&client, &uri, version, &text);
        version += 1;
        let result = request(
            &client,
            "textDocument/completion",
            position(&uri, after(&text, edit)),
        )
        .expect("completion");
        match serde_json::from_value::<Option<CompletionResponse>>(result).expect("completions") {
            Some(CompletionResponse::Array(items)) => items,
            Some(CompletionResponse::List(list)) => list.items,
            None => Vec::new(),
        }
        .into_iter()
        .map(|item| item.label)
        .collect()
    };
    let inside = complete("// INSIDE", "x: @bit8 = L");
    for name in ["LOW", "make", "Limits"] {
        assert!(
            inside.iter().any(|label| label == name),
            "{name}: {inside:?}"
        );
    }
    for name in ["value", "get"] {
        assert!(
            !inside.iter().any(|label| label == name),
            "{name}: {inside:?}"
        );
    }
    let outside = complete("// OUTSIDE", "x: @bit8 = L");
    assert!(!outside.iter().any(|label| label == "LOW"), "{outside:?}");
    shutdown(&client);
    handle.join().expect("server thread");
}

/// A call through a function reference shows the reference's signature:
/// a field's (its parameter names as declared, through an alias too, and
/// its doc), a global's and a local's (the type's slots, unnamed).
#[test]
fn signature_help_through_function_references() {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/refs.lilac").expect("uri");
    let program = "\
Combine: alias function (left: @bit8, right: @bit8) total: @bit8
add: function (a: @bit8, b: @bit8) total: @bit8 {
    total = @i_add_8(a, b)
}
Machine: structure {
    /// Runs one step.
    step: ref function (input: @bit8, scale: @bit8) output: @bit8
    combine: ref Combine
}
/// What runs last.
HANDLER: ref function (code: @bit8) nothing
main: function () nothing {
    machine: Machine = Machine { step = ref add, combine = ref add }
    local: ref function (@bit8, @bit8) @bit8 = ref add
    // EDIT
}
";
    did_open(&client, &uri, program);
    let published = publish_for(&client, &uri);
    assert!(
        published
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::HINT)),
        "{:?}",
        published.diagnostics
    );
    let mut version = 2;
    let mut signature = |edit: &str| -> SignatureHelp {
        let text = program.replace("    // EDIT", edit);
        did_change(&client, &uri, version, &text);
        version += 1;
        let result = request(
            &client,
            "textDocument/signatureHelp",
            position(&uri, after(&text, edit)),
        )
        .expect("signature help");
        serde_json::from_value(result).expect("a signature")
    };

    let help = signature("    x: @bit8 = machine.step(0x1, ");
    assert_eq!(
        help.signatures[0].label,
        "step: ref function (input: @bit8, scale: @bit8) output: @bit8"
    );
    assert_eq!(help.active_parameter, Some(1));
    let Some(Documentation::MarkupContent(doc)) = &help.signatures[0].documentation else {
        panic!("a doc: {help:?}");
    };
    assert_eq!(doc.value, "Runs one step.");

    let help = signature("    x: @bit8 = machine.combine(");
    assert_eq!(
        help.signatures[0].label,
        "combine: ref function (left: @bit8, right: @bit8) total: @bit8"
    );

    let help = signature("    HANDLER(");
    assert_eq!(
        help.signatures[0].label,
        "HANDLER: ref function (code: @bit8) nothing"
    );

    let help = signature("    x: @bit8 = local(0x1, ");
    assert_eq!(
        help.signatures[0].label,
        "local: ref function (@bit8, @bit8) @bit8"
    );
    assert_eq!(help.active_parameter, Some(1));

    shutdown(&client);
    handle.join().expect("server thread");
}

/// `E077`'s fix inserts the missing `ref`, before a bare function type and
/// before a function named as a value.
#[test]
fn bare_function_fix_inserts_ref() {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "\
add_one: function (value: @bit8) result: @bit8 {
    result = @i_add_8(value, 0x1)
}
main: function () nothing {
    f: function (@bit8) @bit8 = ref add_one
    g: ref function (@bit8) @bit8 = add_one
}
";
    did_open(&client, &uri, text);
    let published = publish_for(&client, &uri);
    assert_eq!(
        published.diagnostics.len(),
        2,
        "{:?}",
        published.diagnostics
    );
    let mut inserted = Vec::new();
    for diagnostic in &published.diagnostics {
        let result = request(
            &client,
            "textDocument/codeAction",
            serde_json::json!({
                "textDocument": { "uri": uri.as_str() },
                "range": diagnostic.range,
                "context": { "diagnostics": [diagnostic] },
            }),
        )
        .expect("code actions");
        let actions: Vec<CodeActionOrCommand> = serde_json::from_value(result).expect("actions");
        let [CodeActionOrCommand::CodeAction(action)] = actions.as_slice() else {
            panic!("one quick fix: {actions:?}");
        };
        let edits = &action
            .edit
            .as_ref()
            .expect("edit")
            .changes
            .as_ref()
            .expect("changes")[&uri];
        let [edit] = edits.as_slice() else {
            panic!("one edit: {edits:?}");
        };
        assert_eq!(edit.range.start, edit.range.end, "{edit:?}");
        inserted.push((edit.range.start, edit.new_text.clone()));
    }
    inserted.sort_by_key(|(start, _)| (start.line, start.character));
    assert_eq!(
        inserted,
        [
            (lsp_types::Position::new(4, 7), "ref ".to_owned()),
            (lsp_types::Position::new(5, 36), "ref ".to_owned()),
        ]
    );

    shutdown(&client);
    handle.join().expect("server thread");
}
