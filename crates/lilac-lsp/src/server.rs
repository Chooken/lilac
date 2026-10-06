//! Server state: documents, compile loop, and diagnostics publishing.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::PathBuf;
use std::time::Duration;

use lilac_driver::Session;
use lilac_error::Diagnostic;
use lilac_span::FileId;
use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    Diagnostic as LspDiagnostic, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, PublishDiagnosticsParams, Url,
};

use crate::convert::to_lsp_diagnostic;
use crate::documents::Documents;
use crate::fs::OverlayFs;

/// Debounce window before recompiling after an edit.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// JSON-RPC code for an unknown method.
const METHOD_NOT_FOUND: i32 = -32601;

/// Language-server state.
pub struct Server {
    connection: Connection,
    documents: Documents,
    root: Option<PathBuf>,
    dirty: bool,
    generation: u64,
    published: HashSet<Url>,
    shutdown: bool,
}

impl Server {
    /// New server over `connection`, rooted at `root` (project mode) or `None`.
    #[must_use]
    pub fn new(connection: Connection, root: Option<PathBuf>) -> Self {
        Self {
            connection,
            documents: Documents::new(),
            root,
            dirty: false,
            generation: 0,
            published: HashSet::new(),
            shutdown: false,
        }
    }

    /// Runs until the client sends `exit`.
    ///
    /// Returns `Ok(())` when `exit` follows `shutdown`, and an error when
    /// `exit` arrives first (the client violated the protocol).
    pub fn main_loop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        loop {
            match self.connection.receiver.recv_timeout(DEBOUNCE) {
                Ok(Message::Request(request)) => self.handle_request(request)?,
                Ok(Message::Notification(notification)) => {
                    if notification.method == "exit" {
                        return if self.shutdown {
                            Ok(())
                        } else {
                            Err("exit received before shutdown".into())
                        };
                    }
                    self.handle_notification(notification);
                }
                Ok(Message::Response(_)) => {}
                Err(err) => {
                    if err.is_timeout() {
                        if self.dirty {
                            self.publish();
                        }
                    } else {
                        return Ok(());
                    }
                }
            }
        }
    }

    fn handle_request(&mut self, request: Request) -> Result<(), Box<dyn Error + Send + Sync>> {
        if request.method == "shutdown" {
            self.shutdown = true;
            self.connection
                .sender
                .send(Message::Response(Response::new_ok(
                    request.id,
                    serde_json::Value::Null,
                )))?;
        } else {
            let method = request.method;
            self.connection
                .sender
                .send(Message::Response(Response::new_err(
                    request.id,
                    METHOD_NOT_FOUND,
                    format!("method not found: {method}"),
                )))?;
        }
        Ok(())
    }

    fn handle_notification(&mut self, notification: Notification) {
        let method = notification.method.clone();
        match method.as_str() {
            "textDocument/didOpen" => {
                if let Ok(params) =
                    notification.extract::<DidOpenTextDocumentParams>("textDocument/didOpen")
                {
                    self.documents.open(&params.text_document);
                    self.touch();
                }
            }
            "textDocument/didChange" => {
                if let Ok(params) =
                    notification.extract::<DidChangeTextDocumentParams>("textDocument/didChange")
                {
                    self.documents.change(&params);
                    self.touch();
                }
            }
            "textDocument/didClose" => {
                if let Ok(params) =
                    notification.extract::<DidCloseTextDocumentParams>("textDocument/didClose")
                {
                    self.documents.close(&params.text_document.uri);
                    self.touch();
                }
            }
            _ => {}
        }
    }

    fn touch(&mut self) {
        self.dirty = true;
        self.generation += 1;
    }

    fn publish(&mut self) {
        self.dirty = false;
        let generation = self.generation;
        let overlay = self.documents.overlay();
        let mut by_uri: HashMap<Url, Vec<LspDiagnostic>> = HashMap::new();

        if let Some(root) = self.root.clone() {
            let mut session = Session::new();
            let mut fs = OverlayFs { overlay: &overlay };
            let _ = session.compile_project(&root, &mut fs);
            let file_uris: HashMap<FileId, Url> = session
                .project_files()
                .iter()
                .filter_map(|(id, path)| Url::from_file_path(path).ok().map(|uri| (*id, uri)))
                .collect();
            for diagnostic in session.diags.diagnostics() {
                let Some(id) = primary_file(diagnostic) else {
                    continue;
                };
                let Some(uri) = file_uris.get(&id).cloned() else {
                    continue;
                };
                if let Some(lsp) = to_lsp_diagnostic(diagnostic, &session.source_map, &|file| {
                    file_uris.get(&file).cloned()
                }) {
                    by_uri.entry(uri).or_default().push(lsp);
                }
            }
        } else {
            for (uri, document) in self.documents.iter() {
                let mut session = Session::new();
                let name = document.path.to_string_lossy().into_owned();
                let _ = session.compile_lir(&name, &document.text);
                let entry = by_uri.entry(uri.clone()).or_default();
                for diagnostic in session.diags.diagnostics() {
                    if let Some(lsp) = to_lsp_diagnostic(diagnostic, &session.source_map, &|file| {
                        (file == FileId(0)).then(|| uri.clone())
                    }) {
                        entry.push(lsp);
                    }
                }
            }
        }

        // Publish open documents even when clean, so stale sets are cleared.
        for (uri, _) in self.documents.iter() {
            by_uri.entry(uri.clone()).or_default();
        }

        if generation != self.generation {
            return;
        }

        let current: HashSet<Url> = by_uri.keys().cloned().collect();
        for (uri, diagnostics) in &by_uri {
            self.send_diagnostics(uri, diagnostics.clone());
        }
        for uri in self.published.difference(&current) {
            self.send_diagnostics(uri, Vec::new());
        }
        self.published = current;
    }

    fn send_diagnostics(&self, uri: &Url, diagnostics: Vec<LspDiagnostic>) {
        let params = PublishDiagnosticsParams {
            uri: uri.clone(),
            diagnostics,
            version: None,
        };
        if let Ok(params) = serde_json::to_value(params) {
            let _ = self
                .connection
                .sender
                .send(Message::Notification(Notification {
                    method: "textDocument/publishDiagnostics".to_owned(),
                    params,
                }));
        }
    }
}

/// The file of a diagnostic's primary label (else its first label).
fn primary_file(diagnostic: &Diagnostic) -> Option<FileId> {
    diagnostic
        .labels
        .iter()
        .find(|label| label.primary)
        .or_else(|| diagnostic.labels.first())
        .map(|label| label.span.file)
}
