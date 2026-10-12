//! Server state: documents, compile loop, and diagnostics publishing.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use lsp_server::{Connection, Message, Notification, Request, Response};
use lsp_types::{
    Diagnostic as LspDiagnostic, DidChangeConfigurationParams, DidChangeTextDocumentParams,
    DidChangeWatchedFilesParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    PublishDiagnosticsParams, Url,
};

use crate::compile::{self, Job, Outcome};
use crate::documents::Documents;
use crate::index::Retained;
use crate::roots::{self, canonical_file_uri};

/// Quiet time after the last change before recompiling. Requests (hover,
/// tokens) do not extend it.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// JSON-RPC code for an unknown method.
const METHOD_NOT_FOUND: i32 = -32601;

/// LSP code for a request the server understood but refuses (a rename of a
/// hook).
const REQUEST_FAILED: i32 = -32803;
/// JSON-RPC: the server failed (a request handler panicked).
const INTERNAL_ERROR: i32 = -32603;
/// LSP: the client cancelled the request.
const REQUEST_CANCELLED: i32 = -32800;

/// A request result as JSON (`null` for `None`).
macro_rules! value {
    ($result:expr $(,)?) => {
        $result
            .and_then(|result| serde_json::to_value(result).ok())
            .unwrap_or(serde_json::Value::Null)
    };
}

/// A request's params, or an early `null` reply when they are malformed.
macro_rules! params {
    ($request:expr, $ty:ty) => {
        match serde_json::from_value::<$ty>($request.params.clone()) {
            Ok(params) => params,
            Err(_) => return Some(Ok(serde_json::Value::Null)),
        }
    };
}

/// Why a request is not answered right away (see [`Server::parked`]).
enum Wait {
    /// A compile of the current text is due or running.
    Compile,
    /// No compile is coming.
    Never,
}

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
    /// no compile is due.
    deadline: Option<Instant>,
    /// Counts changes; a compile is current when it saw the latest.
    generation: u64,
    /// The cancel token of the compile on the worker (`None` when it is
    /// idle). One compile runs at a time: when the next is due, the running
    /// one is cancelled, and the next starts once it has stopped.
    running: Option<Arc<AtomicBool>>,
    /// Compile jobs for the worker thread.
    jobs: Sender<Job>,
    /// Finished compiles from the worker, by generation.
    results: Receiver<(u64, Outcome)>,
    /// The generation whose compiler panic the user was told about; cleared
    /// by the next compile that finishes, so a run of failing compiles says
    /// it once.
    reported_panic: Option<u64>,
    published: HashSet<Url>,
    shutdown: bool,
    /// Retained project compile (project mode).
    project: Option<Retained>,
    /// Compiles of open files the project does not reach, by canonical URI
    /// (files of one unit share a compile).
    per_file: HashMap<Url, Arc<Retained>>,
    /// The generation of the analysis the server holds (`None` before the
    /// first compile finishes).
    compiled: Option<u64>,
    /// Requests waiting for a compile, oldest first: completions after a
    /// `.` wait for one of the current text
    /// ([`crate::complete::needs_current_compile`]), and `lilac/lir`
    /// requests for one that kept its IR stages.
    parked: Vec<Request>,
    /// Whether compiles keep their IR stages, and whether the one held did.
    ir_view: IrView,
}

/// The IR view's state: compiles keep their IR stages
/// ([`Job::capture_lir`]) once the client has sent a `lilac/lir` request.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum IrView {
    /// The client never asked: no compile keeps its stages.
    Unused,
    /// Compiles keep their stages, but the one held did not (it started
    /// before the first request): `lilac/lir` waits for one that did.
    Waiting,
    /// The compile held kept its stages.
    Held,
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
        let (jobs, results) = compile::spawn_worker();
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
            generation: 0,
            running: None,
            jobs,
            results,
            reported_panic: None,
            published: HashSet::new(),
            shutdown: false,
            project: None,
            per_file: HashMap::new(),
            compiled: None,
            parked: Vec::new(),
            ir_view: IrView::Unused,
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
            // must not keep postponing the compile. Requests are answered
            // from the last compile while the worker runs the next.
            // A compile that is stopping after its cancel ends the wait.
            let due = match (self.deadline, &self.running) {
                (Some(_), Some(running)) if running.load(Ordering::Relaxed) => {
                    crossbeam_channel::never()
                }
                (Some(deadline), _) => crossbeam_channel::at(deadline),
                (None, _) => crossbeam_channel::never(),
            };
            let message = crossbeam_channel::select! {
                recv(self.connection.receiver) -> message => match message {
                    Ok(message) => message,
                    Err(_) => return Ok(()),
                },
                recv(self.results) -> result => {
                    if let Ok((generation, compiled)) = result {
                        self.finish_compile(generation, compiled);
                    }
                    continue;
                }
                recv(due) -> _ => {
                    self.compile_due();
                    continue;
                }
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
            return Ok(());
        }
        let wait = self
            .dot_completion(&request)
            .or_else(|| self.lir_wait(&request));
        match wait {
            Some(Wait::Compile) => {
                self.parked.push(request);
                self.compile_now();
                Ok(())
            }
            Some(Wait::Never) => {
                let response = Response::new_ok(request.id, serde_json::Value::Null);
                self.connection.sender.send(Message::Response(response))?;
                Ok(())
            }
            None => self.respond(request),
        }
    }

    /// For a completion after a `.` ([`crate::complete::needs_current_compile`])
    /// asked of text the analysis held has not seen: whether a compile of
    /// it is due or running, or will not come (the last one panicked, so
    /// it gets no answer). `None` for any other request, answered now.
    fn dot_completion(&self, request: &Request) -> Option<Wait> {
        if request.method != "textDocument/completion" || self.compiled == Some(self.generation) {
            return None;
        }
        let params =
            serde_json::from_value::<lsp_types::CompletionParams>(request.params.clone()).ok()?;
        let position = &params.text_document_position;
        let document = self.documents.get(&position.text_document.uri)?;
        if !crate::complete::needs_current_compile(&document.text, position.position) {
            return None;
        }
        Some(if self.running.is_some() || self.deadline.is_some() {
            Wait::Compile
        } else {
            Wait::Never
        })
    }

    /// For a `lilac/lir` request while the compile held kept no IR stages
    /// (they are kept only once the client asks): a compile that keeps them
    /// is due or running (the first request turns keeping on and makes one
    /// due now), or none is coming (the last one panicked). `None` when the
    /// stages are held, or for any other request: answered now.
    fn lir_wait(&mut self, request: &Request) -> Option<Wait> {
        if request.method != "lilac/lir" {
            return None;
        }
        match self.ir_view {
            IrView::Held => return None,
            IrView::Waiting => {}
            IrView::Unused => {
                self.ir_view = IrView::Waiting;
                // The compile held, and any running, keep no stages.
                self.deadline = Some(Instant::now());
            }
        }
        Some(if self.running.is_some() || self.deadline.is_some() {
            Wait::Compile
        } else {
            Wait::Never
        })
    }

    /// Starts the due compile now, skipping the debounce (once the running
    /// one, cancelled, has stopped).
    fn compile_now(&mut self) {
        if self.deadline.is_none() {
            return;
        }
        self.deadline = Some(Instant::now());
        self.compile_due();
    }

    /// The due compile's deadline passed: it starts, or, while an older
    /// compile runs, that one is cancelled so it starts when that stops.
    fn compile_due(&mut self) {
        match &self.running {
            Some(running) => running.store(true, Ordering::Relaxed),
            None => self.start_compile(),
        }
    }

    /// Answers `request` from the analysis held.
    fn respond(&self, request: Request) -> Result<(), Box<dyn Error + Send + Sync>> {
        // A handler that panics answers with an internal error, and the
        // server keeps running.
        let response = match compile::guarded(|| self.query_response(&request)) {
            Ok(Some(Ok(value))) => Response::new_ok(request.id, value),
            Ok(Some(Err(message))) => Response::new_err(request.id, REQUEST_FAILED, message),
            Ok(None) => {
                let method = request.method;
                Response::new_err(
                    request.id,
                    METHOD_NOT_FOUND,
                    format!("method not found: {method}"),
                )
            }
            Err(panic) => Response::new_err(
                request.id,
                INTERNAL_ERROR,
                format!("the Lilac language server hit an internal error: {panic}"),
            ),
        };
        self.connection.sender.send(Message::Response(response))?;
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

        let at = |position: &TextDocumentPositionParams| {
            let uri = &position.text_document.uri;
            Some((self.retained_for(uri)?, uri.clone(), position.position))
        };

        let result = match request.method.as_str() {
            "textDocument/hover" => {
                let params = params!(request, lsp_types::HoverParams);
                value!(at(&params.text_document_position_params).and_then(
                    |(retained, uri, position)| handlers::hover(retained, &uri, position)
                ))
            }
            "textDocument/definition" => {
                let params = params!(request, GotoDefinitionParams);
                value!(at(&params.text_document_position_params).and_then(
                    |(retained, uri, position)| handlers::definition(retained, &uri, position)
                ))
            }
            "textDocument/typeDefinition" => {
                let params = params!(request, GotoDefinitionParams);
                value!(at(&params.text_document_position_params).and_then(
                    |(retained, uri, position)| navigate::type_definition(retained, &uri, position)
                ))
            }
            "textDocument/references" => {
                let params = params!(request, ReferenceParams);
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
                let params = params!(request, DocumentHighlightParams);
                value!(at(&params.text_document_position_params).map(
                    |(retained, uri, position)| navigate::highlights(retained, &uri, position)
                ))
            }
            "textDocument/documentSymbol" => {
                let params = params!(request, DocumentSymbolParams);
                let uri = &params.text_document.uri;
                value!(self.retained_for(uri).map(|retained| {
                    DocumentSymbolResponse::Nested(navigate::document_symbols(retained, uri))
                }))
            }
            "workspace/symbol" => {
                let params = params!(request, WorkspaceSymbolParams);
                value!(Some(WorkspaceSymbolResponse::Nested(
                    self.workspace_symbols(&params.query),
                )))
            }
            "textDocument/prepareRename" | "textDocument/rename" => {
                return Some(self.rename_response(request));
            }
            "textDocument/semanticTokens/full" => {
                let params = params!(request, SemanticTokensParams);
                let uri = &params.text_document.uri;
                value!(self
                    .retained_for(uri)
                    .map(|retained| handlers::semantic_tokens(retained, uri)),)
            }
            "lilac/lir" => self.lir_response(request),
            _ => return self.editing_response(request),
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

    /// The replies to completion, signature help and quick fixes (`None`
    /// for another method).
    fn editing_response(&self, request: &Request) -> Option<Result<serde_json::Value, String>> {
        let result = match request.method.as_str() {
            "textDocument/completion" => {
                let params = params!(request, lsp_types::CompletionParams);
                let position = &params.text_document_position;
                let uri = &position.text_document.uri;
                value!(self.document_and_compile(uri).and_then(|(text, retained)| {
                    crate::complete::completion(retained, uri, text, position.position)
                }))
            }
            "textDocument/signatureHelp" => {
                let params = params!(request, lsp_types::SignatureHelpParams);
                let position = &params.text_document_position_params;
                let uri = &position.text_document.uri;
                value!(self.document_and_compile(uri).and_then(|(text, retained)| {
                    crate::complete::signature_help(retained, uri, text, position.position)
                }))
            }
            "textDocument/codeAction" => {
                let params = params!(request, lsp_types::CodeActionParams);
                value!(Some(crate::actions::code_actions(&params)))
            }
            _ => return None,
        };
        Some(Ok(result))
    }

    /// An open document's current text and the compile covering it.
    fn document_and_compile(&self, uri: &Url) -> Option<(&str, &Retained)> {
        let text = self.documents.get(uri)?.text.as_str();
        Some((text, self.retained_for(uri)?))
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
            // A parked completion the client gave up on; others are
            // answered too fast to cancel.
            "$/cancelRequest" => {
                if let Ok(params) =
                    notification.extract::<lsp_types::CancelParams>("$/cancelRequest")
                {
                    let id: lsp_server::RequestId = match params.id {
                        lsp_types::NumberOrString::Number(id) => id.into(),
                        lsp_types::NumberOrString::String(id) => id.into(),
                    };
                    if let Some(at) = self.parked.iter().position(|request| request.id == id) {
                        self.parked.remove(at);
                        let _ = self
                            .connection
                            .sender
                            .send(Message::Response(Response::new_err(
                                id,
                                REQUEST_CANCELLED,
                                "cancelled".to_owned(),
                            )));
                    }
                }
            }
            _ => {}
        }
    }

    /// Settles the parked requests after a compile finished: each is
    /// answered once the analysis held can (a `.` completion when it is of
    /// the current text, `lilac/lir` when it kept its IR stages); while a
    /// compile is due they wait for it, started now; with none coming (this
    /// one panicked) they get no answer.
    fn settle_parked(&mut self) {
        let mut waiting = Vec::new();
        for request in std::mem::take(&mut self.parked) {
            let ready = if request.method == "lilac/lir" {
                self.ir_view == IrView::Held
            } else {
                self.compiled == Some(self.generation)
            };
            if ready {
                let _ = self.respond(request);
            } else if self.deadline.is_some() || self.running.is_some() {
                waiting.push(request);
            } else {
                let response = Response::new_ok(request.id, serde_json::Value::Null);
                let _ = self.connection.sender.send(Message::Response(response));
            }
        }
        self.parked = waiting;
        if !self.parked.is_empty() {
            self.compile_now();
        }
    }

    /// Schedules a recompile [`DEBOUNCE`] from now; a compile already
    /// running is now stale, and is cancelled when that time comes.
    fn touch(&mut self) {
        self.generation += 1;
        self.deadline = Some(Instant::now() + DEBOUNCE);
    }

    /// Starts a compile of the current state on the worker.
    fn start_compile(&mut self) {
        self.deadline = None;
        let cancel = Arc::new(AtomicBool::new(false));
        let job = Job {
            generation: self.generation,
            documents: self.documents.clone(),
            workspace: self.workspace.clone(),
            root: self.root.clone(),
            libraries: self.libraries.clone(),
            cancel: Arc::clone(&cancel),
            capture_lir: self.ir_view != IrView::Unused,
        };
        if self.jobs.send(job).is_ok() {
            self.running = Some(cancel);
        }
    }

    /// Takes a finished compile: its analysis answers requests from now on
    /// (it is newer than what the server had), and its diagnostics are
    /// published unless an edit arrived meanwhile (the next compile, already
    /// scheduled, publishes instead). A compile that was cancelled changes
    /// nothing (the due one starts next). A compile that panicked changes
    /// nothing, but the user is told (once until a compile succeeds).
    fn finish_compile(&mut self, generation: u64, outcome: Outcome) {
        self.running = None;
        let compiled = match outcome {
            Outcome::Compiled(compiled) => *compiled,
            Outcome::Cancelled => {
                self.settle_parked();
                return;
            }
            Outcome::Panicked(panic) => {
                if self.reported_panic.replace(generation).is_none() {
                    self.show_error(&format!(
                        "The Lilac compiler hit an internal error; diagnostics may be stale. {panic}"
                    ));
                }
                self.settle_parked();
                return;
            }
        };
        self.reported_panic = None;
        self.compiled = Some(generation);
        self.project = compiled.project;
        self.per_file = compiled.per_file;
        if self.ir_view != IrView::Unused {
            self.ir_view = if compiled.captured_lir {
                IrView::Held
            } else {
                IrView::Waiting
            };
        }
        if generation == self.generation {
            let current: HashSet<Url> = compiled.diagnostics.keys().cloned().collect();
            for (uri, (diagnostics, version)) in compiled.diagnostics {
                self.send_diagnostics(&uri, diagnostics, version);
            }
            for uri in self.published.difference(&current) {
                self.send_diagnostics(uri, Vec::new(), None);
            }
            self.published = current;
        }
        self.settle_parked();

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

    /// Shows `message` to the user as an error (`window/showMessage`).
    fn show_error(&self, message: &str) {
        let params = lsp_types::ShowMessageParams {
            typ: lsp_types::MessageType::ERROR,
            message: message.to_owned(),
        };
        if let Ok(params) = serde_json::to_value(params) {
            let _ = self
                .connection
                .sender
                .send(Message::Notification(Notification {
                    method: "window/showMessage".to_owned(),
                    params,
                }));
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::Compiled;
    use lsp_types::{TextDocumentItem, VersionedTextDocumentIdentifier};

    fn open(server: &mut Server, uri: &Url, text: &str) {
        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: "lilac".to_owned(),
                version: 1,
                text: text.to_owned(),
            },
        };
        server.handle_notification(Notification {
            method: "textDocument/didOpen".to_owned(),
            params: serde_json::to_value(params).expect("params"),
        });
    }

    fn job(server: &Server) -> Job {
        Job {
            generation: server.generation,
            documents: server.documents.clone(),
            workspace: None,
            root: None,
            libraries: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            capture_lir: server.ir_view != IrView::Unused,
        }
    }

    fn published(client: &Connection) -> Vec<i32> {
        client
            .receiver
            .try_iter()
            .filter_map(|message| match message {
                Message::Notification(notification)
                    if notification.method == "textDocument/publishDiagnostics" =>
                {
                    serde_json::from_value::<PublishDiagnosticsParams>(notification.params)
                        .ok()
                        .and_then(|params| params.version)
                }
                _ => None,
            })
            .collect()
    }

    /// A compile that an edit overtook installs its analysis (newer than
    /// what the server had) but publishes nothing; the current one
    /// publishes.
    #[test]
    fn a_stale_compile_answers_requests_but_is_not_published() {
        let (connection, client) = Connection::memory();
        let mut server = Server::new(connection, None, Vec::new(), false);
        let dir = std::env::temp_dir().join("lilac-lsp-stale-compile");
        let uri = Url::from_file_path(dir.join("main.lilac")).expect("uri");
        open(
            &mut server,
            &uri,
            "main: function () nothing {\n    x: @bit8 = y\n}\n",
        );
        let stale = job(&server).run().expect("not cancelled");

        server.handle_notification(Notification {
            method: "textDocument/didChange".to_owned(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version: 2,
                },
                content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: "main: function () nothing {\n}\n".to_owned(),
                }],
            })
            .expect("params"),
        });
        server.finish_compile(stale.generation, Outcome::Compiled(Box::new(stale)));
        assert!(server.retained_for(&uri).is_some());
        assert_eq!(published(&client), Vec::<i32>::new());

        let current = job(&server).run().expect("not cancelled");
        server.finish_compile(current.generation, Outcome::Compiled(Box::new(current)));
        assert_eq!(published(&client), vec![2]);
    }

    fn shown_errors(client: &Connection) -> Vec<String> {
        client
            .receiver
            .try_iter()
            .filter_map(|message| match message {
                Message::Notification(notification)
                    if notification.method == "window/showMessage" =>
                {
                    serde_json::from_value::<lsp_types::ShowMessageParams>(notification.params)
                        .ok()
                        .map(|params| params.message)
                }
                _ => None,
            })
            .collect()
    }

    /// A compile that panicked is shown to the user once, with its message,
    /// until a compile succeeds again.
    #[test]
    fn a_compiler_panic_is_shown_once() {
        let (connection, client) = Connection::memory();
        let mut server = Server::new(connection, None, Vec::new(), false);
        let failed = compile::guarded(|| -> Compiled { panic!("boom") });
        server.finish_compile(0, Outcome::Panicked(failed.err().expect("panicked")));
        server.finish_compile(1, Outcome::Panicked("again".to_owned()));
        let shown = shown_errors(&client);
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert!(shown[0].contains("internal error") && shown[0].contains("boom"));

        let fine = job(&server).run().expect("not cancelled");
        server.finish_compile(fine.generation, Outcome::Compiled(Box::new(fine)));
        server.finish_compile(2, Outcome::Panicked("later".to_owned()));
        let shown = shown_errors(&client);
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert!(shown[0].contains("later"));
    }

    fn change(server: &mut Server, uri: &Url, version: i32, text: &str) {
        server.handle_notification(Notification {
            method: "textDocument/didChange".to_owned(),
            params: serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: uri.clone(),
                    version,
                },
                content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: text.to_owned(),
                }],
            })
            .expect("params"),
        });
    }

    fn complete_request(uri: &Url, line: u32, character: u32) -> Request {
        Request {
            id: "complete".to_owned().into(),
            method: "textDocument/completion".to_owned(),
            params: serde_json::json!({
                "textDocument": { "uri": uri.as_str() },
                "position": { "line": line, "character": character },
            }),
        }
    }

    fn responses(client: &Connection) -> Vec<Response> {
        client
            .receiver
            .try_iter()
            .filter_map(|message| match message {
                Message::Response(response) => Some(response),
                _ => None,
            })
            .collect()
    }

    const SHAPES: &str = "Shape: enum {\n    Dot\n    Line\n}\nPaint: enum {\n    Red\n    Blue\n}\nmain: function () nothing {\n    s: Shape = .\n}\n";

    /// A `.` completion waits for a compile of the text it was asked
    /// about; an edit while it waits makes it wait for the next compile,
    /// which answers it.
    #[test]
    fn a_parked_completion_is_answered_from_the_newest_compile() {
        let (connection, client) = Connection::memory();
        let mut server = Server::new(connection, None, Vec::new(), false);
        let dir = std::env::temp_dir().join("lilac-lsp-parked-completion");
        let uri = Url::from_file_path(dir.join("main.lilac")).expect("uri");
        open(&mut server, &uri, SHAPES);
        let stale = job(&server).run().expect("not cancelled");
        server
            .handle_request(complete_request(&uri, 9, 16))
            .expect("request");
        assert_eq!(server.parked.len(), 1);
        change(
            &mut server,
            &uri,
            2,
            &SHAPES.replace("s: Shape", "s: Paint"),
        );

        server.finish_compile(stale.generation, Outcome::Compiled(Box::new(stale)));
        assert!(responses(&client).is_empty());
        assert_eq!(server.parked.len(), 1);

        let current = job(&server).run().expect("not cancelled");
        server.finish_compile(current.generation, Outcome::Compiled(Box::new(current)));
        let answered = responses(&client);
        let [response] = answered.as_slice() else {
            panic!("one response: {answered:?}");
        };
        let items: Vec<lsp_types::CompletionItem> =
            serde_json::from_value(response.response_result.clone().expect("result"))
                .expect("items");
        let mut labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        labels.sort_unstable();
        assert_eq!(labels, ["Blue", "Red"]);
        assert!(server.parked.is_empty());
    }

    fn lir_request(uri: &Url) -> Request {
        Request {
            id: "lir".to_owned().into(),
            method: "lilac/lir".to_owned(),
            params: serde_json::json!({ "uri": uri.as_str(), "stage": "final" }),
        }
    }

    /// The IR text of the one response the client got.
    fn lir_answer(client: &Connection) -> String {
        let answered = responses(client);
        let [response] = answered.as_slice() else {
            panic!("one response: {answered:?}");
        };
        let result = response.response_result.clone().expect("result");
        result["text"].as_str().expect("text").to_owned()
    }

    /// Compiles keep no IR until the client asks for it: its first
    /// `lilac/lir` request waits for a compile that keeps it, and every
    /// compile after keeps it too.
    #[test]
    fn the_ir_is_kept_once_the_client_asks_for_it() {
        let (connection, client) = Connection::memory();
        let mut server = Server::new(connection, None, Vec::new(), false);
        let dir = std::env::temp_dir().join("lilac-lsp-ir-on-demand");
        let uri = Url::from_file_path(dir.join("main.lilac")).expect("uri");
        open(&mut server, &uri, "main: function () nothing {\n}\n");
        let plain = job(&server).run().expect("not cancelled");
        server.finish_compile(plain.generation, Outcome::Compiled(Box::new(plain)));
        let held = server.retained_for(&uri).expect("retained");
        assert!(held.stages.modules.is_empty());

        server.handle_request(lir_request(&uri)).expect("request");
        assert_eq!(server.parked.len(), 1);
        assert!(responses(&client).is_empty());
        let kept = job(&server).run().expect("not cancelled");
        server.finish_compile(kept.generation, Outcome::Compiled(Box::new(kept)));
        assert!(lir_answer(&client).contains("function @main"));
        let held = server.retained_for(&uri).expect("retained");
        let stages: Vec<&str> = held.stages.modules.iter().map(|(name, _)| *name).collect();
        assert_eq!(stages, ["lower", "runtime", "final"]);

        change(
            &mut server,
            &uri,
            2,
            "main: function () nothing {\n    x: @bit16 = 0x1\n}\n",
        );
        let next = job(&server).run().expect("not cancelled");
        server.finish_compile(next.generation, Outcome::Compiled(Box::new(next)));
        server.handle_request(lir_request(&uri)).expect("request");
        assert!(server.parked.is_empty());
        assert!(lir_answer(&client).contains("#bit16"));
    }

    /// A parked completion the client cancels is answered as cancelled.
    #[test]
    fn a_parked_completion_can_be_cancelled() {
        let (connection, client) = Connection::memory();
        let mut server = Server::new(connection, None, Vec::new(), false);
        let dir = std::env::temp_dir().join("lilac-lsp-cancelled-completion");
        let uri = Url::from_file_path(dir.join("main.lilac")).expect("uri");
        open(&mut server, &uri, SHAPES);
        server
            .handle_request(complete_request(&uri, 9, 16))
            .expect("request");
        server.handle_notification(Notification {
            method: "$/cancelRequest".to_owned(),
            params: serde_json::json!({ "id": "complete" }),
        });
        assert!(server.parked.is_empty());
        let answered = responses(&client);
        let [response] = answered.as_slice() else {
            panic!("one response: {answered:?}");
        };
        assert_eq!(
            response
                .response_result
                .as_ref()
                .err()
                .map(|error| error.code),
            Some(REQUEST_CANCELLED)
        );
    }

    /// A cancelled compile installs and publishes nothing: the previous
    /// analysis keeps answering requests. A job whose token is set stops.
    #[test]
    fn a_cancelled_compile_changes_nothing() {
        let (connection, client) = Connection::memory();
        let mut server = Server::new(connection, None, Vec::new(), false);
        let dir = std::env::temp_dir().join("lilac-lsp-cancelled-compile");
        let uri = Url::from_file_path(dir.join("main.lilac")).expect("uri");
        open(&mut server, &uri, "main: function () nothing {\n}\n");
        let first = job(&server).run().expect("not cancelled");
        let first_generation = first.generation;
        server.finish_compile(first_generation, Outcome::Compiled(Box::new(first)));
        assert_eq!(published(&client), vec![1]);

        change(
            &mut server,
            &uri,
            2,
            "main: function () nothing {\n    x: @bit8 = y\n}\n",
        );
        let cancelled = job(&server);
        cancelled.cancel.store(true, Ordering::Relaxed);
        assert!(cancelled.run().is_none());
        server.finish_compile(cancelled.generation, Outcome::Cancelled);
        assert_eq!(published(&client), Vec::<i32>::new());
        assert_eq!(server.compiled, Some(first_generation));
        assert!(server.retained_for(&uri).is_some());
    }

    /// When the next compile is due while one runs, the running one is
    /// cancelled rather than waited for.
    #[test]
    fn a_due_compile_cancels_the_running_one() {
        let (connection, _client) = Connection::memory();
        let mut server = Server::new(connection, None, Vec::new(), false);
        let dir = std::env::temp_dir().join("lilac-lsp-due-compile");
        let uri = Url::from_file_path(dir.join("main.lilac")).expect("uri");
        open(&mut server, &uri, "main: function () nothing {\n}\n");
        server.start_compile();
        let running = server.running.clone().expect("a compile runs");
        change(&mut server, &uri, 2, "main: function () nothing {\n\n}\n");
        assert!(!running.load(Ordering::Relaxed));
        server.compile_due();
        assert!(running.load(Ordering::Relaxed));
        assert!(server.deadline.is_some(), "the due compile still waits");
    }

    #[test]
    fn guarded_turns_a_panic_into_its_message() {
        let formatted = 7;
        assert_eq!(compile::guarded(|| 3), Ok(3));
        assert_eq!(
            compile::guarded(|| -> u8 { panic!("static") }),
            Err("static".to_owned())
        );
        assert_eq!(
            compile::guarded(|| -> u8 { panic!("value {formatted}") }),
            Err("value 7".to_owned())
        );
    }
}
