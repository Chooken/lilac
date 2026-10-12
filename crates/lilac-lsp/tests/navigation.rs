//! Phase 3 navigation: references, highlights, rename, symbols, type
//! definition and the richer hovers.

use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId, ResponseError};
use lsp_types::{
    DidOpenTextDocumentParams, DocumentHighlight, DocumentHighlightKind, DocumentSymbol, Location,
    PublishDiagnosticsParams, SymbolKind, TextDocumentItem, Url, WorkspaceEdit,
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
    second: @bit8 = add(first, first)
    point: Point = Point {
        x = first
        y = second
    }
    shape: Shape = .Line(point.x)
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

/// The `(line, character)` of `offset` bytes into the `nth` (0-based)
/// occurrence of `needle` in `text` (ASCII only).
fn at(text: &str, needle: &str, nth: usize, offset: usize) -> (u32, u32) {
    let byte = text
        .match_indices(needle)
        .nth(nth)
        .unwrap_or_else(|| panic!("no occurrence {nth} of {needle:?}"))
        .0
        + offset;
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

/// A server with `PROGRAM` open (checked with no errors) as a single file.
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
            .all(|diagnostic| diagnostic.severity == Some(lsp_types::DiagnosticSeverity::HINT)),
        "{:?}",
        published.diagnostics
    );
    (client, handle, uri)
}

fn references(client: &Connection, uri: &Url, at: (u32, u32), declaration: bool) -> Vec<Location> {
    let mut params = position(uri, at);
    params["context"] = serde_json::json!({ "includeDeclaration": declaration });
    let result = request(client, "textDocument/references", params).expect("references");
    let mut locations: Vec<Location> = serde_json::from_value(result).expect("locations");
    locations.sort_by_key(|location| (location.range.start.line, location.range.start.character));
    locations
}

fn starts(locations: &[Location]) -> Vec<(u32, u32)> {
    locations
        .iter()
        .map(|location| (location.range.start.line, location.range.start.character))
        .collect()
}

#[test]
fn capabilities_advertise_navigation() {
    let (client, handle) = start_server();
    let result = initialize(&client, serde_json::json!({ "capabilities": {} }));
    let capabilities = &result["capabilities"];
    for key in [
        "referencesProvider",
        "documentHighlightProvider",
        "documentSymbolProvider",
        "workspaceSymbolProvider",
        "typeDefinitionProvider",
    ] {
        assert_eq!(capabilities[key], serde_json::json!(true), "{key}");
    }
    assert_eq!(capabilities["renameProvider"]["prepareProvider"], true);
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn references_of_a_function_include_its_declaration_on_request() {
    let (client, handle, uri) = open_program();
    let call = at(PROGRAM, "= add(", 0, 3);

    let with = references(&client, &uri, call, true);
    assert_eq!(
        starts(&with),
        vec![
            at(PROGRAM, "add:", 0, 0),
            at(PROGRAM, "= add(", 0, 2),
            at(PROGRAM, "= add(", 1, 2)
        ]
    );
    let without = references(&client, &uri, call, false);
    assert_eq!(starts(&without), starts(&with)[1..].to_vec());

    // From the declaration's name too.
    let from_declaration = references(&client, &uri, at(PROGRAM, "add:", 0, 1), true);
    assert_eq!(starts(&from_declaration), starts(&with));

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn references_of_a_local_and_a_parameter() {
    let (client, handle, uri) = open_program();
    let local = references(&client, &uri, at(PROGRAM, "first", 2, 1), true);
    assert_eq!(
        starts(&local),
        vec![
            at(PROGRAM, "first", 0, 0),
            at(PROGRAM, "first", 1, 0),
            at(PROGRAM, "first", 2, 0),
            at(PROGRAM, "first", 3, 0),
        ]
    );

    let parameter = references(&client, &uri, at(PROGRAM, "a, b)", 0, 0), true);
    assert_eq!(
        starts(&parameter),
        vec![at(PROGRAM, "a: @bit8", 0, 0), at(PROGRAM, "a, b)", 0, 0)]
    );

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn highlights_mark_the_declaration_as_a_write() {
    let (client, handle, uri) = open_program();
    let result = request(
        &client,
        "textDocument/documentHighlight",
        position(&uri, at(PROGRAM, "second", 1, 2)),
    )
    .expect("highlight");
    let highlights: Vec<DocumentHighlight> = serde_json::from_value(result).expect("highlights");
    let kinds: Vec<_> = highlights.iter().map(|highlight| highlight.kind).collect();
    assert_eq!(
        kinds,
        vec![
            Some(DocumentHighlightKind::WRITE),
            Some(DocumentHighlightKind::READ)
        ]
    );
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn definition_of_a_local_and_a_parameter() {
    let (client, handle, uri) = open_program();
    for (from, to) in [
        (at(PROGRAM, "first", 3, 1), at(PROGRAM, "first", 0, 0)),
        (at(PROGRAM, "a, b)", 0, 0), at(PROGRAM, "a: @bit8", 0, 0)),
        (at(PROGRAM, "self.x", 0, 1), at(PROGRAM, "self)", 0, 0)),
    ] {
        let result =
            request(&client, "textDocument/definition", position(&uri, from)).expect("definition");
        let location: Location = serde_json::from_value(result).expect("location");
        assert_eq!(
            (location.range.start.line, location.range.start.character),
            to,
            "from {from:?}"
        );
    }
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn type_definition_of_a_local_goes_to_its_object() {
    let (client, handle, uri) = open_program();
    for from in [at(PROGRAM, "point:", 0, 1), at(PROGRAM, "point.x", 0, 1)] {
        let result = request(&client, "textDocument/typeDefinition", position(&uri, from))
            .expect("type definition");
        let location: Location = serde_json::from_value(result).expect("location");
        assert_eq!(location.range.start, lsp_types::Position::new(0, 0));
    }
    // A variant's type is its enum.
    let result = request(
        &client,
        "textDocument/typeDefinition",
        position(&uri, at(PROGRAM, ".Line(", 0, 2)),
    )
    .expect("type definition");
    let location: Location = serde_json::from_value(result).expect("location");
    let shape = at(PROGRAM, "Shape: enum", 0, 0);
    assert_eq!(
        location.range.start,
        lsp_types::Position::new(shape.0, shape.1)
    );
    shutdown(&client);
    handle.join().expect("server thread");
}

fn edits_by_start(edit: &WorkspaceEdit) -> Vec<((u32, u32), String)> {
    let mut edits: Vec<_> = edit
        .changes
        .as_ref()
        .expect("changes")
        .values()
        .flatten()
        .map(|edit| {
            (
                (edit.range.start.line, edit.range.start.character),
                edit.new_text.clone(),
            )
        })
        .collect();
    edits.sort();
    edits
}

#[test]
fn rename_a_function_and_a_local() {
    let (client, handle, uri) = open_program();

    let prepared = request(
        &client,
        "textDocument/prepareRename",
        position(&uri, at(PROGRAM, "= add(", 0, 4)),
    )
    .expect("prepare");
    let range: lsp_types::Range = serde_json::from_value(prepared).expect("range");
    let call = at(PROGRAM, "= add(", 0, 2);
    assert_eq!(range.start, lsp_types::Position::new(call.0, call.1));
    assert_eq!(range.end.character, call.1 + 3);

    let mut params = position(&uri, at(PROGRAM, "= add(", 0, 4));
    params["newName"] = serde_json::json!("plus");
    let edit: WorkspaceEdit =
        serde_json::from_value(request(&client, "textDocument/rename", params).expect("rename"))
            .expect("edit");
    let plus = |at: (u32, u32)| (at, "plus".to_owned());
    assert_eq!(
        edits_by_start(&edit),
        vec![
            plus(at(PROGRAM, "add:", 0, 0)),
            plus(at(PROGRAM, "= add(", 0, 2)),
            plus(at(PROGRAM, "= add(", 1, 2)),
        ]
    );

    let mut params = position(&uri, at(PROGRAM, "second", 0, 0));
    params["newName"] = serde_json::json!("other");
    let edit: WorkspaceEdit =
        serde_json::from_value(request(&client, "textDocument/rename", params).expect("rename"))
            .expect("edit");
    let other = |nth| (at(PROGRAM, "second", nth, 0), "other".to_owned());
    assert_eq!(edits_by_start(&edit), vec![other(0), other(1)]);

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn rename_a_variant_leaves_its_enum_alone() {
    let (client, handle, uri) = open_program();
    let mut params = position(&uri, at(PROGRAM, ".Line(", 0, 2));
    params["newName"] = serde_json::json!("Segment");
    let edit: WorkspaceEdit =
        serde_json::from_value(request(&client, "textDocument/rename", params).expect("rename"))
            .expect("edit");
    let segment = |nth| (at(PROGRAM, "Line", nth, 0), "Segment".to_owned());
    assert_eq!(edits_by_start(&edit), vec![segment(0), segment(1)]);
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn rename_refuses_keywords_and_hooks() {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    let text = "Wide: object {\n    data: @bit8\n    @add: function (lhs: Wide, rhs: Wide) Wide => lhs\n}\nmain: function () nothing {\n}\n";
    did_open(&client, &uri, text);
    let _ = publish_for(&client, &uri);

    let error = request(
        &client,
        "textDocument/prepareRename",
        position(&uri, at(text, "@add", 0, 1)),
    )
    .expect_err("hooks keep their names");
    assert!(error.message.contains("hook"), "{}", error.message);

    let mut params = position(&uri, at(text, "data", 0, 0));
    params["newName"] = serde_json::json!("match");
    let error = request(&client, "textDocument/rename", params).expect_err("a keyword");
    assert!(error.message.contains("identifier"), "{}", error.message);

    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn rename_refuses_a_library_definition() {
    let dir = std::env::temp_dir().join(format!("lilac-lsp-rename-lib-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    let app = dir.join("app");
    std::fs::create_dir_all(&lib).expect("mkdir");
    std::fs::create_dir_all(&app).expect("mkdir");
    std::fs::write(lib.join("lib.lilac"), "helper: function () nothing {\n}\n").expect("write");
    let main = app.join("main.lilac");
    let text = "using Lib\n\nmain: function () nothing {\n    helper()\n}\n";
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
        published.diagnostics.is_empty(),
        "{:?}",
        published.diagnostics
    );

    let error = request(
        &client,
        "textDocument/prepareRename",
        position(&uri, at(text, "helper", 0, 1)),
    )
    .expect_err("a library function");
    assert!(error.message.contains("`Lib`"), "{}", error.message);

    shutdown(&client);
    handle.join().expect("server thread");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn document_symbols_nest_members() {
    let (client, handle, uri) = open_program();
    let result = request(
        &client,
        "textDocument/documentSymbol",
        serde_json::json!({ "textDocument": { "uri": uri.as_str() } }),
    )
    .expect("symbols");
    let symbols: Vec<DocumentSymbol> = serde_json::from_value(result).expect("symbols");
    let top: Vec<(&str, SymbolKind)> = symbols
        .iter()
        .map(|symbol| (symbol.name.as_str(), symbol.kind))
        .collect();
    assert_eq!(
        top,
        vec![
            ("Point", SymbolKind::STRUCT),
            ("Shape", SymbolKind::ENUM),
            ("limit", SymbolKind::VARIABLE),
            ("add", SymbolKind::FUNCTION),
            ("main", SymbolKind::FUNCTION),
        ]
    );
    let members = |at: usize| -> Vec<(String, SymbolKind)> {
        symbols[at]
            .children
            .iter()
            .flatten()
            .map(|symbol| (symbol.name.clone(), symbol.kind))
            .collect()
    };
    assert_eq!(
        members(0),
        vec![
            ("x".to_owned(), SymbolKind::FIELD),
            ("y".to_owned(), SymbolKind::FIELD),
            ("sum".to_owned(), SymbolKind::METHOD),
        ]
    );
    assert_eq!(
        members(1),
        vec![
            ("Dot".to_owned(), SymbolKind::ENUM_MEMBER),
            ("Line".to_owned(), SymbolKind::ENUM_MEMBER),
        ]
    );
    assert_eq!(
        symbols[3].detail.as_deref(),
        Some("function add(@bit8, @bit8) -> @bit8")
    );
    shutdown(&client);
    handle.join().expect("server thread");
}

#[test]
fn workspace_symbols_match_a_subsequence() {
    let (client, handle, uri) = open_program();
    let result = request(
        &client,
        "workspace/symbol",
        serde_json::json!({ "query": "lmt" }),
    )
    .expect("symbols");
    let symbols: Vec<lsp_types::WorkspaceSymbol> = serde_json::from_value(result).expect("symbols");
    let names: Vec<&str> = symbols.iter().map(|symbol| symbol.name.as_str()).collect();
    assert_eq!(names, vec!["limit"]);
    let lsp_types::OneOf::Left(location) = &symbols[0].location else {
        panic!("a location");
    };
    assert_eq!(location.uri, uri);
    shutdown(&client);
    handle.join().expect("server thread");
}

fn hover_text(client: &Connection, uri: &Url, at: (u32, u32)) -> String {
    let result = request(client, "textDocument/hover", position(uri, at)).expect("hover");
    result["contents"]["value"]
        .as_str()
        .unwrap_or_else(|| panic!("no hover at {at:?}: {result}"))
        .to_owned()
}

#[test]
fn hovers_show_fields_variants_globals_and_intrinsics() {
    let (client, handle, uri) = open_program();
    for (from, expected) in [
        (at(PROGRAM, "point.x", 0, 6), "field Point.x: @bit8"),
        (at(PROGRAM, ".Line(", 0, 2), "variant Shape.Line: @bit8"),
        (at(PROGRAM, "limit)", 0, 1), "global limit: @bit8 = 0x10"),
        (
            at(PROGRAM, "@i_add_8", 1, 2),
            "@i_add_8(@bit8, @bit8) @bit8",
        ),
    ] {
        let text = hover_text(&client, &uri, from);
        assert!(text.contains(expected), "{text}");
    }
    shutdown(&client);
    handle.join().expect("server thread");
}

const ALIASES: &str = "\
Point: object {
    x: @bit8
}
Spot: alias Point
make: function () spot: Spot {
    spot = Spot { x = 0x1 }
}
main: function () nothing {
    here: Spot = make()
}
";

/// A type alias is an item: hover shows its declaration, definition and
/// references find it, type definition goes on to the type it names, and
/// a rename changes every spelling of it but not the type.
#[test]
fn aliases_hover_navigate_and_rename() {
    let (client, handle) = start_server();
    initialize(&client, serde_json::json!({ "capabilities": {} }));
    let uri = Url::parse("file:///c:/main.lilac").expect("uri");
    did_open(&client, &uri, ALIASES);
    let published = publish_for(&client, &uri);
    assert!(
        published
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity == Some(lsp_types::DiagnosticSeverity::HINT)),
        "{:?}",
        published.diagnostics
    );
    let use_site = at(ALIASES, "here: Spot", 0, 7);

    let hover = request(&client, "textDocument/hover", position(&uri, use_site)).expect("hover");
    let hover = hover.to_string();
    assert!(hover.contains("Spot: alias Point"), "{hover}");

    let definition =
        request(&client, "textDocument/definition", position(&uri, use_site)).expect("definition");
    let definition: Location = serde_json::from_value(definition).expect("location");
    let declared = at(ALIASES, "Spot: alias", 0, 0);
    assert_eq!(
        definition.range.start,
        lsp_types::Position::new(declared.0, declared.1)
    );

    let type_definition = request(
        &client,
        "textDocument/typeDefinition",
        position(&uri, use_site),
    )
    .expect("type definition");
    let type_definition: Location = serde_json::from_value(type_definition).expect("location");
    let point = at(ALIASES, "Point: object", 0, 0);
    assert_eq!(
        type_definition.range.start,
        lsp_types::Position::new(point.0, point.1)
    );

    let spots = [
        at(ALIASES, "Spot", 0, 0),
        at(ALIASES, "Spot", 1, 0),
        at(ALIASES, "Spot", 2, 0),
        at(ALIASES, "Spot", 3, 0),
    ];
    assert_eq!(
        starts(&references(&client, &uri, use_site, true)),
        spots.to_vec()
    );

    let mut params = position(&uri, use_site);
    params["newName"] = serde_json::json!("Place");
    let edit: WorkspaceEdit =
        serde_json::from_value(request(&client, "textDocument/rename", params).expect("rename"))
            .expect("edit");
    let place = |at: (u32, u32)| (at, "Place".to_owned());
    assert_eq!(
        edits_by_start(&edit),
        spots.into_iter().map(place).collect::<Vec<_>>()
    );

    shutdown(&client);
    handle.join().expect("server thread");
}
