//! Server state: documents, compile loop, and diagnostics publishing.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use lilac_driver::Session;
use lilac_error::Diagnostic;
use lilac_span::FileId;
use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    Diagnostic as LspDiagnostic, DidChangeConfigurationParams, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    PublishDiagnosticsParams, Url,
};

use crate::convert::{to_lsp_diagnostic, trace_diagnostics};
use crate::documents::Documents;
use crate::fs::OverlayFs;
use crate::index::{LirStages, Retained};
use crate::roots::{self, canonical_file_uri};

/// Quiet time after the last change before recompiling. Requests (hover,
/// tokens) do not extend it.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// JSON-RPC code for an unknown method.
const METHOD_NOT_FOUND: i32 = -32601;

/// LSP code for a request the server understood but refuses (a rename of a
/// hook).
const REQUEST_FAILED: i32 = -32803;

/// Language-server state.
pub struct Server {
    connection: Connection,
    documents: Documents,
    /// The workspace folder (holds `lilac.json`).
    workspace: Option<PathBuf>,
    /// The `lilac.libraries` setting.
    setting_libraries: Vec<(String, PathBuf)>,
    /// The project root ([`roots::project_root`] of the workspace).
    root: Option<PathBuf>,
    /// Mounted libraries (`lilac.json` and `lilac.libraries`), passed to
    /// every session.
    libraries: Vec<(String, PathBuf)>,
    /// Whether the client accepts `workspace/semanticTokens/refresh`.
    refresh_tokens: bool,
    /// Id of the next server-to-client request.
    next_request: i32,
    /// When to recompile: [`DEBOUNCE`] after the last change, `None` when
    /// the published diagnostics are current.
    deadline: Option<Instant>,
    published: HashSet<Url>,
    shutdown: bool,
    /// Retained project compile (project mode).
    project: Option<Retained>,
    /// Compiles of open files the project does not reach, by canonical URI
    /// (files of one unit share a compile).
    per_file: HashMap<Url, Rc<Retained>>,
}

impl Server {
    /// New server over `connection` for `workspace` (project mode) or `None`,
    /// with the `lilac.libraries` setting `setting_libraries` (merged over
    /// `lilac.json`'s). With `refresh_tokens`, the client is asked to
    /// re-request semantic tokens after every compile.
    #[must_use]
    pub fn new(
        connection: Connection,
        workspace: Option<PathBuf>,
        setting_libraries: Vec<(String, PathBuf)>,
        refresh_tokens: bool,
    ) -> Self {
        let mut server = Self {
            connection,
            documents: Documents::new(),
            workspace,
            setting_libraries,
            root: None,
            libraries: Vec::new(),
            refresh_tokens,
            next_request: 0,
            deadline: None,
            published: HashSet::new(),
            shutdown: false,
            project: None,
            per_file: HashMap::new(),
        };
        server.configure();
        server
    }

    /// Re-reads `lilac.json`: the project root and the mounted libraries.
    fn configure(&mut self) {
        let workspace = self.workspace.as_deref();
        self.libraries = roots::merge_libraries(
            workspace.map_or_else(Vec::new, roots::config_libraries),
            self.setting_libraries.clone(),
        );
        self.root = workspace.map(roots::project_root);
    }

    /// Asks the client to report changes to `.lilac` files and
    /// `lilac.json` (`workspace/didChangeWatchedFiles`), for a client that
    /// supports registering watchers.
    pub fn watch_files(&mut self) {
        self.next_request += 1;
        let _ = self.connection.sender.send(Message::Request(Request {
            id: format!("lilac-watch-{}", self.next_request).into(),
            method: "client/registerCapability".to_owned(),
            params: serde_json::json!({
                "registrations": [{
                    "id": "lilac-watched-files",
                    "method": "workspace/didChangeWatchedFiles",
                    "registerOptions": {
                        "watchers": [
                            { "globPattern": "**/*.lilac" },
                            { "globPattern": "**/lilac.json" },
                        ],
                    },
                }],
            }),
        }));
    }

    /// Runs until the client sends `exit`.
    ///
    /// Returns `Ok(())` when `exit` follows `shutdown`, and an error when
    /// `exit` arrives first (the client violated the protocol).
    pub fn main_loop(&mut self) -> Result<(), Box<dyn Error + Send + Sync>> {
        loop {
            // A deadline, not a timeout per receive: a stream of requests
            // must not keep postponing the compile.
            let message = match self.deadline {
                Some(deadline) if deadline <= Instant::now() => {
                    self.publish();
                    continue;
                }
                Some(deadline) => match self
                    .connection
                    .receiver
                    .recv_timeout(deadline - Instant::now())
                {
                    Ok(message) => message,
                    Err(err) if err.is_timeout() => continue,
                    Err(_) => return Ok(()),
                },
                None => match self.connection.receiver.recv() {
                    Ok(message) => message,
                    Err(_) => return Ok(()),
                },
            };
            match message {
                Message::Request(request) => self.handle_request(request)?,
                Message::Notification(notification) => {
                    if notification.method == "exit" {
                        return if self.shutdown {
                            Ok(())
                        } else {
                            Err("exit received before shutdown".into())
                        };
                    }
                    self.handle_notification(notification);
                }
                Message::Response(_) => {}
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
        } else if let Some(result) = self.query_response(&request) {
            let response = match result {
                Ok(value) => Response::new_ok(request.id, value),
                Err(message) => Response::new_err(request.id, REQUEST_FAILED, message),
            };
            self.connection.sender.send(Message::Response(response))?;
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

    /// The reply to an editor query (`None` for another method): a result,
    /// `null` when nothing is under the cursor (or the params are malformed),
    /// or a refusal.
    fn query_response(&self, request: &Request) -> Option<Result<serde_json::Value, String>> {
        use lsp_types::{
            DocumentHighlightParams, DocumentSymbolParams, DocumentSymbolResponse,
            GotoDefinitionParams, ReferenceParams, SemanticTokensParams,
            TextDocumentPositionParams, WorkspaceSymbolParams, WorkspaceSymbolResponse,
        };

        use crate::{handlers, navigate};

        macro_rules! value {
            ($result:expr $(,)?) => {
                $result
                    .and_then(|result| serde_json::to_value(result).ok())
                    .unwrap_or(serde_json::Value::Null)
            };
        }
        macro_rules! params {
            ($ty:ty) => {
                match serde_json::from_value::<$ty>(request.params.clone()) {
                    Ok(params) => params,
                    Err(_) => return Some(Ok(serde_json::Value::Null)),
                }
            };
        }
        let at = |position: &TextDocumentPositionParams| {
            let uri = &position.text_document.uri;
            Some((self.retained_for(uri)?, uri.clone(), position.position))
        };

        let result = match request.method.as_str() {
            "textDocument/hover" => {
                let params = params!(lsp_types::HoverParams);
                value!(at(&params.text_document_position_params).and_then(
                    |(retained, uri, position)| handlers::hover(retained, &uri, position)
                ))
            }
            "textDocument/definition" => {
                let params = params!(GotoDefinitionParams);
                value!(at(&params.text_document_position_params).and_then(
                    |(retained, uri, position)| handlers::definition(retained, &uri, position)
                ))
            }
            "textDocument/typeDefinition" => {
                let params = params!(GotoDefinitionParams);
                value!(at(&params.text_document_position_params).and_then(
                    |(retained, uri, position)| navigate::type_definition(retained, &uri, position)
                ))
            }
            "textDocument/references" => {
                let params = params!(ReferenceParams);
                value!(
                    at(&params.text_document_position).map(|(retained, uri, position)| {
                        navigate::references(
                            retained,
                            &uri,
                            position,
                            params.context.include_declaration,
                        )
                    })
                )
            }
            "textDocument/documentHighlight" => {
                let params = params!(DocumentHighlightParams);
                value!(at(&params.text_document_position_params).map(
                    |(retained, uri, position)| navigate::highlights(retained, &uri, position)
                ))
            }
            "textDocument/documentSymbol" => {
                let params = params!(DocumentSymbolParams);
                let uri = &params.text_document.uri;
                value!(self.retained_for(uri).map(|retained| {
                    DocumentSymbolResponse::Nested(navigate::document_symbols(retained, uri))
                }))
            }
            "workspace/symbol" => {
                let params = params!(WorkspaceSymbolParams);
                value!(Some(WorkspaceSymbolResponse::Nested(
                    self.workspace_symbols(&params.query),
                )))
            }
            "textDocument/prepareRename" | "textDocument/rename" => {
                return Some(self.rename_response(request));
            }
            "textDocument/semanticTokens/full" => {
                let params = params!(SemanticTokensParams);
                let uri = &params.text_document.uri;
                value!(self
                    .retained_for(uri)
                    .map(|retained| handlers::semantic_tokens(retained, uri)),)
            }
            "lilac/lir" => self.lir_response(request),
            _ => return None,
        };
        Some(Ok(result))
    }

    /// The `textDocument/prepareRename` and `textDocument/rename` replies:
    /// the name's range or the edit, `null` when nothing is under the
    /// cursor, or why the name cannot be renamed.
    fn rename_response(&self, request: &Request) -> Result<serde_json::Value, String> {
        let null = serde_json::Value::Null;
        let refused = |refusal: String| format!("cannot rename: {refusal}");
        let result = if request.method == "textDocument/prepareRename" {
            let Ok(params) = serde_json::from_value::<lsp_types::TextDocumentPositionParams>(
                request.params.clone(),
            ) else {
                return Ok(null);
            };
            let uri = &params.text_document.uri;
            let Some(retained) = self.retained_for(uri) else {
                return Ok(null);
            };
            crate::navigate::prepare_rename(retained, uri, params.position, &self.libraries)
                .map_err(refused)?
                .and_then(|range| serde_json::to_value(range).ok())
        } else {
            let Ok(params) =
                serde_json::from_value::<lsp_types::RenameParams>(request.params.clone())
            else {
                return Ok(null);
            };
            let position = params.text_document_position;
            let uri = &position.text_document.uri;
            let Some(retained) = self.retained_for(uri) else {
                return Ok(null);
            };
            crate::navigate::rename(
                retained,
                uri,
                position.position,
                &params.new_name,
                &self.libraries,
            )
            .map_err(refused)?
            .and_then(|edit| serde_json::to_value(edit).ok())
        };
        Ok(result.unwrap_or(null))
    }

    /// Symbols matching `query` from every retained compile, each once.
    fn workspace_symbols(&self, query: &str) -> Vec<lsp_types::WorkspaceSymbol> {
        let mut seen = HashSet::new();
        let compiles = self
            .project
            .iter()
            .chain(self.per_file.values().map(|retained| &**retained));
        let mut symbols = Vec::new();
        for retained in compiles {
            for symbol in crate::navigate::workspace_symbols(retained, query) {
                let lsp_types::OneOf::Left(location) = &symbol.location else {
                    continue;
                };
                let key = (
                    canonical_file_uri(&location.uri),
                    location.range.start.line,
                    location.range.start.character,
                );
                if seen.insert(key) {
                    symbols.push(symbol);
                }
            }
        }
        symbols.sort_by(|a, b| a.name.cmp(&b.name));
        symbols
    }

    /// The compile that covers `uri`: its own unit's, else the project's.
    fn retained_for(&self, uri: &Url) -> Option<&Retained> {
        self.per_file
            .get(&canonical_file_uri(uri))
            .map(|retained| &**retained)
            .or(self.project.as_ref())
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
            // Files changed on disk (new, deleted, `git checkout`): files
            // are read afresh on every compile, so recompiling is enough,
            // plus re-reading `lilac.json` when it changed.
            "workspace/didChangeWatchedFiles" => {
                if let Ok(params) = notification
                    .extract::<DidChangeWatchedFilesParams>("workspace/didChangeWatchedFiles")
                {
                    let config = params.changes.iter().any(|change| {
                        roots::url_to_path(&change.uri)
                            .is_some_and(|path| path.file_name() == Some("lilac.json".as_ref()))
                    });
                    if config {
                        self.configure();
                    }
                    self.touch();
                }
            }
            // `{ "lilac": { "libraries": { ... } } }`; other settings are
            // the client's.
            "workspace/didChangeConfiguration" => {
                if let Ok(params) = notification
                    .extract::<DidChangeConfigurationParams>("workspace/didChangeConfiguration")
                {
                    if let Some(libraries) = params
                        .settings
                        .get("lilac")
                        .and_then(|lilac| lilac.get("libraries"))
                    {
                        self.setting_libraries =
                            roots::parse_libraries(libraries, self.workspace.as_deref());
                        self.configure();
                        self.touch();
                    }
                }
            }
            _ => {}
        }
    }

    /// Schedules a recompile [`DEBOUNCE`] from now.
    fn touch(&mut self) {
        self.deadline = Some(Instant::now() + DEBOUNCE);
    }

    fn publish(&mut self) {
        self.deadline = None;
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
            // Project-load errors (E025) can have no source span.
            let anchor = self.project_anchor(&root);
            new_project = Some(retain(
                session,
                stages,
                anchor.as_ref(),
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

        self.project = new_project;
        self.per_file = new_per_file;

        let current: HashSet<Url> = by_uri.keys().cloned().collect();
        for (uri, diagnostics) in by_uri {
            let version = self.documents.get(&uri).map(|document| document.version);
            self.send_diagnostics(&uri, diagnostics, version);
        }
        for uri in self.published.difference(&current) {
            self.send_diagnostics(uri, Vec::new(), None);
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

    /// Where the project's diagnostics without a source span go: the
    /// workspace's `lilac.json`, else the root's `main.lilac`, else the
    /// first open document under the root (by URI, so the choice is stable).
    fn project_anchor(&self, root: &Path) -> Option<Url> {
        let config = self
            .workspace
            .as_deref()
            .map(|workspace| workspace.join("lilac.json"))
            .filter(|config| config.is_file());
        if let Some(config) = config {
            return Url::from_file_path(config).ok();
        }
        let open: Vec<&Url> = self
            .documents
            .iter()
            .filter(|(_, document)| document.path.starts_with(root))
            .map(|(uri, _)| uri)
            .collect();
        self.unit_anchor(root, &open)
    }

    /// Where a unit's diagnostics without a source span go: `dir`'s
    /// `main.lilac` when it exists or is open, else the first of `uris`.
    fn unit_anchor(&self, dir: &Path, uris: &[&Url]) -> Option<Url> {
        let main = dir.join("main.lilac");
        let main_open = self
            .documents
            .iter()
            .any(|(_, document)| document.path == main);
        if main.is_file() || main_open {
            return Url::from_file_path(main).ok();
        }
        uris.iter().min().map(|uri| (*uri).clone())
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
                    let anchor = uris.iter().min();
                    retain(session, stages, anchor, covered, by_uri)
                }
                Unit::Folder(dir) => {
                    let _ = session.compile_project_with_stages(dir, &mut fs, &mut capture);
                    let anchor = self.unit_anchor(dir, &uris.iter().collect::<Vec<_>>());
                    retain(session, stages, anchor.as_ref(), covered, by_uri)
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

    /// Publishes `diagnostics` for `uri`, computed from the open
    /// document's `version` (`None` for a file that is not open).
    fn send_diagnostics(&self, uri: &Url, diagnostics: Vec<LspDiagnostic>, version: Option<i32>) {
        let params = PublishDiagnosticsParams {
            uri: uri.clone(),
            diagnostics,
            version,
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
/// retains it. Diagnostics without a file go to `anchor`. Files already in
/// `covered` (published by an earlier compile) are skipped; this compile's
/// files are added to it.
fn retain(
    session: Session,
    stages: LirStages,
    anchor: Option<&Url>,
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
    let uri_of = |file| file_uris.get(&file).cloned();
    for diagnostic in session.diags.diagnostics() {
        let target = primary_file(diagnostic)
            .and_then(uri_of)
            .or_else(|| anchor.cloned());
        if let Some(uri) = target.filter(|uri| !covered.contains(&canonical_file_uri(uri))) {
            if let Some(lsp) = to_lsp_diagnostic(diagnostic, &session.source_map, &uri_of) {
                by_uri.entry(uri).or_default().push(lsp);
            }
        }
        for (uri, lsp) in trace_diagnostics(diagnostic, &session.source_map, &uri_of) {
            if !covered.contains(&canonical_file_uri(&uri)) {
                by_uri.entry(uri).or_default().push(lsp);
            }
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
    let uri_of = |file| (file == FileId(0)).then(|| uri.clone());
    for diagnostic in session.diags.diagnostics() {
        if let Some(lsp) = to_lsp_diagnostic(diagnostic, &session.source_map, &uri_of) {
            entry.push(lsp);
        }
        let sites = trace_diagnostics(diagnostic, &session.source_map, &uri_of);
        entry.extend(sites.into_iter().map(|(_, lsp)| lsp));
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
