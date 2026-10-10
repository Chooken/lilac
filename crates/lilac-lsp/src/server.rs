//! Server state: documents, compile loop, and diagnostics publishing.

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::path::PathBuf;
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
    /// Whether the worker is compiling (one compile runs at a time; a due
    /// one starts when it finishes).
    compiling: bool,
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
            compiling: false,
            jobs,
            results,
            reported_panic: None,
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
            // must not keep postponing the compile. Requests are answered
            // from the last compile while the worker runs the next.
            let due = match self.deadline {
                Some(_) if self.compiling => crossbeam_channel::never(),
                Some(deadline) => crossbeam_channel::at(deadline),
                None => crossbeam_channel::never(),
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
                    self.start_compile();
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
            _ => {}
        }
    }

    /// Schedules a recompile [`DEBOUNCE`] from now; a compile already
    /// running is now stale.
    fn touch(&mut self) {
        self.generation += 1;
        self.deadline = Some(Instant::now() + DEBOUNCE);
    }

    /// Starts a compile of the current state on the worker.
    fn start_compile(&mut self) {
        self.deadline = None;
        let job = Job {
            generation: self.generation,
            documents: self.documents.clone(),
            workspace: self.workspace.clone(),
            root: self.root.clone(),
            libraries: self.libraries.clone(),
        };
        if self.jobs.send(job).is_ok() {
            self.compiling = true;
        }
    }

    /// Takes a finished compile: its analysis answers requests from now on
    /// (it is newer than what the server had), and its diagnostics are
    /// published unless an edit arrived meanwhile (the next compile, already
    /// scheduled, publishes instead). A compile that panicked changes
    /// nothing, but the user is told (once until a compile succeeds).
    fn finish_compile(&mut self, generation: u64, compiled: Outcome) {
        self.compiling = false;
        let compiled = match compiled {
            Ok(compiled) => compiled,
            Err(panic) => {
                if self.reported_panic.replace(generation).is_none() {
                    self.show_error(&format!(
                        "The Lilac compiler hit an internal error; diagnostics may be stale. {panic}"
                    ));
                }
                return;
            }
        };
        self.reported_panic = None;
        self.project = compiled.project;
        self.per_file = compiled.per_file;
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
        let stale = job(&server).run();

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
        server.finish_compile(stale.generation, Ok(stale));
        assert!(server.retained_for(&uri).is_some());
        assert_eq!(published(&client), Vec::<i32>::new());

        let current = job(&server).run();
        server.finish_compile(current.generation, Ok(current));
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
        server.finish_compile(0, failed);
        server.finish_compile(1, Err("again".to_owned()));
        let shown = shown_errors(&client);
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert!(shown[0].contains("internal error") && shown[0].contains("boom"));

        let fine = job(&server).run();
        server.finish_compile(fine.generation, Ok(fine));
        server.finish_compile(2, Err("later".to_owned()));
        let shown = shown_errors(&client);
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert!(shown[0].contains("later"));
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
