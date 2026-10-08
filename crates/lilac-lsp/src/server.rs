//! Server state: documents, compile loop, and diagnostics publishing.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::rc::Rc;
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
    /// Mounted libraries (`lilac.json` and `lilac.libraries`), passed to
    /// every session.
    libraries: Vec<(String, PathBuf)>,
    /// Whether the client accepts `workspace/semanticTokens/refresh`.
    refresh_tokens: bool,
    /// Id of the next server-to-client request.
    next_request: i32,
    dirty: bool,
    generation: u64,
    published: HashSet<Url>,
    shutdown: bool,
    /// Retained project compile (project mode).
    project: Option<Retained>,
    /// Compiles of open files the project does not reach, by canonical URI
    /// (files of one unit share a compile).
    per_file: HashMap<Url, Rc<Retained>>,
}

impl Server {
    /// New server over `connection`, rooted at `root` (project mode) or `None`,
    /// with `libraries` mounted. With `refresh_tokens`, the client is asked to
    /// re-request semantic tokens after every compile.
    #[must_use]
    pub fn new(
        connection: Connection,
        root: Option<PathBuf>,
        libraries: Vec<(String, PathBuf)>,
        refresh_tokens: bool,
    ) -> Self {
        Self {
            connection,
            documents: Documents::new(),
            root,
            libraries,
            refresh_tokens,
            next_request: 0,
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
        } else if request.method == "textDocument/semanticTokens/full" {
            let result = self.semantic_tokens_response(&request);
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

    /// The compile that covers `uri`: its own unit's, else the project's.
    fn retained_for(&self, uri: &Url) -> Option<&Retained> {
        self.per_file
            .get(&canonical_file_uri(uri))
            .map(|retained| &**retained)
            .or(self.project.as_ref())
    }

    /// The `textDocument/hover` reply: a hover object, or `null`.
    fn hover_response(&self, request: &Request) -> serde_json::Value {
        let Ok(params) = serde_json::from_value::<lsp_types::HoverParams>(request.params.clone())
        else {
            return serde_json::Value::Null;
        };
        let position = params.text_document_position_params.position;
        let uri = &params.text_document_position_params.text_document.uri;
        let retained = self.retained_for(uri);
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
        let retained = self.retained_for(uri);
        let Some(retained) = retained else {
            return serde_json::Value::Null;
        };
        match crate::handlers::definition(retained, uri, position) {
            Some(response) => serde_json::to_value(response).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        }
    }

    /// The `textDocument/semanticTokens/full` reply: encoded tokens, or `null`.
    fn semantic_tokens_response(&self, request: &Request) -> serde_json::Value {
        let Ok(params) =
            serde_json::from_value::<lsp_types::SemanticTokensParams>(request.params.clone())
        else {
            return serde_json::Value::Null;
        };
        let uri = &params.text_document.uri;
        let retained = self.retained_for(uri);
        let Some(retained) = retained else {
            return serde_json::Value::Null;
        };
        let tokens = crate::handlers::semantic_tokens(retained, uri);
        serde_json::to_value(tokens).unwrap_or(serde_json::Value::Null)
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
        let retained = self.retained_for(&uri);
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

        let mut covered: HashSet<Url> = HashSet::new();
        if let Some(root) = self.root.clone() {
            let mut session = new_session(&self.libraries);
            let mut fs = OverlayFs { overlay: &overlay };
            let mut stages = LirStages::default();
            let _ = session.compile_project_with_stages(&root, &mut fs, &mut |name, lir| {
                stages.modules.push((name, lir.clone()));
            });
            // Project-load errors (E025) can have no source span; attach those
            // to the first open document so they are not dropped silently.
            let fallback = self.documents.iter().next().map(|(uri, _)| uri.clone());
            new_project = Some(retain(
                session,
                stages,
                fallback.as_ref(),
                &mut covered,
                &mut by_uri,
            ));
        }

        let new_per_file = self.compile_units(&overlay, &mut covered, &mut by_uri);

        // Key everything by the client's spelling of an open document's URI
        // (`file:///c%3A/...`), so one document never gets two diagnostic
        // sets that overwrite each other.
        let open: HashMap<Url, Url> = self
            .documents
            .iter()
            .map(|(uri, _)| (canonical_file_uri(uri), uri.clone()))
            .collect();
        let mut by_uri: HashMap<Url, Vec<LspDiagnostic>> =
            by_uri
                .into_iter()
                .fold(HashMap::new(), |mut acc, (uri, diagnostics)| {
                    let uri = open.get(&canonical_file_uri(&uri)).cloned().unwrap_or(uri);
                    acc.entry(uri).or_default().extend(diagnostics);
                    acc
                });

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

        // Tokens the client asked for before this compile (on open, or
        // mid-edit) came from no analysis or a stale one; ask again.
        if self.refresh_tokens {
            self.next_request += 1;
            let _ = self.connection.sender.send(Message::Request(Request {
                id: format!("lilac-refresh-{}", self.next_request).into(),
                method: "workspace/semanticTokens/refresh".to_owned(),
                params: serde_json::Value::Null,
            }));
        }
    }

    /// Compiles the open files the project did not cover (`covered`), one
    /// compile per unit, and returns each file's compile by canonical URI.
    fn compile_units(
        &self,
        overlay: &HashMap<PathBuf, String>,
        covered: &mut HashSet<Url>,
        by_uri: &mut HashMap<Url, Vec<LspDiagnostic>>,
    ) -> HashMap<Url, Rc<Retained>> {
        // An open file the project does not reach is checked on its own unit
        // (see `unit_of`). Files sharing a unit share one compile, and a
        // compile also covers the open files of every unit it loads (a
        // library's root namespace loads its subfolders), so outer units go
        // first and covered files are neither recompiled nor re-published.
        let mut new_per_file: HashMap<Url, Rc<Retained>> = HashMap::new();
        let mut units: HashMap<Unit, Vec<Url>> = HashMap::new();
        for (uri, document) in self.documents.iter() {
            if covered.contains(&canonical_file_uri(uri)) {
                continue;
            }
            units
                .entry(unit_of(&document.path, &document.text, &self.libraries))
                .or_default()
                .push(uri.clone());
        }
        let mut units: Vec<(Unit, Vec<Url>)> = units.into_iter().collect();
        units.sort_by_key(|(unit, _)| unit.order());
        for (unit, uris) in units {
            if uris
                .iter()
                .all(|uri| new_per_file.contains_key(&canonical_file_uri(uri)))
            {
                continue;
            }
            let mut session = new_session(&self.libraries);
            let mut stages = LirStages::default();
            let mut capture = |name, lir: &lilac_ir::LirModule| {
                stages.modules.push((name, lir.clone()));
            };
            let mut fs = OverlayFs { overlay };
            let retained = Rc::new(match &unit {
                Unit::Namespace(namespace) => {
                    let root = self.root.clone().unwrap_or_default();
                    let _ = session.compile_namespace_with_stages(
                        &root,
                        namespace,
                        &mut fs,
                        &mut capture,
                    );
                    retain(session, stages, uris.first(), covered, by_uri)
                }
                Unit::Folder(dir) => {
                    let _ = session.compile_project_with_stages(dir, &mut fs, &mut capture);
                    retain(session, stages, uris.first(), covered, by_uri)
                }
                Unit::File(_) => {
                    let uri = &uris[0];
                    let Some(document) = self.documents.get(uri) else {
                        continue;
                    };
                    let name = document.path.to_string_lossy().into_owned();
                    let _ = session.compile_lir_with_stages(&name, &document.text, &mut capture);
                    retain_single(session, stages, uri, covered, by_uri)
                }
            });
            for uri in retained.uri_files.keys().chain(&uris) {
                new_per_file
                    .entry(canonical_file_uri(uri))
                    .or_insert_with(|| Rc::clone(&retained));
            }
        }
        new_per_file
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
fn new_session(libraries: &[(String, PathBuf)]) -> Session {
    let mut session = Session::new();
    session.libraries = libraries.to_vec();
    session.diags.max_errors = usize::MAX;
    session.retain_partial_analysis = true;
    session
}

/// What an open file outside the project is compiled as.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Unit {
    /// A mounted library namespace (`Standard.Heap`).
    Namespace(String),
    /// A program folder (one with a `main.lilac`).
    Folder(PathBuf),
    /// The file alone (single-file mode).
    File(PathBuf),
}

impl Unit {
    /// Compile order: outer library namespaces first (they load their
    /// subfolders), then folders, then single files.
    fn order(&self) -> (usize, usize, Self) {
        match self {
            Self::Namespace(path) => (0, path.matches('.').count(), self.clone()),
            Self::Folder(_) => (1, 0, self.clone()),
            Self::File(_) => (2, 0, self.clone()),
        }
    }
}

/// The unit an open file belongs to: its namespace when it lies in a mounted
/// library; its folder when the folder is a program (has a `main.lilac`) or
/// the file declares no `main` (library-style code that needs its siblings);
/// else the file alone (a loose program next to other programs).
fn unit_of(path: &Path, text: &str, libraries: &[(String, PathBuf)]) -> Unit {
    let Some(dir) = path.parent() else {
        return Unit::File(path.to_path_buf());
    };
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let dir_canonical = canonical(dir);
    for (name, lib) in libraries {
        if let Ok(rest) = dir_canonical.strip_prefix(canonical(lib)) {
            let mut namespace = name.clone();
            for segment in rest.components() {
                namespace.push('.');
                namespace.push_str(&segment.as_os_str().to_string_lossy());
            }
            return Unit::Namespace(namespace);
        }
    }
    let declares_main = text.lines().any(|line| line.starts_with("main:"));
    if dir.join("main.lilac").is_file() || !declares_main {
        return Unit::Folder(dir.to_path_buf());
    }
    Unit::File(path.to_path_buf())
}

/// Publishes a finished multi-file compile's diagnostics into `by_uri` and
/// retains it. Diagnostics without a file go to `fallback`. Files already in
/// `covered` (published by an earlier compile) are skipped; this compile's
/// files are added to it.
fn retain(
    session: Session,
    stages: LirStages,
    fallback: Option<&Url>,
    covered: &mut HashSet<Url>,
    by_uri: &mut HashMap<Url, Vec<LspDiagnostic>>,
) -> Retained {
    let file_uris: HashMap<FileId, Url> = session
        .project_files()
        .iter()
        .filter_map(|(id, path)| {
            Url::from_file_path(path)
                .ok()
                .map(|uri| (*id, canonical_file_uri(&uri)))
        })
        .collect();
    for diagnostic in session.diags.diagnostics() {
        let target = primary_file(diagnostic)
            .and_then(|id| file_uris.get(&id).cloned())
            .or_else(|| fallback.cloned());
        let Some(uri) = target else {
            continue;
        };
        if covered.contains(&canonical_file_uri(&uri)) {
            continue;
        }
        if let Some(lsp) = to_lsp_diagnostic(diagnostic, &session.source_map, &|file| {
            file_uris.get(&file).cloned()
        }) {
            by_uri.entry(uri).or_default().push(lsp);
        }
    }
    covered.extend(file_uris.values().cloned());
    let uri_files = file_uris
        .iter()
        .map(|(id, uri)| (uri.clone(), *id))
        .collect();
    Retained::build_with_stages(session, file_uris, uri_files, stages)
}

/// Like [`retain`] for a single-file compile of the document at `uri`.
fn retain_single(
    session: Session,
    stages: LirStages,
    uri: &Url,
    covered: &mut HashSet<Url>,
    by_uri: &mut HashMap<Url, Vec<LspDiagnostic>>,
) -> Retained {
    covered.insert(canonical_file_uri(uri));
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
    Retained::build_with_stages(session, file_uris, uri_files, stages)
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
