//! Driver: `Session` plus the one pipeline (`Session::run`): module load,
//! parse, resolve, check, lower, fold, runtime injection, DCE, the live
//! `@compile_error` check, and verification.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lilac_error::{Diagnostic, Diags, Emitter, EmitterHuman, EmitterJson, ErrorGuaranteed, Level};
use lilac_ir::{LirModule, Syscalls, System};
use lilac_module::Fs;
use lilac_span::{FileId, SourceMap};

pub use lilac_ir::Entry;
pub use lilac_target::{Arch, LinkFlavor, LinkInfo, Os, Target};

/// `--error-format=` selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ErrorFormat {
    /// `rustc`-style human output.
    #[default]
    Human,
    /// Newline-delimited JSON.
    Json,
}

/// Optimization mode (Zig-style names). The CLI maps each onto the LLVM
/// backend's [`lilac_codegen::OptLevel`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OptimizeMode {
    /// No optimization (default).
    #[default]
    Debug,
    /// Standard optimization (`--release`).
    Release,
    /// Optimize for size.
    ReleaseSmall,
}

impl OptimizeMode {
    /// Parses one of `debug`/`release`/`release-small`; `None` otherwise.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "debug" => Some(Self::Debug),
            "release" => Some(Self::Release),
            "release-small" => Some(Self::ReleaseSmall),
            _ => None,
        }
    }

    /// The mode's name, as [`OptimizeMode::parse`] reads it and as `lilac
    /// build` names its output folder.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
            Self::ReleaseSmall => "release-small",
        }
    }
}

/// A pipeline stage, reported to [`Session::progress`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    /// Loading the source files (the module loader, or the one file).
    Load,
    /// Parsing every file.
    Parse,
    /// Building the definition table.
    Resolve,
    /// Type checking.
    Check,
    /// HIR to LIR.
    Lower,
    /// Constant folding.
    Fold,
    /// Injecting the `.lilac.*` runtime.
    Runtime,
    /// Dead-code elimination.
    Prune,
    /// The live `@compile_error` check and IR verification.
    Verify,
}

/// A progress event for one [`Stage`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StageEvent {
    /// The stage is starting.
    Begin,
    /// The stage finished. `count` is the files loaded ([`Stage::Load`]),
    /// the functions checked ([`Stage::Check`]) or the functions left after
    /// pruning ([`Stage::Prune`]); `None` for the other stages.
    End {
        /// The stage's item count, if it has one.
        count: Option<usize>,
    },
}

/// A progress callback. The driver only calls it; timing and printing are
/// the caller's (the driver stays I/O-free and clock-free).
pub struct Progress(Box<dyn FnMut(Stage, StageEvent)>);

impl Progress {
    /// Wraps `hook` as a progress callback.
    #[must_use]
    pub fn new(hook: impl FnMut(Stage, StageEvent) + 'static) -> Self {
        Self(Box::new(hook))
    }
}

impl std::fmt::Debug for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Progress(..)")
    }
}

/// Per-compilation options.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    /// Show full instantiation stacks (default elides past 5 frames).
    pub verbose: bool,
    /// Output format.
    pub error_format: ErrorFormat,
    /// Colour human diagnostics with ANSI codes.
    pub color: bool,
}
/// Retained analysis artifacts from the last compile (for editor tooling).
#[derive(Clone, Debug)]
pub struct Analysis {
    /// Resolved definitions, namespaces and per-file scopes.
    pub resolved: lilac_resolve::Resolved,
    /// Type table (for rendering types).
    pub types: lilac_ty::Types,
    /// Declared signature of every function and hook.
    pub sigs: std::collections::HashMap<lilac_resolve::DefId, lilac_ty::FuncSig>,
    /// Checked HIR (carries `uses`), plus the editor-only bodies of
    /// uninstantiated generic functions (`Checked::templates`).
    pub hir: lilac_hir::HirProgram,
    /// Non-item semantic-token sites, deduped by `(span, kind)` and sorted by
    /// `(file, lo)`.
    pub semantics: Vec<lilac_hir::SemanticSite>,
}

/// Dedups semantic-token sites by `(span, kind)` and sorts them by
/// `(file, lo)`.
fn merge_semantics(sites: &[lilac_hir::SemanticSite]) -> Vec<lilac_hir::SemanticSite> {
    let mut sites = sites.to_vec();
    sites.sort_by_key(|site| (site.span.file.0, site.span.lo.0, site.span.hi.0));
    sites.dedup_by(|a, b| a.span == b.span && a.kind == b.kind);
    sites
}

/// Compilation session: source map + diagnostics + targets + options.
///
/// Threaded explicitly through every pass (no globals, unlike the old Zig
/// `Logger`); mirrors `rustc_session`.
#[derive(Debug)]
pub struct Session {
    /// File contents.
    pub source_map: SourceMap,
    /// Collected diagnostics.
    pub diags: Diags,
    /// Host (build scripts run here).
    pub host: Target,
    /// Target (code runs here; fixes `@bit_native`).
    pub target: Target,
    /// Flags.
    pub opts: Opts,
    /// Optimization level (read by the LLVM backend).
    pub optimize: OptimizeMode,
    /// Cache directory (reserved for the post-v1 content cache).
    pub cache_dir: Option<PathBuf>,
    /// Extra reachability roots (`--export=<name>`): functions to keep alive
    /// even when nothing calls them, so a library can expose entry points.
    pub exports: HashSet<String>,
    /// Loaded project files: identity to path (empty outside project compiles).
    pub project_files: Vec<(FileId, PathBuf)>,
    /// Mounted libraries (`--lib Name=dir`): `using Name.X` loads `dir/X`.
    pub libraries: Vec<(String, PathBuf)>,
    /// Retained analysis from the last compile (best-effort; editor tooling).
    pub analysis: Option<Analysis>,
    /// Whether to run the passes past parse/type errors and keep the partial
    /// analysis (editor tooling). Strict (the CLI) leaves this `false`, so the
    /// stage gates reproduce today's stop-at-the-first-failing-stage behavior.
    pub retain_partial_analysis: bool,
    /// Whether the program must have an entry point (`lilac build`): a
    /// non-`extern` `main` in the root namespace, else `E032`.
    pub require_entry: bool,
    /// How the program enters. [`Entry::Native`] (`lilac build` without
    /// `--libc`) adds the `.lilac.start` entry and exits through the OS
    /// rather than the C library.
    pub entry: Entry,
    /// Called at the start and end of each pipeline [`Stage`].
    pub progress: Option<Progress>,
}

impl Default for Session {
    /// Session with the real build host, the canonical default target, and
    /// human errors. Kept identical to [`Session::new`] so neither construction
    /// path surprises the other.
    fn default() -> Self {
        Self {
            source_map: SourceMap::default(),
            diags: Diags::default(),
            host: Target::host(),
            target: Target::default(),
            opts: Opts::default(),
            optimize: OptimizeMode::default(),
            cache_dir: None,
            exports: HashSet::new(),
            project_files: Vec::new(),
            libraries: Vec::new(),
            analysis: None,
            retain_partial_analysis: false,
            require_entry: false,
            entry: Entry::CMain,
            progress: None,
        }
    }
}

impl Session {
    /// Default session (real host, canonical default target, human errors).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Compiles one source file (a one-file project) through IR
    /// verification, returning the folded module.
    pub fn compile_lir(&mut self, name: &str, src: &str) -> Result<LirModule, ErrorGuaranteed> {
        self.compile_lir_with_stages(name, src, &mut |_: &'static str, _: &LirModule| {})
    }

    /// Like [`Session::compile_lir`], but invokes `on_stage` at every pipeline
    /// boundary with the stage name and the module as it stands there. Stages,
    /// in order: `lower` (before folding), `fold`, `runtime` (after the
    /// `.lilac.*` prelude is injected, before DCE), and `final` (the returned
    /// module). The driver does no file I/O itself.
    pub fn compile_lir_with_stages(
        &mut self,
        name: &str,
        src: &str,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        self.project_files.clear();
        self.report(Stage::Load, StageEvent::Begin);
        let project = lilac_module::single_file(name, src, &mut self.source_map);
        self.report(Stage::Load, StageEvent::End { count: Some(1) });
        self.run(&project, on_stage)
    }

    /// Compiles a folder project rooted at `root`, loading every file reachable
    /// through `using` via the injected [`Fs`]. Returns the folded, verified
    /// module.
    pub fn compile_project(
        &mut self,
        root: &Path,
        fs: &mut dyn Fs,
    ) -> Result<LirModule, ErrorGuaranteed> {
        self.compile_project_with_stages(root, fs, &mut |_: &'static str, _: &LirModule| {})
    }

    /// Like [`Session::compile_project`], but fires `on_stage` at every pipeline
    /// boundary exactly as [`Session::compile_lir_with_stages`] does.
    pub fn compile_project_with_stages(
        &mut self,
        root: &Path,
        fs: &mut dyn Fs,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        self.project_files.clear();
        self.report(Stage::Load, StageEvent::Begin);
        let (project, failed) = lilac_module::load_with_status(
            root,
            &self.libraries,
            fs,
            &mut self.source_map,
            &mut self.diags,
        );
        self.run_loaded(&project, failed, on_stage)
    }

    /// Compiles one source file (`name`, already read as `src`) as the root
    /// namespace's only file, loading what its `using`s reach (mounted
    /// libraries, or folders next to it) through `fs`. This is how the CLI
    /// compiles a file argument.
    pub fn compile_file_with_stages(
        &mut self,
        name: &str,
        src: &str,
        fs: &mut dyn Fs,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        self.project_files.clear();
        self.report(Stage::Load, StageEvent::Begin);
        let (project, failed) = lilac_module::load_file(
            name,
            src,
            &self.libraries,
            fs,
            &mut self.source_map,
            &mut self.diags,
        );
        self.run_loaded(&project, failed, on_stage)
    }

    /// Records a loaded project's files, ends the Load stage, and runs the
    /// pipeline unless loading failed.
    fn run_loaded(
        &mut self,
        project: &lilac_module::Project,
        failed: bool,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        self.project_files = project
            .files
            .iter()
            .map(|file| (file.file, file.path.clone()))
            .collect();
        self.report(
            Stage::Load,
            StageEvent::End {
                count: Some(project.files.len()),
            },
        );
        if failed {
            return Err(ErrorGuaranteed::new());
        }
        self.run(project, on_stage)
    }

    /// Checks the namespace `namespace` (typically a mounted library folder)
    /// and everything it `using`s, with an empty root namespace. Editor
    /// tooling uses this for library files no project reaches.
    pub fn compile_namespace_with_stages(
        &mut self,
        root: &Path,
        namespace: &str,
        fs: &mut dyn Fs,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        self.project_files.clear();
        let (project, failed) = lilac_module::load_namespace(
            root,
            namespace,
            &self.libraries,
            fs,
            &mut self.source_map,
            &mut self.diags,
        );
        self.project_files = project
            .files
            .iter()
            .map(|file| (file.file, file.path.clone()))
            .collect();
        if failed {
            return Err(ErrorGuaranteed::new());
        }
        self.run(&project, on_stage)
    }

    /// The loaded project's file identities and paths (empty outside project
    /// compiles); maps diagnostic spans to file paths.
    #[must_use]
    pub fn project_files(&self) -> &[(FileId, PathBuf)] {
        &self.project_files
    }

    /// Retained analysis from the last compile (populated best-effort, so it
    /// survives a compile that later failed).
    #[must_use]
    pub fn analysis(&self) -> Option<&Analysis> {
        self.analysis.as_ref()
    }

    /// Calls the progress hook, if any.
    fn report(&mut self, stage: Stage, event: StageEvent) {
        if let Some(Progress(hook)) = &mut self.progress {
            hook(stage, event);
        }
    }

    /// Parse → resolve → check → lower → fold → inject runtime → prune →
    /// live `@compile_error` check → verify. Strict mode (the CLI) stops at the
    /// first failing stage; tolerant mode (editor tooling) keeps going through
    /// checking so the partial analysis is retained.
    #[allow(clippy::too_many_lines)]
    fn run(
        &mut self,
        project: &lilac_module::Project,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        const DONE: StageEvent = StageEvent::End { count: None };
        let strict = !self.retain_partial_analysis;
        self.report(Stage::Parse, StageEvent::Begin);
        let (programs, failed) = lilac_module::parse_all(project, &mut self.diags, strict);
        if failed && strict {
            return Err(ErrorGuaranteed::new());
        }
        self.report(Stage::Parse, DONE);
        self.report(Stage::Resolve, StageEvent::Begin);
        let resolved = lilac_resolve::resolve(project, &programs, &mut self.diags);
        if self.diags.has_errors() && strict {
            return Err(ErrorGuaranteed::new());
        }
        self.report(Stage::Resolve, DONE);
        self.report(Stage::Check, StageEvent::Begin);
        let mut types = lilac_ty::Types::new();
        let check = if self.retain_partial_analysis {
            lilac_typeck::check_for_editor
        } else {
            lilac_typeck::check
        };
        let checked = check(
            &resolved,
            &mut types,
            &mut self.diags,
            self.target.bit_native_width(),
        );
        let mut hir = checked.hir.clone();
        hir.funcs.extend(checked.templates.iter().cloned());
        self.analysis = Some(Analysis {
            resolved: resolved.clone(),
            types: types.clone(),
            sigs: checked.sigs.clone(),
            semantics: merge_semantics(&checked.hir.semantics),
            hir,
        });
        if self.diags.has_errors() {
            return Err(ErrorGuaranteed::new());
        }
        lilac_lower::check_extern_signatures(&checked.hir, &resolved, &types, &mut self.diags);
        if self.diags.has_errors() {
            return Err(ErrorGuaranteed::new());
        }
        self.report(
            Stage::Check,
            StageEvent::End {
                count: Some(checked.hir.funcs.len()),
            },
        );
        self.report(Stage::Lower, StageEvent::Begin);
        let ptr_bytes = u64::from(self.target.bit_native_width() / 8);
        let triple = self.target.to_triple();
        let mut lir = lilac_lower::lower(&checked.hir, &resolved, &types, &triple, ptr_bytes);
        if self.require_entry && !has_entry(&lir) {
            return Err(self.diags.emit(
                Diagnostic::error(
                    "E032",
                    "no `main` function; `lilac build` needs an entry point",
                )
                .help("add `main: function () nothing { … }`, or use `lilac check` for a library"),
            ));
        }
        on_stage("lower", &lir);
        self.report(Stage::Lower, DONE);
        self.report(Stage::Fold, StageEvent::Begin);
        let report = lilac_const::fold(&mut lir);
        on_stage("fold", &lir);
        self.report(Stage::Fold, DONE);
        self.report(Stage::Runtime, StageEvent::Begin);
        self.inject_runtime(&mut lir);
        on_stage("runtime", &lir);
        self.report(Stage::Runtime, DONE);
        self.report(Stage::Prune, StageEvent::Begin);
        let mut roots = self.exports.clone();
        if self.entry == Entry::Native && has_entry(&lir) {
            roots.insert(".lilac.start".to_owned());
        }
        lilac_ir::prune_unreachable(&mut lir, &roots);
        self.report(
            Stage::Prune,
            StageEvent::End {
                count: Some(lir.funcs.len()),
            },
        );
        self.report(Stage::Verify, StageEvent::Begin);
        lilac_const::check_live_compile_errors(&lir, &report, &mut self.diags)?;
        lilac_ir::verify(&lir, &mut self.diags)?;
        if self.diags.has_errors() {
            return Err(ErrorGuaranteed::new());
        }
        self.report(Stage::Verify, DONE);
        on_stage("final", &lir);
        Ok(lir)
    }

    fn human_emitter(&self) -> EmitterHuman {
        EmitterHuman {
            color: self.opts.color,
            verbose: self.opts.verbose,
        }
    }

    /// Renders all recorded diagnostics to one string (human ones separated
    /// by blank lines).
    #[must_use]
    pub fn render_diagnostics(&self) -> String {
        let human = self.human_emitter();
        let rendered = self
            .diags
            .diagnostics()
            .iter()
            .map(|diagnostic| match self.opts.error_format {
                ErrorFormat::Human => human.render(diagnostic, &self.source_map),
                ErrorFormat::Json => EmitterJson.render(diagnostic, &self.source_map),
            })
            .collect::<Vec<_>>();
        match self.opts.error_format {
            ErrorFormat::Human => rendered.join("\n"),
            ErrorFormat::Json => rendered.concat(),
        }
    }

    /// The human closing line (`error: aborting due to …`), preceded by a
    /// blank line; empty for JSON or when nothing was reported.
    #[must_use]
    pub fn render_summary(&self) -> String {
        if self.opts.error_format == ErrorFormat::Json {
            return String::new();
        }
        let count = |level| {
            self.diags
                .diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.level == level)
                .count()
        };
        let suppressed = self.diags.suppressed_errors();
        let summary = self.human_emitter().summary(
            count(Level::Error) + suppressed,
            suppressed,
            count(Level::Warning),
        );
        if summary.is_empty() {
            summary
        } else {
            format!("\n{summary}")
        }
    }
}

impl Session {
    /// Appends the runtime for the target's OS layer and entry, plus the
    /// native `.lilac.start` when the program has a `main`. A runtime
    /// `extern` the program already declares (say, the C library's `exit`)
    /// is not declared twice.
    fn inject_runtime(&self, lir: &mut LirModule) {
        let system = match (self.target.os, self.target.arch, self.entry) {
            (Os::Windows, _, _) => System::Windows,
            (Os::Linux, Arch::X86_64, Entry::Native) => System::Linux(Syscalls::X86_64),
            (Os::Linux, Arch::Aarch64, Entry::Native) => System::Linux(Syscalls::AARCH64),
            (Os::Linux | Os::Macos, _, _) => System::Libc,
            (Os::None | Os::Unknown, _, _) => System::None,
        };
        let runtime =
            lilac_ir::build_runtime(self.target.bit_native_width() / 8, system, self.entry);
        for func in runtime {
            let declared = func.is_extern
                && lir
                    .funcs
                    .iter()
                    .any(|other| other.is_extern && other.name == func.name);
            if !declared {
                lir.funcs.push(func);
            }
        }
        if self.entry == Entry::Native {
            if let Some(main) = lir
                .funcs
                .iter()
                .find(|func| func.name == "main" && !func.is_extern)
            {
                let start = lilac_ir::entry_point(main);
                lir.funcs.push(start);
            }
        }
    }
}

/// Whether `lir` has the program entry point: a non-`extern` `main` in the
/// root namespace (whose symbols carry no prefix).
fn has_entry(lir: &LirModule) -> bool {
    lir.funcs
        .iter()
        .any(|func| func.name == "main" && !func.is_extern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_compiles() {
        let mut session = Session::new();
        let module = session
            .compile_lir("hello.lilac", "main: function () nothing {\n}\n")
            .expect("hello should compile");
        assert!(module.funcs.iter().any(|func| func.name == "main"));
        assert_eq!(session.render_diagnostics(), "");
    }

    #[test]
    fn analysis_is_retained_on_a_type_error() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "bad.lilac",
            "pi: @bit32 = 0x2A\nmain: function () nothing {\n    y: @bit8 = pi\n}\n",
        );
        assert!(result.is_err(), "the body has a type error");
        let analysis = session.analysis().expect("partial analysis retained");
        assert!(
            !analysis.hir.uses.is_empty(),
            "uses must survive the error: {:?}",
            analysis.hir.uses
        );
    }

    #[test]
    fn tolerant_analysis_retained_after_syntax_and_type_errors() {
        // (a) A syntax error in one function: the recovered `ItemKind::Error`
        // does not stop the pipeline, and the surviving items still contribute
        // name/type sites to the retained HIR.
        let mut session = Session::new();
        session.retain_partial_analysis = true;
        let result = session.compile_lir(
            "syntax.lilac",
            "broken: function ( {\n}\npi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n",
        );
        assert!(result.is_err(), "errors still refuse lowering");
        let analysis = session.analysis().expect("partial analysis retained");
        assert!(
            !analysis.hir.uses.is_empty(),
            "uses must survive the syntax error: {:?}",
            analysis.hir.uses
        );

        // (b) A type error in one body: `check_bodies` runs to completion and
        // its partial HIR is retained.
        let mut session = Session::new();
        session.retain_partial_analysis = true;
        let result = session.compile_lir(
            "type.lilac",
            "pi: @bit32 = 0x2A\nmain: function () nothing {\n    y: @bit8 = pi\n    x: @bit32 = pi\n}\n",
        );
        assert!(result.is_err(), "the body has a type error");
        let analysis = session.analysis().expect("partial analysis retained");
        assert!(
            !analysis.hir.uses.is_empty(),
            "uses must survive the type error: {:?}",
            analysis.hir.uses
        );
    }

    /// In strict mode the post-parse gate stops before `collect`, so a syntax
    /// error retains no analysis (the CLI's stop-at-the-first-failing-stage
    /// behavior is unchanged).
    #[test]
    fn strict_analysis_is_not_retained_on_syntax_error() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "syntax.lilac",
            "broken: function ( {\n}\npi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n",
        );
        assert!(result.is_err(), "strict mode refuses the syntax error");
        assert!(
            session.analysis().is_none(),
            "strict mode must stop before building the analysis"
        );
    }

    #[test]
    fn analysis_is_retained_on_success() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "ok.lilac",
            "pi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n",
        );
        assert!(result.is_ok(), "{:?}", session.render_diagnostics());
        let analysis = session.analysis().expect("analysis retained");
        assert!(!analysis.hir.uses.is_empty(), "{:?}", analysis.hir.uses);
    }

    #[test]
    fn semantic_analysis_populated_on_success_and_type_error() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "ok.lilac",
            "main: function () nothing {\n    x: @bit32 = 0x1\n    y: @bit32 = x\n}\n",
        );
        assert!(result.is_ok(), "{}", session.render_diagnostics());
        let analysis = session.analysis().expect("analysis retained");
        assert!(
            !analysis.semantics.is_empty(),
            "semantics must be populated: {:?}",
            analysis.semantics
        );

        let mut session = Session::new();
        let result = session.compile_lir(
            "bad.lilac",
            "pi: @bit32 = 0x2A\nmain: function () nothing {\n    y: @bit8 = pi\n}\n",
        );
        assert!(result.is_err(), "the body has a type error");
        let analysis = session.analysis().expect("partial analysis retained");
        assert!(
            !analysis.semantics.is_empty(),
            "semantics must survive the error: {:?}",
            analysis.semantics
        );
    }

    #[test]
    fn broken_source_reports_e001() {
        let mut session = Session::new();
        let result = session.compile_lir("bad.lilac", "main: function ( {\n}\n");
        assert!(result.is_err());
        assert!(session.render_diagnostics().contains("error[E001]"));
    }

    #[test]
    fn check_runs_typeck() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "bad.lilac",
            "main: function () nothing {\n    x: @bit8 = nope\n}\n",
        );
        assert!(result.is_err());
        assert!(session.render_diagnostics().contains("error[E021]"));
    }

    #[test]
    fn check_accepts_clean_typed_source() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "ok.lilac",
            "@conversion: function (from: @bit32) to: @bit8 {\n    to = @truncate(from)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = a\n}\n",
        );
        assert!(result.is_ok(), "{}", session.render_diagnostics());
    }

    /// A source that reaches no runtime function must not keep any of the
    /// injected `.lilac.*` prelude: DCE culls it.
    #[test]
    fn no_runtime_source_culls_the_prelude() {
        let mut session = Session::new();
        let module = session
            .compile_lir("hello.lilac", "main: function () nothing {\n}\n")
            .expect("hello should compile");
        let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
        assert!(
            names.iter().all(|name| !name.starts_with(".lilac.")),
            "unreachable prelude survived DCE: {names:?}"
        );
    }

    /// A source that reaches the byte-buffer runtime keeps exactly the
    /// reachable prelude functions (and no others). `@byte_buffer_ptr` cannot
    /// be folded, so its `.lilac.` body survives DCE.
    #[test]
    fn used_runtime_survives_dce() {
        let mut session = Session::new();
        let module = session
            .compile_lir(
                "string.lilac",
                "main: function () nothing {\n    s: @byte_buffer = \"hello\"\n    p: ref @bit8 = @byte_buffer_ptr(s)\n}\n",
            )
            .expect("string should compile");
        let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&".lilac.byte_buffer_ptr"), "{names:?}");
        assert!(!names.contains(&".lilac.format_args_new"), "{names:?}");
    }

    /// A non-const `@load_u8`/`@store_u8` keeps its `.lilac.` body through DCE,
    /// closing the gap where the moved byte-access runtime had no definition.
    #[test]
    fn non_const_byte_access_keeps_runtime() {
        let mut session = Session::new();
        let module = session
            .compile_lir(
                "byte_access.lilac",
                "main: function () nothing {\n    s: @byte_buffer = \"hello\"\n    p: ref @bit8 = @byte_buffer_ptr(s)\n    c: @bit8 = @load_u8(p, 0x0)\n    @store_u8(p, 0x0, 0x41)\n}\n",
            )
            .expect("byte access should compile");
        let names: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&".lilac.load_u8"), "{names:?}");
        assert!(names.contains(&".lilac.store_u8"), "{names:?}");
    }

    #[test]
    fn pipeline_produces_lir() {
        let mut session = Session::new();
        let module = session
            .compile_lir(
                "branch.lilac",
                "main: function () nothing {\n    x: @bit8 = 0x0\n    if x {\n    }\n}\n",
            )
            .expect("branch source should lower");
        assert!(module.to_text().contains("branch"), "{}", module.to_text());
    }

    #[test]
    fn check_runs_through_verify() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "ok.lilac",
            "main: function () nothing {\n    x: @bit8 = 0x1\n    if x {\n    }\n}\n",
        );
        assert!(result.is_ok(), "{}", session.render_diagnostics());
        assert_eq!(session.render_diagnostics(), "");
    }

    #[test]
    fn generic_inline_control_flow_inlines() {
        // Regression: a generic `=>` body with control flow lowers cleanly.
        let mut session = Session::new();
        let result = session.compile_lir(
            "gen.lilac",
            "pick[T]: function (x: T, c: @bit8) T => if c then x else x\nmain: function () nothing {\n    y: @bit32 = pick[@bit32](0x1, 0x1)\n}\n",
        );
        assert!(result.is_ok(), "{}", session.render_diagnostics());
        assert_eq!(session.render_diagnostics(), "");
    }

    #[test]
    fn concrete_inline_control_flow_arguments_inline() {
        // Regression: a *concrete* `=>` callee whose argument itself carries
        // control flow (a value-`if` or an inline `or`) must have its own body
        // temps renamed *before* substitution, so the argument's already-emitted
        // prelude keeps its temp id and lowering does not ICE with `use of
        // unassigned join temp`.
        for src in [
            // value-`if` argument
            "pick: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
             main: function () nothing {\n    c: @bit8 = 0x1\n    r: @bit8 = pick(if c then 0x1 else 0x2, 0x3)\n}\n",
            // inline `or` argument
            "or2: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
             main: function () nothing {\n    c: @bit8 = 0x1\n    r: @bit8 = or2(c or c, 0x3)\n}\n",
            // nested inline calls: `inner`'s value is `outer`'s argument
            "inner: function (a: @bit8, b: @bit8) @bit8 => a or b\n\
             outer: function (a: @bit8) @bit8 => a or a\n\
             main: function () nothing {\n    r: @bit8 = outer(inner(0x1, 0x2))\n}\n",
        ] {
            let mut session = Session::new();
            let result = session.compile_lir("inline_cf.lilac", src);
            assert!(result.is_ok(), "{}", session.render_diagnostics());
            assert_eq!(session.render_diagnostics(), "");
        }
    }

    #[test]
    fn e010_shows_both_sides() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "e010.lilac",
            "set: function (r: ref @bit8) nothing {\n}\nmain: function () nothing {\n    v: @bit8 = 0x1\n    set(v)\n}\n",
        );
        assert!(result.is_err());
        let rendered = session.render_diagnostics();
        assert!(rendered.contains("error[E010]"), "{rendered}");
        assert!(rendered.contains("expected `ref @bit8`"), "{rendered}");
        assert!(rendered.contains("found `@bit8`"), "{rendered}");
    }

    #[test]
    fn mode_parses_known_names_only() {
        assert_eq!(OptimizeMode::parse("debug"), Some(OptimizeMode::Debug));
        assert_eq!(OptimizeMode::parse("release"), Some(OptimizeMode::Release));
        assert_eq!(
            OptimizeMode::parse("release-small"),
            Some(OptimizeMode::ReleaseSmall)
        );
        assert_eq!(OptimizeMode::parse("O2"), None);
        assert_eq!(OptimizeMode::parse("bogus"), None);
        assert_eq!(OptimizeMode::default(), OptimizeMode::Debug);
    }

    #[test]
    fn target_host_is_valid_and_parse_reports_width() {
        assert!(!Target::host().to_triple().is_empty());
        assert_eq!(
            Target::parse("x86_64-pc-windows-msvc")
                .expect("valid")
                .bit_native_width(),
            64
        );
    }

    /// Returns the `stride` constant literal baked into a `format_args` runtime
    /// function by [`lilac_ir::runtime::build_runtime`] (via the driver).
    fn stride_lit(module: &LirModule, name: &str) -> String {
        let func = module
            .funcs
            .iter()
            .find(|func| func.name == name)
            .unwrap_or_else(|| panic!("missing runtime function `{name}`"));
        func.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .find_map(|inst| match inst {
                lilac_ir::LirInst::Const { dst, lit, ty, .. }
                    if dst == "stride" && *ty == lilac_ir::LirTy::BitNative =>
                {
                    Some(lit.clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing stride const in `{name}`"))
    }

    /// The stride the driver bakes is `#byte_buffer`'s size, so it must track
    /// the target's pointer width for every 64-bit arch, not just
    /// `x86_64`/`aarch64`. Regression: `riscv64` previously baked `0x8` while
    /// the backend laid `#byte_buffer` out as 16 bytes.
    #[test]
    fn format_args_stride_follows_target_width() {
        for (triple, want) in [
            ("riscv64-unknown-none", "0x10"),
            ("riscv32-unknown-none", "0x8"),
        ] {
            let mut session = Session::new();
            session.target = Target::parse(triple).expect("valid triple");
            let module = session
                .compile_lir(
                    "fs.lilac",
                    "main: function () nothing {\n    part: @byte_buffer = \"x\"\n    msg: @format_args = $\"a{part}b\"\n    n: @bit_native = @format_args_count(msg)\n    first: @byte_buffer = @format_args_piece(msg, 0x0)\n}\n",
                )
                .unwrap_or_else(|_| panic!("{triple}: {}", session.render_diagnostics()));
            for name in [".lilac.format_args_push", ".lilac.format_args_piece"] {
                assert_eq!(stride_lit(&module, name), want, "{triple} in {name}");
            }
        }
    }

    /// `compile_lir_with_stages` fires the hook at each boundary, in order, and
    /// the `final` stage is exactly the returned module.
    #[test]
    fn stages_hook_fires_in_order() {
        let mut session = Session::new();
        let mut texts: Vec<(&'static str, String)> = Vec::new();
        let module = session
            .compile_lir_with_stages(
                "s.lilac",
                "main: function () nothing {\n    x: @bit_native = @i_add_native(0x1, 0x2)\n}\n",
                &mut |stage, lir| texts.push((stage, lir.to_text())),
            )
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let names: Vec<&str> = texts.iter().map(|(stage, _)| *stage).collect();
        assert_eq!(names, ["lower", "fold", "runtime", "final"]);
        assert_ne!(
            texts[0].1, texts[1].1,
            "const folding must change the module"
        );
        assert_eq!(
            texts[3].1,
            module.to_text(),
            "final stage is the returned module"
        );
    }

    /// A `@compile_error` in a function unreachable from `main` is silent:
    /// global DCE drops the function before the live-error check runs.
    #[test]
    fn unreachable_compile_error_is_silent() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "dead.lilac",
            "dead: function () nothing {\n    @compile_error(\"dead\")\n}\n\nmain: function () nothing {\n}\n",
        );
        assert!(result.is_ok(), "{}", session.render_diagnostics());
    }

    /// A `@compile_error` reachable from `main` still reports `E031`.
    #[test]
    fn reachable_compile_error_is_e031() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "live.lilac",
            "boom: function () nothing {\n    @compile_error(\"live\")\n}\n\nmain: function () nothing {\n    boom()\n}\n",
        );
        assert!(result.is_err());
        assert!(
            session.render_diagnostics().contains("error[E031]"),
            "{}",
            session.render_diagnostics()
        );
    }

    /// An `--export`ed function roots the module: a `@compile_error` in a
    /// function nothing calls still reports `E031`.
    #[test]
    fn exported_compile_error_is_e031() {
        let mut session = Session::new();
        session.exports.insert("api".to_owned());
        let result = session.compile_lir(
            "api.lilac",
            "api: function () nothing {\n    @compile_error(\"api\")\n}\n\nmain: function () nothing {\n}\n",
        );
        assert!(result.is_err());
        assert!(
            session.render_diagnostics().contains("error[E031]"),
            "{}",
            session.render_diagnostics()
        );
    }

    /// An `--export` root prunes unexported functions even with no `main`, so a
    /// library keeps exactly its exported entry points.
    #[test]
    fn export_roots_without_main_prune_others() {
        let mut session = Session::new();
        session.exports.insert("api".to_owned());
        let module = session
            .compile_lir(
                "lib.lilac",
                "api: function () nothing {\n}\n\ndead: function () nothing {\n}\n",
            )
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let names: Vec<&str> = module.funcs.iter().map(|func| func.name.as_str()).collect();
        assert!(names.contains(&"api"), "{names:?}");
        assert!(!names.contains(&"dead"), "{names:?}");
    }

    /// `require_entry` (set by `lilac build`) turns a missing `main` into
    /// `E032`; other compiles accept a library without one.
    #[test]
    fn require_entry_reports_e032() {
        let lib = "helper: function () nothing {\n}\n";
        let mut session = Session::new();
        session.exports.insert("helper".to_owned());
        assert!(session.compile_lir("lib.lilac", lib).is_ok());

        let mut session = Session::new();
        session.require_entry = true;
        assert!(session.compile_lir("lib.lilac", lib).is_err());
        let rendered = session.render_diagnostics();
        assert!(rendered.contains("error[E032]"), "{rendered}");

        let mut session = Session::new();
        session.require_entry = true;
        let result = session.compile_lir("main.lilac", "main: function () nothing {\n}\n");
        assert!(result.is_ok(), "{}", session.render_diagnostics());
    }

    /// The progress hook sees every stage begin and end, in pipeline order,
    /// with the load, check and prune counts.
    #[test]
    fn progress_reports_every_stage_in_order() {
        use std::cell::RefCell;
        use std::rc::Rc;
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&events);
        let mut session = Session::new();
        session.progress = Some(Progress::new(move |stage, event| {
            sink.borrow_mut().push((stage, event));
        }));
        session
            .compile_lir("p.lilac", "main: function () nothing {\n}\n")
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let events = events.borrow();
        let stages = [
            Stage::Load,
            Stage::Parse,
            Stage::Resolve,
            Stage::Check,
            Stage::Lower,
            Stage::Fold,
            Stage::Runtime,
            Stage::Prune,
            Stage::Verify,
        ];
        assert_eq!(events.len(), stages.len() * 2, "{events:?}");
        for (pair, stage) in events.chunks(2).zip(stages) {
            assert_eq!(pair[0], (stage, StageEvent::Begin), "{events:?}");
            assert_eq!(pair[1].0, stage, "{events:?}");
        }
        assert_eq!(events[1].1, StageEvent::End { count: Some(1) });
        assert_eq!(events[7].1, StageEvent::End { count: Some(1) });
        assert_eq!(events[15].1, StageEvent::End { count: Some(1) });
    }

    struct TestFs {
        files: std::collections::HashMap<PathBuf, String>,
        dirs: std::collections::HashMap<PathBuf, Vec<PathBuf>>,
    }

    impl lilac_module::Fs for TestFs {
        fn read(&mut self, path: &Path) -> std::io::Result<String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"))
        }

        fn list_dir(&mut self, dir: &Path) -> std::io::Result<Vec<PathBuf>> {
            self.dirs.get(dir).cloned().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such directory")
            })
        }
    }

    #[test]
    fn folded_literal_lowers_to_const() {
        let mut s = Session::new();
        let lir = s
            .compile_lir(
                "d.lilac",
                "main: function () nothing {\n    x: @bit8 = 2 * 3\n}\n",
            )
            .expect("compiles");
        assert!(lir.to_text().contains("const 0x6"), "{}", lir.to_text());
    }

    #[test]
    fn division_by_zero_is_e060() {
        let mut s = Session::new();
        assert!(s
            .compile_lir(
                "d.lilac",
                "main: function () nothing {\n    x: @byte_buffer = 1 / 0\n}\n"
            )
            .is_err());
        assert!(s.render_diagnostics().contains("E060"));
    }

    /// Strict mode aborts a project compile at the first parse-failing file
    /// (today's `?` on the old `Err` return), so later files are never parsed
    /// or reported. Regression guard for item-level recovery making
    /// `parse_program` always `Ok`.
    #[test]
    fn strict_project_aborts_at_first_parse_failing_file() {
        let root = PathBuf::from("proj");
        let a = root.join("a.lilac");
        let b = root.join("b.lilac");
        let mut fs = TestFs {
            files: [
                (a.clone(), "broken_a: function (\n".to_owned()),
                (b.clone(), "broken_b: function (\n".to_owned()),
            ]
            .into_iter()
            .collect(),
            dirs: [(root.clone(), vec![a, b])].into_iter().collect(),
        };
        let mut session = Session::new();
        let result = session.compile_project(&root, &mut fs);
        assert!(result.is_err(), "both files are broken");
        let rendered = session.render_diagnostics();
        assert_eq!(
            rendered.matches("error[E001]").count(),
            1,
            "only the first parse-failing file is reported:\n{rendered}"
        );
        assert!(rendered.contains("a.lilac"), "{rendered}");
        assert!(
            !rendered.contains("b.lilac"),
            "the later file must not be parsed in strict mode:\n{rendered}"
        );
    }

    #[test]
    fn project_files_maps_ids_to_paths() {
        let root = PathBuf::from("proj");
        let main = root.join("main.lilac");
        let mut fs = TestFs {
            files: [(main.clone(), "main: function () nothing {\n}\n".to_owned())]
                .into_iter()
                .collect(),
            dirs: [(root.clone(), vec![main.clone()])].into_iter().collect(),
        };
        let mut session = Session::new();
        let _ = session.compile_project(&root, &mut fs);
        assert_eq!(session.project_files(), [(FileId(0), main)]);
    }
}
