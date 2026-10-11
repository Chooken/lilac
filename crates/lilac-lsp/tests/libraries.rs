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

/// Initializes with `initializationOptions` and returns the server's
/// `initialize` result.
fn initialize(client: &Connection, options: &serde_json::Value) -> serde_json::Value {
    initialize_with(client, &serde_json::json!({}), options)
}

fn initialize_with(
    client: &Connection,
    capabilities: &serde_json::Value,
    options: &serde_json::Value,
) -> serde_json::Value {
    let id = RequestId::from(1);
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: "initialize".to_owned(),
            params: serde_json::json!({
                "capabilities": capabilities,
                "initializationOptions": options,
            }),
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
    token_type: u32,
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
            token_type: token.token_type,
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

const VARIABLE: u32 = 5;
const PARAMETER: u32 = 6;

fn standard() -> std::path::PathBuf {
    // Deliberately not canonicalized: on Windows that yields a `\\?\` path,
    // which no editor sends.
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo = manifest.parent().and_then(std::path::Path::parent);
    repo.expect("repo root").join("lib").join("Standard")
}

/// A file of a mounted library is checked as its namespace (siblings and
/// `using`s loaded), so it has no spurious errors, and the bodies of its
/// generic types are highlighted even though nothing instantiates them.
#[test]
fn a_library_file_checks_clean_and_highlights_generic_bodies() {
    let (client, handle) = start_server();
    let lib = standard();
    initialize(
        &client,
        &serde_json::json!({ "libraries": { "Standard": lib.to_string_lossy() } }),
    );
    for name in [
        "box.lilac",
        "int32.lilac",
        "Heap/allocator.lilac",
        "Io/io.lilac",
        "Process/process.lilac",
        "C/io.lilac",
    ] {
        let path = lib.join(name);
        let text = std::fs::read_to_string(&path).expect("read");
        let uri = Url::from_file_path(&path).expect("uri");
        did_open(&client, &uri, &text);
        let published = publish_for(&client, &uri);
        assert!(
            published.diagnostics.is_empty(),
            "{name}: {:?}",
            published.diagnostics
        );
        if name == "box.lilac" {
            let tokens = decode(&semantic_tokens_at(&client, &uri));
            // `ptr` declared in the generic `create`, and `self` in `@on_drop`.
            let ptr = token_after(&tokens, &text, "ptr: ref T", 0);
            assert_eq!(ptr.token_type, VARIABLE, "{ptr:?}");
            let this = token_after(&tokens, &text, "self.value.get()", 0);
            assert_eq!(this.token_type, PARAMETER, "{this:?}");
        }
    }
    shutdown(&client);
    handle.join().expect("server thread");
}

/// Without the library mounted, a file still sees its own folder: `Bool` is
/// declared by a sibling of `int32.lilac`, so only the unmounted
/// `using *.Heap`-style imports could fail, never a sibling type.
#[test]
fn an_unmounted_library_file_still_sees_its_siblings() {
    let (client, handle) = start_server();
    initialize(&client, &serde_json::Value::Null);
    let path = standard().join("int32.lilac");
    let text = std::fs::read_to_string(&path).expect("read");
    let uri = Url::from_file_path(&path).expect("uri");
    did_open(&client, &uri, &text);
    let published = publish_for(&client, &uri);
    assert!(
        !published
            .diagnostics
            .iter()
            .any(|d| d.message.contains("unknown type `Bool`")),
        "{:?}",
        published.diagnostics
    );
    shutdown(&client);
    handle.join().expect("server thread");
}

/// Two open library files whose units overlap (`Standard` loads
/// `Standard.C`) share one compile, so an error is published once.
#[test]
fn overlapping_library_units_publish_an_error_once() {
    let (client, handle) = start_server();
    let lib = standard();
    initialize(
        &client,
        &serde_json::json!({ "libraries": { "Standard": lib.to_string_lossy() } }),
    );
    let io = lib.join("C/io.lilac");
    let broken = std::fs::read_to_string(&io).expect("read") + "\nbad: function () Nope {}\n";
    let io_uri = Url::from_file_path(&io).expect("uri");
    did_open(&client, &io_uri, &broken);
    let boxed = lib.join("box.lilac");
    let box_uri = Url::from_file_path(&boxed).expect("uri");
    did_open(
        &client,
        &box_uri,
        &std::fs::read_to_string(&boxed).expect("read"),
    );
    let published = publish_for(&client, &io_uri);
    let nope = published
        .diagnostics
        .iter()
        .filter(|d| d.message.contains("Nope"))
        .count();
    assert_eq!(nope, 1, "{:?}", published.diagnostics);
    shutdown(&client);
    handle.join().expect("server thread");
}

/// A client that supports it is asked to refresh semantic tokens after each
/// compile (its first request, on open, raced the compile).
#[test]
fn semantic_tokens_are_refreshed_after_a_compile() {
    let (client, handle) = start_server();
    initialize_with(
        &client,
        &serde_json::json!({ "workspace": { "semanticTokens": { "refreshSupport": true } } }),
        &serde_json::Value::Null,
    );
    let uri = Url::parse("file:///c:/refresh/main.lilac").expect("uri");
    did_open(&client, &uri, "main: function () nothing {\n}\n");
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Request(request))
                if request.method == "workspace/semanticTokens/refresh" =>
            {
                break;
            }
            Ok(_) => {}
            Err(err) => panic!("no refresh request: {err}"),
        }
    }
    shutdown(&client);
    handle.join().expect("server thread");
}

/// A mounted library loads whole, so an error in a library namespace no file
/// `using`s is published when a project file is compiled.
#[test]
fn an_unused_library_namespace_reports_its_errors() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("lsp_unused_library");
    let _ = std::fs::remove_dir_all(&dir);
    let bad = dir.join("lib").join("Unused").join("bad.lilac");
    std::fs::create_dir_all(bad.parent().expect("parent")).expect("mkdir");
    std::fs::write(&bad, "broken: function () Nope {}\n").expect("write");
    let main = dir.join("app").join("main.lilac");
    std::fs::create_dir_all(main.parent().expect("parent")).expect("mkdir");
    let text = "main: function () nothing {\n}\n";
    std::fs::write(&main, text).expect("write");

    let (client, handle) = start_server();
    initialize(
        &client,
        &serde_json::json!({ "libraries": { "Lib": dir.join("lib").to_string_lossy() } }),
    );
    did_open(&client, &Url::from_file_path(&main).expect("uri"), text);
    // The server spells unopened files' URIs canonically (a lowercase drive
    // letter on Windows), so match on the path.
    let published = loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Notification(notification))
                if notification.method == "textDocument/publishDiagnostics" =>
            {
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(notification.params).expect("decode");
                if params.uri.path().ends_with("/lib/Unused/bad.lilac") {
                    break params;
                }
            }
            Ok(_) => {}
            Err(err) => panic!("no diagnostics for {}: {err}", bad.display()),
        }
    };
    assert!(
        published
            .diagnostics
            .iter()
            .any(|d| d.message.contains("Nope")),
        "{:?}",
        published.diagnostics
    );
    shutdown(&client);
    handle.join().expect("server thread");
}

/// A `@compile_error` in a library function marks the user's call to it too,
/// not only the library line, and the two link to each other.
#[test]
fn a_library_compile_error_marks_the_users_call() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("lsp_trace_library");
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib").join("Guard.lilac");
    std::fs::create_dir_all(lib.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        &lib,
        "refuse: function () nothing {\n    @compile_error(\"not supported here\")\n}\n",
    )
    .expect("write");
    let main = dir.join("app").join("main.lilac");
    std::fs::create_dir_all(main.parent().expect("parent")).expect("mkdir");
    let text = "main: function () nothing {\n    Lib.refuse()\n}\n";
    std::fs::write(&main, text).expect("write");

    let (client, handle) = start_server();
    initialize(
        &client,
        &serde_json::json!({ "libraries": { "Lib": dir.join("lib").to_string_lossy() } }),
    );
    let uri = Url::from_file_path(&main).expect("uri");
    did_open(&client, &uri, text);
    let published = publish_for(&client, &uri);
    let site = published
        .diagnostics
        .iter()
        .find(|d| d.message.contains("not supported here"))
        .unwrap_or_else(|| panic!("{:?}", published.diagnostics));
    assert_eq!(site.range.start.line, 1, "{site:?}");
    assert_eq!(site.range.start.character, 4, "{site:?}");
    let back = site.related_information.as_ref().expect("links back");
    assert!(
        back[0].location.uri.path().ends_with("/lib/Guard.lilac"),
        "{back:?}"
    );
    shutdown(&client);
    handle.join().expect("server thread");
}

/// An `E042` in a library generic marks the user's instantiating call too.
#[test]
fn a_library_generic_failure_marks_the_users_call() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("lsp_trace_generic");
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib").join("Make.lilac");
    std::fs::create_dir_all(lib.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        &lib,
        "make[T]: function () out: T {\n    out = T.other()\n}\n",
    )
    .expect("write");
    let main = dir.join("app").join("main.lilac");
    std::fs::create_dir_all(main.parent().expect("parent")).expect("mkdir");
    let text = "main: function () nothing {\n    value: @bit32 = Lib.make[@bit32]()\n}\n";
    std::fs::write(&main, text).expect("write");

    let (client, handle) = start_server();
    initialize(
        &client,
        &serde_json::json!({ "libraries": { "Lib": dir.join("lib").to_string_lossy() } }),
    );
    let uri = Url::from_file_path(&main).expect("uri");
    did_open(&client, &uri, text);
    let published = publish_for(&client, &uri);
    let site = published
        .diagnostics
        .iter()
        .find(|d| d.message.starts_with("this call leads to an error"))
        .unwrap_or_else(|| panic!("{:?}", published.diagnostics));
    assert_eq!(site.range.start.line, 1, "{site:?}");
    assert_eq!(
        site.code,
        Some(lsp_types::NumberOrString::String("E042".to_owned()))
    );
    shutdown(&client);
    handle.join().expect("server thread");
}
