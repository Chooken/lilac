//! Compiling the workspace off the main thread: a [`Job`] snapshots what a
//! compile reads (documents, root, libraries), runs on the worker
//! ([`spawn_worker`]), and comes back as [`Compiled`]: the retained
//! analyses and the diagnostics to publish.

use std::collections::{HashMap, HashSet};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use lilac_driver::Session;
use lilac_error::Diagnostic;
use lilac_span::FileId;
use lsp_types::{Diagnostic as LspDiagnostic, Url};

use crate::convert::{to_lsp_diagnostic, trace_diagnostics};
use crate::documents::Documents;
use crate::fs::OverlayFs;
use crate::index::{LirStages, Retained};
use crate::roots::canonical_file_uri;

/// One compile's inputs, snapshotted when it starts so edits that arrive
/// meanwhile do not affect it.
pub struct Job {
    /// The edit generation the snapshot was taken at.
    pub generation: u64,
    /// The open documents.
    pub documents: Documents,
    /// The workspace folder (holds `lilac.json`).
    pub workspace: Option<PathBuf>,
    /// The project root.
    pub root: Option<PathBuf>,
    /// Mounted libraries.
    pub libraries: Vec<(String, PathBuf)>,
}

/// A finished compile.
pub struct Compiled {
    /// The [`Job::generation`] it was compiled from.
    pub generation: u64,
    /// The project compile (project mode).
    pub project: Option<Retained>,
    /// Compiles of open files the project does not reach, by canonical URI.
    pub per_file: HashMap<Url, Arc<Retained>>,
    /// Diagnostics by URI (an open document under the client's spelling),
    /// with the document version they were computed from. Every open
    /// document has an entry, so clean ones are cleared.
    pub diagnostics: HashMap<Url, (Vec<LspDiagnostic>, Option<i32>)>,
}

/// Starts the compile thread: it runs each job it receives and sends back
/// the result, or `None` when the compiler panicked (the panic is printed
/// to stderr and the server keeps its previous analysis).
///
/// # Panics
///
/// When the operating system cannot start a thread.
#[must_use]
pub fn spawn_worker() -> (Sender<Job>, Receiver<(u64, Option<Compiled>)>) {
    let (jobs, job_receiver) = crossbeam_channel::unbounded::<Job>();
    let (result_sender, results) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("lilac-compile".to_owned())
        .spawn(move || {
            for job in job_receiver {
                let generation = job.generation;
                let compiled = panic::catch_unwind(AssertUnwindSafe(|| job.run())).ok();
                if result_sender.send((generation, compiled)).is_err() {
                    break;
                }
            }
        })
        .expect("spawning the compile thread");
    (jobs, results)
}

impl Job {
    /// Compiles the project and every open file it does not reach.
    #[must_use]
    pub fn run(&self) -> Compiled {
        let overlay = self.documents.overlay();
        let mut by_uri: HashMap<Url, Vec<LspDiagnostic>> = HashMap::new();
        let mut covered: HashSet<Url> = HashSet::new();
        let project = self.root.as_ref().map(|root| {
            let mut session = new_session(&self.libraries);
            let mut fs = OverlayFs { overlay: &overlay };
            let mut stages = LirStages::default();
            let _ = session.compile_project_with_stages(root, &mut fs, &mut |name, lir| {
                stages.modules.push((name, lir.clone()));
            });
            // Project-load errors (E025) can have no source span.
            let anchor = self.project_anchor(root);
            retain(session, stages, anchor.as_ref(), &mut covered, &mut by_uri)
        });
        let per_file = self.compile_units(&overlay, &mut covered, &mut by_uri);

        // Key everything by the client's spelling of an open document's URI
        // (`file:///c%3A/...`), so one document never gets two diagnostic
        // sets that overwrite each other.
        let open: HashMap<Url, Url> = self
            .documents
            .iter()
            .map(|(uri, _)| (canonical_file_uri(uri), uri.clone()))
            .collect();
        let mut diagnostics: HashMap<Url, (Vec<LspDiagnostic>, Option<i32>)> = HashMap::new();
        for (uri, found) in by_uri {
            let uri = open.get(&canonical_file_uri(&uri)).cloned().unwrap_or(uri);
            diagnostics.entry(uri).or_default().0.extend(found);
        }
        // Publish open documents even when clean, so stale sets are cleared.
        for (uri, document) in self.documents.iter() {
            diagnostics.entry(uri.clone()).or_default().1 = Some(document.version);
        }
        Compiled {
            generation: self.generation,
            project,
            per_file,
            diagnostics,
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
    ) -> HashMap<Url, Arc<Retained>> {
        // An open file the project does not reach is checked on its own unit
        // (see `unit_of`). Files sharing a unit share one compile, and a
        // compile also covers the open files of every unit it loads (a
        // library's root namespace loads its subfolders), so outer units go
        // first and covered files are neither recompiled nor re-published.
        let mut new_per_file: HashMap<Url, Arc<Retained>> = HashMap::new();
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
            let retained = Arc::new(match &unit {
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
                    .or_insert_with(|| Arc::clone(&retained));
            }
        }
        new_per_file
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
