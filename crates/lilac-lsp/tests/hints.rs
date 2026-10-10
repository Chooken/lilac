//! Faded hints: unused names and unreachable statements, published as
//! `Hint` + `Unnecessary` beside the compiler's diagnostics.

use std::time::Duration;

use lsp_server::{Connection, Message, Notification, Request, RequestId};
use lsp_types::{
    DiagnosticSeverity, DiagnosticTag, DidOpenTextDocumentParams, PublishDiagnosticsParams,
    TextDocumentItem, Url,
};

fn start_server() -> (Connection, std::thread::JoinHandle<()>) {
    let (server, client) = Connection::memory();
    let handle = std::thread::spawn(move || {
        let _ = lilac_lsp::run(server);
    });
    (client, handle)
}

fn request(client: &Connection, id: i32, method: &str, params: serde_json::Value) {
    let id = RequestId::from(id);
    client
        .sender
        .send(Message::Request(Request {
            id: id.clone(),
            method: method.to_owned(),
            params,
        }))
        .expect("request");
    loop {
        match client.receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Message::Response(response)) if response.id == id => return,
            Ok(_) => {}
            Err(err) => panic!("no response for {id:?}: {err}"),
        }
    }
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

/// Starts a server (on `root` when given), opens `uri` with `text`, and
/// returns what it publishes for it, then shuts it down.
fn published(root: Option<&Url>, uri: &Url, text: &str) -> PublishDiagnosticsParams {
    let (client, handle) = start_server();
    let params = match root {
        Some(root) => serde_json::json!({ "capabilities": {}, "rootUri": root.as_str() }),
        None => serde_json::json!({ "capabilities": {} }),
    };
    request(&client, 1, "initialize", params);
    notify(&client, "initialized", serde_json::Value::Null);
    let open = DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: uri.clone(),
            language_id: "lilac".to_owned(),
            version: 1,
            text: text.to_owned(),
        },
    };
    notify(
        &client,
        "textDocument/didOpen",
        serde_json::to_value(open).expect("params"),
    );
    let published = loop {
        match client.receiver.recv_timeout(Duration::from_secs(10)) {
            Ok(Message::Notification(notification))
                if notification.method == "textDocument/publishDiagnostics" =>
            {
                let params: PublishDiagnosticsParams =
                    serde_json::from_value(notification.params).expect("params");
                if params.uri == *uri {
                    break params;
                }
            }
            Ok(_) => {}
            Err(err) => panic!("no diagnostics: {err}"),
        }
    };
    request(&client, 2, "shutdown", serde_json::Value::Null);
    notify(&client, "exit", serde_json::Value::Null);
    handle.join().expect("server");
    published
}

/// The hints in `published` as `(line, message)`, after checking each is
/// a faded hint.
fn hints(published: &PublishDiagnosticsParams) -> Vec<(u32, String)> {
    published
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::HINT))
        .map(|diagnostic| {
            assert_eq!(
                diagnostic.tags.as_deref(),
                Some(&[DiagnosticTag::UNNECESSARY][..])
            );
            (diagnostic.range.start.line, diagnostic.message.clone())
        })
        .collect()
}

const PROGRAM: &str = "\
private {
    unused_helper: function () nothing {
    }
    used_helper: function (unused_param: @bit32, _ignored: @bit32) r: @bit32 {
        r = 0x1
    }
}

Counter: object {
    count: @bit32
    @on_drop: function (self) nothing {
    }
}

main: function () nothing {
    unused: @bit32 = 0x1
    used: @bit32 = 0x2
    used = used_helper(used, 0x3)
    used == 0x2
    return
    later: @bit32 = 0x4
    @trap()
}
";

#[test]
fn unused_names_and_unreachable_statements_are_faded() {
    let uri = Url::parse("file:///c:/hints/main.lilac").expect("uri");
    let published = published(None, &uri, PROGRAM);
    assert_eq!(
        hints(&published),
        vec![
            (1, "`unused_helper` is never used".to_owned()),
            (3, "parameter `unused_param` is never used".to_owned()),
            (15, "local `unused` is never used".to_owned()),
            (18, "this value is never used".to_owned()),
            (20, "unreachable code".to_owned()),
        ],
        "{:?}",
        published.diagnostics
    );
    assert_eq!(
        published.diagnostics.len(),
        5,
        "{:?}",
        published.diagnostics
    );
}

#[test]
fn an_unused_alias_is_faded() {
    let dir = std::env::temp_dir().join("lilac-lsp-hints-alias");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("geometry")).expect("mkdir");
    std::fs::write(
        dir.join("geometry").join("geo.lilac"),
        "area: function () r: @bit32 {\n    r = 0x1\n}\n",
    )
    .expect("write");
    let text = "using geometry as Geo\nusing geometry as Used\n\
                main: function () nothing {\n    x: @bit32 = Used.area()\n    y: @bit32 = x\n}\n";
    let main = dir.join("main.lilac");
    std::fs::write(&main, text).expect("write");
    let root = Url::from_directory_path(&dir).expect("root");
    let uri = Url::from_file_path(&main).expect("uri");
    let published = published(Some(&root), &uri, text);
    assert_eq!(
        hints(&published),
        vec![
            (0, "alias `Geo` is never used".to_owned()),
            (4, "local `y` is never used".to_owned()),
        ],
        "{:?}",
        published.diagnostics
    );
}

/// A scope with an error gets no hints: what the checker skipped after
/// the error may be what uses its names.
#[test]
fn a_scope_with_an_error_gets_no_hints() {
    let uri = Url::parse("file:///c:/hints/broken.lilac").expect("uri");
    let text = "main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit32 = missing\n}\n";
    let published = published(None, &uri, text);
    assert_eq!(hints(&published), Vec::new(), "{:?}", published.diagnostics);
    assert_eq!(
        published.diagnostics.len(),
        1,
        "{:?}",
        published.diagnostics
    );
}
