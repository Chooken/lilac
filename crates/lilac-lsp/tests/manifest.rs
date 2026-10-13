//! The workspace's `lilac.toml`: its settings reach every compile, and its
//! errors are published on it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    DidOpenTextDocumentParams, NumberOrString, PublishDiagnosticsParams, ShowMessageParams,
    TextDocumentItem, Url,
};

fn start_server() -> (Connection, std::thread::JoinHandle<()>) {
    let (server, client) = Connection::memory();
    let handle = std::thread::spawn(move || {
        let _ = lilac_lsp::run(server);
    });
    (client, handle)
}

fn request(client: &Connection, id: i32, method: &str, params: serde_json::Value) -> RequestId {
    let id = RequestId::from(id);
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: method.to_owned(),
            params,
        }))
        .expect("request");
    id
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

/// Initializes a server for the workspace `dir`.
fn initialize(client: &Connection, dir: &Path) {
    let folder = Url::from_directory_path(dir).expect("root");
    let id = request(
        client,
        1,
        "initialize",
        serde_json::json!({
            "capabilities": {},
            "workspaceFolders": [{ "uri": folder.as_str(), "name": "ws" }],
        }),
    );
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Response(response)) if response.id == id => break,
            Ok(_) => {}
            Err(err) => panic!("no initialize reply: {err}"),
        }
    }
    notify(client, "initialized", serde_json::Value::Null);
}

/// Shuts the server down and returns every message sent until the
/// shutdown reply.
fn shutdown(client: &Connection, handle: std::thread::JoinHandle<()>) -> Vec<Message> {
    let id = request(client, 2, "shutdown", serde_json::Value::Null);
    let mut messages = Vec::new();
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Response(response)) if response.id == id => break,
            Ok(message) => messages.push(message),
            Err(err) => panic!("no shutdown reply: {err}"),
        }
    }
    notify(client, "exit", serde_json::Value::Null);
    handle.join().expect("server thread");
    messages
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

/// Tells the server `path` changed on disk.
fn changed_on_disk(client: &Connection, path: &Path) {
    notify(
        client,
        "workspace/didChangeWatchedFiles",
        serde_json::json!({
            "changes": [{
                "uri": Url::from_file_path(path).expect("uri").as_str(),
                "type": 2,
            }],
        }),
    );
}

type Published = HashMap<String, PublishDiagnosticsParams>;

/// The last diagnostics published for each file (by name), read until
/// `done` holds and then until the server is quiet, so a compile's other
/// publishes are in and none is left for the next call.
fn publishes(client: &Connection, done: impl Fn(&Published) -> bool) -> Published {
    let mut found = Published::new();
    let mut quiet = false;
    loop {
        let wait = Duration::from_millis(if quiet { 500 } else { 10_000 });
        match client.receiver.recv_timeout(wait) {
            Ok(Message::Notification(notification))
                if notification.method == "textDocument/publishDiagnostics" =>
            {
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(notification.params).expect("decode");
                let name = params
                    .uri
                    .path()
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_owned();
                found.insert(name, params);
                quiet = done(&found);
            }
            Ok(_) => {}
            Err(_) if quiet => return found,
            Err(err) => panic!("not published ({found:?}): {err}"),
        }
    }
}

/// Whether `file` was published with exactly `expected` codes.
fn has(published: &Published, file: &str, expected: &[&str]) -> bool {
    published
        .get(file)
        .is_some_and(|params| codes(params) == expected)
}

fn codes(params: &PublishDiagnosticsParams) -> Vec<String> {
    params
        .diagnostics
        .iter()
        .filter_map(|diagnostic| match &diagnostic.code {
            Some(NumberOrString::String(code)) => Some(code.clone()),
            _ => None,
        })
        .collect()
}

/// A fresh, empty temporary folder for one test.
fn temp_project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lilac-lsp-toml-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

const MAIN: &str = "main: function () nothing {\n}\n";

/// A program using the library `Lib` (in `dir/lib`), which `main` needs.
fn with_library(dir: &Path) -> (Url, &'static str) {
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).expect("mkdir");
    std::fs::write(lib.join("lib.lilac"), "helper: function () nothing {\n}\n").expect("write");
    let text = "using Lib\n\nmain: function () nothing {\n    helper()\n}\n";
    let main = dir.join("main.lilac");
    std::fs::write(&main, text).expect("write");
    (Url::from_file_path(main).expect("uri"), text)
}

/// An unknown key is an `E076` at its range on `lilac.toml`, with its
/// did-you-mean, published before any document opens.
#[test]
fn an_unknown_key_is_published_on_lilac_toml() {
    let dir = temp_project("unknown-key");
    std::fs::write(dir.join("main.lilac"), MAIN).expect("write");
    std::fs::write(dir.join("lilac.toml"), "[build]\nmdoe = \"release\"\n").expect("write");

    let (client, handle) = start_server();
    initialize(&client, &dir);
    let published = publishes(&client, |found| has(found, "lilac.toml", &["E076"]));
    let diagnostic = &published["lilac.toml"].diagnostics[0];
    assert_eq!(diagnostic.range.start, lsp_types::Position::new(1, 0));
    assert_eq!(diagnostic.range.end, lsp_types::Position::new(1, 4));
    assert!(
        diagnostic.message.contains("`mode`"),
        "{}",
        diagnostic.message
    );

    shutdown(&client, handle);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Saving a change to `lilac.toml` recompiles with its settings.
#[test]
fn saving_lilac_toml_recompiles() {
    let dir = temp_project("save");
    let (main, text) = with_library(&dir);

    let (client, handle) = start_server();
    initialize(&client, &dir);
    did_open(&client, &main, text);
    publishes(&client, |found| has(found, "main.lilac", &["E025"]));

    let manifest = dir.join("lilac.toml");
    std::fs::write(&manifest, "[libraries]\nLib = \"lib\"\n").expect("write");
    changed_on_disk(&client, &manifest);
    let fixed = publishes(&client, |found| {
        has(found, "main.lilac", &[]) && has(found, "lilac.toml", &[])
    });
    assert!(has(&fixed, "main.lilac", &[]), "{fixed:?}");

    shutdown(&client, handle);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A syntax error (`E075`) in `lilac.toml` keeps the last good settings:
/// the library stays mounted.
#[test]
fn a_syntax_error_keeps_the_last_libraries() {
    let dir = temp_project("syntax");
    let (main, text) = with_library(&dir);
    let manifest = dir.join("lilac.toml");
    std::fs::write(&manifest, "[libraries]\nLib = \"lib\"\n").expect("write");

    let (client, handle) = start_server();
    initialize(&client, &dir);
    did_open(&client, &main, text);
    publishes(&client, |found| has(found, "main.lilac", &[]));

    std::fs::write(&manifest, "[libraries\nLib = \"lib\"\n").expect("write");
    changed_on_disk(&client, &manifest);
    let published = publishes(&client, |found| has(found, "lilac.toml", &["E075"]));
    assert!(has(&published, "main.lilac", &[]), "{published:?}");

    shutdown(&client, handle);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The manifest's target reaches the compile: an item only Linux has
/// checks clean for a Linux target.
#[test]
fn the_manifest_target_reaches_the_compile() {
    let dir = temp_project("target");
    let text = "if @builtin.Target.os == .Linux {\n    only_linux: function () nothing {}\n}\n\n\
                main: function () nothing {\n    only_linux()\n}\n";
    let main = dir.join("main.lilac");
    std::fs::write(&main, text).expect("write");
    std::fs::write(
        dir.join("lilac.toml"),
        "[build]\ntarget = \"x86_64-unknown-linux-gnu\"\n",
    )
    .expect("write");

    let (client, handle) = start_server();
    initialize(&client, &dir);
    did_open(&client, &Url::from_file_path(main).expect("uri"), text);
    let published = publishes(&client, |found| found.contains_key("main.lilac"));
    assert!(has(&published, "main.lilac", &[]), "{published:?}");

    shutdown(&client, handle);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A workspace with a `lilac.json` and no `lilac.toml` is told once that
/// `lilac.json` is no longer read.
#[test]
fn lilac_json_is_reported_once() {
    let dir = temp_project("json");
    std::fs::write(dir.join("main.lilac"), MAIN).expect("write");
    std::fs::write(
        dir.join("lilac.json"),
        r#"{ "libraries": { "Standard": "lib/Standard" } }"#,
    )
    .expect("write");

    let (client, handle) = start_server();
    initialize(&client, &dir);
    changed_on_disk(&client, &dir.join("lilac.toml"));
    let messages = shutdown(&client, handle);
    let shown: Vec<String> = messages
        .into_iter()
        .filter_map(|message| match message {
            Message::Notification(notification) if notification.method == "window/showMessage" => {
                serde_json::from_value::<ShowMessageParams>(notification.params)
                    .ok()
                    .map(|params| params.message)
            }
            _ => None,
        })
        .collect();
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert!(
        shown[0].contains("lilac.json") && shown[0].contains("lilac.toml"),
        "{}",
        shown[0]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
