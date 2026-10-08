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
use crate::documents::{Document, Documents};
use crate::fs::OverlayFs;
use crate::index::{LirStages, Retained};
use crate::roots::canonical_file_uri;

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
    /// Retained project compile (project mode).
    project: Option<Retained>,
    /// Retained per-document compiles (single-file mode).
    per_file: HashMap<Url, Retained>,
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
            project: None,
            per_file: HashMap::new(),
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
        } else if request.method == "textDocument/hover" {
            let result = self.hover_response(&request);
            self.connection
                .sender
                .send(Message::Response(Response::new_ok(request.id, result)))?;
        } else if request.method == "textDocument/definition" {
            let result = self.definition_response(&request);
            self.connection
                .sender
                .send(Message::Response(Response::new_ok(request.id, result)))?;
        } else if request.method == "lilac/lir" {
            let result = self.lir_response(&request);
            self.connection
                .sender
                .send(Message::Response(Response::new_ok(request.id, result)))?;
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

    /// The `textDocument/hover` reply: a hover object, or `null`.
    fn hover_response(&self, request: &Request) -> serde_json::Value {
        let Ok(params) = serde_json::from_value::<lsp_types::HoverParams>(request.params.clone())
        else {
            return serde_json::Value::Null;
        };
        let position = params.text_document_position_params.position;
        let uri = &params.text_document_position_params.text_document.uri;
        let retained = self.per_file.get(uri).or(self.project.as_ref());
        let Some(retained) = retained else {
            return serde_json::Value::Null;
        };
        match crate::handlers::hover(retained, uri, position) {
            Some(hover) => serde_json::to_value(hover).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    }

    /// The `textDocument/definition` reply: a location, or `null`.
    fn definition_response(&self, request: &Request) -> serde_json::Value {
        let Ok(params) =
            serde_json::from_value::<lsp_types::GotoDefinitionParams>(request.params.clone())
        else {
            return serde_json::Value::Null;
        };
        let position = params.text_document_position_params.position;
        let uri = &params.text_document_position_params.text_document.uri;
        let retained = self.per_file.get(uri).or(self.project.as_ref());
        let Some(retained) = retained else {
            return serde_json::Value::Null;
        };
        match crate::handlers::definition(retained, uri, position) {
            Some(response) => serde_json::to_value(response).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    }

    /// The `lilac/lir` reply: a stage payload, or `null`.
    fn lir_response(&self, request: &Request) -> serde_json::Value {
        let params = &request.params;
        let Some(uri) = params
            .get("uri")
            .and_then(serde_json::Value::as_str)
            .and_then(|uri| Url::parse(uri).ok())
        else {
            return serde_json::Value::Null;
        };
        let Some(stage) = params.get("stage").and_then(serde_json::Value::as_str) else {
            return serde_json::Value::Null;
        };
        let retained = self.per_file.get(&uri).or(self.project.as_ref());
        let Some(retained) = retained else {
            return serde_json::Value::Null;
        };
        crate::lir::lir_payload(retained, &uri, stage).unwrap_or(serde_json::Value::Null)
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
        let mut new_project = None;
        let mut new_per_file: HashMap<Url, Retained> = HashMap::new();

        if let Some(root) = self.root.clone() {
            let mut session = new_session();
            let mut fs = OverlayFs { overlay: &overlay };
            let mut stages = LirStages::default();
            let _ = session.compile_project_with_stages(&root, &mut fs, &mut |name, lir| {
                stages.modules.push((name, lir.clone()));
            });
            let file_uris: HashMap<FileId, Url> = session
                .project_files()
                .iter()
                .filter_map(|(id, path)| Url::from_file_path(path).ok().map(|uri| (*id, uri)))
                .collect();
            // Project-load errors (E025) can have no source span; attach those
            // to the first open document so they are not dropped silently.
            let fallback = self.documents.iter().next().map(|(uri, _)| uri.clone());
            for diagnostic in session.diags.diagnostics() {
                let target = primary_file(diagnostic)
                    .and_then(|id| file_uris.get(&id).cloned())
                    .or_else(|| fallback.clone());
                let Some(uri) = target else {
                    continue;
                };
                if let Some(lsp) = to_lsp_diagnostic(diagnostic, &session.source_map, &|file| {
                    file_uris.get(&file).cloned()
                }) {
                    by_uri.entry(uri).or_default().push(lsp);
                }
            }
            let uri_files = file_uris
                .iter()
                .map(|(id, uri)| (uri.clone(), *id))
                .collect();
            let project_uris: HashSet<Url> = file_uris.values().cloned().collect();
            new_project = Some(Retained::build_with_stages(
                session, file_uris, uri_files, stages,
            ));

            // Fallback: an open `.lilac` file that is not part of the project
            // (e.g. the workspace folder is not the project root) is compiled
            // standalone so diagnostics, hover, definition, and the IR view
            // still work for it.
            for (uri, document) in self.documents.iter() {
                if project_uris.contains(uri) || project_uris.contains(&canonical_file_uri(uri)) {
                    continue;
                }
                compile_standalone(uri, document, &mut by_uri, &mut new_per_file);
            }
        } else {
            for (uri, document) in self.documents.iter() {
                compile_standalone(uri, document, &mut by_uri, &mut new_per_file);
            }
        }

        // Publish open documents even when clean, so stale sets are cleared.
        for (uri, _) in self.documents.iter() {
            by_uri.entry(uri.clone()).or_default();
        }

        if generation != self.generation {
            return;
        }

        self.project = new_project;
        self.per_file = new_per_file;

        let current: HashSet<Url> = by_uri.keys().cloned().collect();
        for (uri, diagnostics) in &by_uri {
            self.send_diagnostics(uri, diagnostics.clone());
        }
        for uri in self.published.difference(&current) {
            self.send_diagnostics(uri, Vec::new());
        }
        self.published = current;

        let _ = self
            .connection
            .sender
            .send(Message::Notification(Notification {
                method: "lilac/lirInvalidated".to_owned(),
                params: serde_json::Value::Null,
            }));
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

/// A compiler session for editor use: the CLI's `max_errors` cap (10) is for
/// terminals, but an editor wants the full diagnostic list, so lift it here.
/// The session also analyzes tolerantly so hover/definition keep working while
/// the buffer has syntax or type errors.
fn new_session() -> Session {
    let mut session = Session::new();
    session.diags.max_errors = usize::MAX;
    session.retain_partial_analysis = true;
    session
}

/// Compiles one open document standalone (single-file mode) and records its
/// diagnostics and retained compile, so hover/definition/IR work for it even
/// when it is not part of the project.
fn compile_standalone(
    uri: &Url,
    document: &Document,
    by_uri: &mut HashMap<Url, Vec<LspDiagnostic>>,
    per_file: &mut HashMap<Url, Retained>,
) {
    let mut session = new_session();
    let name = document.path.to_string_lossy().into_owned();
    let mut stages = LirStages::default();
    let _ = session.compile_lir_with_stages(&name, &document.text, &mut |name, lir| {
        stages.modules.push((name, lir.clone()));
    });
    let entry = by_uri.entry(uri.clone()).or_default();
    for diagnostic in session.diags.diagnostics() {
        if let Some(lsp) = to_lsp_diagnostic(diagnostic, &session.source_map, &|file| {
            (file == FileId(0)).then(|| uri.clone())
        }) {
            entry.push(lsp);
        }
    }
    let file_uris = HashMap::from([(FileId(0), uri.clone())]);
    let uri_files = HashMap::from([(uri.clone(), FileId(0))]);
    per_file.insert(
        uri.clone(),
        Retained::build_with_stages(session, file_uris, uri_files, stages),
    );
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
