//! Driver: `Session` plus the one pipeline (`Session::run`): module load,
//! the generated `@builtin` namespace ([`builtin`]), parse, resolve, check, lower, runtime injection, DCE, the live
//! `@compile_error` check, stack-trace instrumentation, and verification.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use lilac_error::{
    codes, Diagnostic, Diags, Emitter, EmitterHuman, EmitterJson, ErrorGuaranteed, Level,
    Unreported,
};
use lilac_ir::LirModule;
use lilac_module::Fs;
use lilac_span::{FileId, SourceMap};

pub mod builtin;
mod conditional;

pub use lilac_ir::Entry;
pub use lilac_target::{Arch, LinkFlavor, LinkInfo, Os, Target};

/// The stack a compile needs: run [`Session`]s on a thread this big.
///
/// Every pass recurses over the syntax tree, and the parser bounds its
/// nesting (`lilac_parse::MAX_NESTING`), so this covers the deepest program
/// it accepts, in a debug build. It is reserved address space, not memory
/// used up front. The main thread's stack (1 MiB on Windows) is too small.
pub const COMPILE_STACK_BYTES: usize = 64 << 20;

/// How many errors a terminal shows ([`Session::render_diagnostics`]); the
/// summary counts the rest. The session keeps them all, and JSON output
/// carries them all.
pub const SHOWN_ERRORS: usize = 10;

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
pub struct Progress(Box<dyn FnMut(Stage, StageEvent) + Send + Sync>);

impl Progress {
    /// Wraps `hook` as a progress callback.
    #[must_use]
    pub fn new(hook: impl FnMut(Stage, StageEvent) + Send + Sync + 'static) -> Self {
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
/// What a compile keeps for editor tooling ([`Session::keep_analysis`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Keep {
    /// Nothing (the CLI): the pipeline stops at the first failing stage,
    /// and the passes' outputs go once lowered.
    #[default]
    Nothing,
    /// The [`Analysis`] of a compile that got through type checking; the
    /// pipeline still stops at the first failing stage.
    Analysis,
    /// The [`Analysis`], with the passes run past parse and type errors so
    /// a broken program has a partial one (the language server).
    Partial,
}

/// What the last compile found, for editor tooling
/// ([`Session::keep_analysis`]). It owns the passes' outputs: the pipeline
/// lowers from them and then moves them in, so keeping it copies nothing.
#[derive(Debug)]
pub struct Analysis {
    /// Resolved definitions, namespaces and per-file scopes.
    pub resolved: lilac_resolve::Resolved,
    /// Type table (for rendering types).
    pub types: lilac_ty::Types,
    /// Declared signature of every function and hook.
    pub sigs: std::collections::HashMap<lilac_resolve::DefId, lilac_ty::FuncSig>,
    /// Checked HIR (carries `uses`), with the top-level conditions' tag and
    /// member sites added. Its `semantics` moved to [`Analysis::semantics`].
    pub hir: lilac_hir::HirProgram,
    /// The never-lowered bodies of uninstantiated generic functions
    /// (`Checked::templates`), for semantic tokens and hovers.
    pub templates: Vec<lilac_hir::HirFunc>,
    /// Non-item semantic-token sites, deduped by `(span, kind)` and sorted by
    /// `(file, lo)`.
    pub semantics: Vec<lilac_hir::SemanticSite>,
}

impl Analysis {
    /// Takes the check's outputs and the top-level conditions' editor
    /// sites.
    fn new(
        resolved: lilac_resolve::Resolved,
        types: lilac_ty::Types,
        checked: lilac_typeck::Checked,
        conditions: conditional::Sites,
    ) -> Self {
        let lilac_typeck::Checked {
            mut hir,
            sigs,
            templates,
        } = checked;
        hir.tags.extend(conditions.tags);
        hir.members.extend(conditions.members);
        let namespaces = resolved
            .namespace_sites
            .iter()
            .map(|&span| lilac_hir::SemanticSite {
                span,
                kind: lilac_hir::SemanticKind::Namespace,
            });
        let semantics = merge_semantics(
            std::mem::take(&mut hir.semantics)
                .into_iter()
                .chain(conditions.semantics)
                .chain(namespaces),
        );
        Self {
            resolved,
            types,
            sigs,
            hir,
            templates,
            semantics,
        }
    }
}

/// What a cancelled compile stops with ([`Session::cancelled`]): no
/// diagnostic of its own, and it keeps no analysis.
fn stopped() -> ErrorGuaranteed {
    ErrorGuaranteed::unreported(Unreported::Cancelled)
}

/// Dedups semantic-token sites by `(span, kind)` and sorts them by
/// `(file, lo)`.
fn merge_semantics(
    sites: impl Iterator<Item = lilac_hir::SemanticSite>,
) -> Vec<lilac_hir::SemanticSite> {
    let mut sites: Vec<_> = sites.collect();
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
    /// The last compile's loaded files: identity to path.
    pub project_files: Vec<(FileId, PathBuf)>,
    /// Mounted libraries (`--lib Name=dir`): `using Name.X` loads `dir/X`.
    pub libraries: Vec<(String, PathBuf)>,
    /// The build folder, which a folder compile does not load as a
    /// namespace; `None` means `<root>/build`. A manifest names it
    /// (`lilac_manifest::Settings::apply`).
    pub build_dir: Option<PathBuf>,
    /// The last compile's analysis, when [`Session::keep_analysis`] keeps
    /// one and the compile got through type checking (a later failure
    /// keeps it).
    pub analysis: Option<Analysis>,
    /// What a compile keeps for editor tooling, and so whether it runs
    /// past errors.
    pub keep_analysis: Keep,
    /// Whether the unit is an executable (`lilac build`): it needs the three
    /// platform hooks (`E033`), its `@entry` must find what it calls
    /// (`E032`), and the entry stub `.lilac.start` is added as the root.
    pub require_entry: bool,
    /// How the program enters: [`Entry::Native`] (`lilac build` without
    /// `--libc`) enters at `.lilac.start`; [`Entry::CMain`] has the C
    /// `main` call it, and sets `@builtin.Target.libc`.
    pub entry: Entry,
    /// Called at the start and end of each pipeline [`Stage`].
    pub progress: Option<Progress>,
    /// Whether to instrument functions for panic stack traces
    /// (`lilac_ir::trace`); the CLI sets it from the mode and
    /// `--stack-traces`.
    pub stack_traces: bool,
    /// Cancels the compile when set, by whoever holds the other end (the
    /// language server, when a newer compile is due). The pipeline checks
    /// it between stages, before each function instance it type checks and
    /// before each function it lowers. A cancelled compile returns an error
    /// with no diagnostic of its own ([`Session::cancelled`] says why), its
    /// diagnostics are incomplete, and it keeps no analysis.
    pub cancel: Option<Arc<AtomicBool>>,
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
            project_files: Vec::new(),
            libraries: Vec::new(),
            build_dir: None,
            analysis: None,
            keep_analysis: Keep::Nothing,
            require_entry: false,
            entry: Entry::CMain,
            progress: None,
            stack_traces: false,
            cancel: None,
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
    /// verification, returning the verified module.
    pub fn compile_lir(&mut self, name: &str, src: &str) -> Result<LirModule, ErrorGuaranteed> {
        self.compile_lir_with_stages(name, src, &mut |_: &'static str, _: &LirModule| {})
    }

    /// Like [`Session::compile_lir`], but invokes `on_stage` at every pipeline
    /// boundary with the stage name and the module as it stands there. Stages,
    /// in order: `lower`, `runtime` (after the
    /// `.lilac.*` prelude is injected, before DCE), and `final` (the returned
    /// module). The driver does no file I/O itself.
    pub fn compile_lir_with_stages(
        &mut self,
        name: &str,
        src: &str,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        self.begin_load();
        let project = lilac_module::single_file(name, src, &mut self.source_map);
        self.run_loaded(&project, false, on_stage)
    }

    /// Compiles a folder project rooted at `root`, loading every file reachable
    /// through `using` via the injected [`Fs`]. Returns the verified
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
        self.begin_load();
        let (project, failed) = lilac_module::load_with_status(
            root,
            &self.libraries,
            self.build_dir.as_deref(),
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
        self.begin_load();
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

    /// Starts a compile: the last one's files and analysis go, and the Load
    /// stage begins.
    fn begin_load(&mut self) {
        self.project_files.clear();
        self.analysis = None;
        self.report(Stage::Load, StageEvent::Begin);
    }

    /// Records a loaded project's files, ends the Load stage, and runs the
    /// pipeline unless loading failed. Every compile goes through here
    /// after [`Session::begin_load`].
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
            return Err(self.diags.already_reported());
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
        self.begin_load();
        let (project, failed) = lilac_module::load_namespace(
            root,
            namespace,
            &self.libraries,
            fs,
            &mut self.source_map,
            &mut self.diags,
        );
        self.run_loaded(&project, failed, on_stage)
    }

    /// The last compile's loaded files, identities and paths; maps
    /// diagnostic spans to file paths.
    #[must_use]
    pub fn project_files(&self) -> &[(FileId, PathBuf)] {
        &self.project_files
    }

    /// The last compile's analysis ([`Session::analysis`]).
    #[must_use]
    pub fn analysis(&self) -> Option<&Analysis> {
        self.analysis.as_ref()
    }

    /// Whether [`Session::cancel`] is set: a compile that returned an
    /// error was cut short, not failed, and its diagnostics are incomplete.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
    }

    /// Calls the progress hook, if any.
    fn report(&mut self, stage: Stage, event: StageEvent) {
        if let Some(Progress(hook)) = &mut self.progress {
            hook(stage, event);
        }
    }

    /// Parse → resolve → check → lower → inject runtime → prune → live
    /// `@compile_error` check (with the reach chain) → trace (when
    /// [`Session::stack_traces`] is set) → verify. Strict mode (the CLI) stops at the
    /// first failing stage; under [`Keep::Partial`] (editor tooling) it keeps going
    /// through checking, so a broken program has a partial analysis.
    /// A set [`Session::cancel`] stops it at the next check.
    fn run(
        &mut self,
        project: &lilac_module::Project,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        const DONE: StageEvent = StageEvent::End { count: None };
        if self.cancelled() {
            return Err(stopped());
        }
        let strict = self.keep_analysis != Keep::Partial;
        let facts = builtin::Facts {
            target: self.target,
            entry: self.entry,
            mode: self.optimize,
        };
        let mut project = builtin::with_builtin(project, facts, &mut self.source_map);
        self.report(Stage::Parse, StageEvent::Begin);
        let (mut programs, failed) = lilac_module::parse_all(&project, &mut self.diags, strict);
        if failed && strict {
            return Err(self.diags.already_reported());
        }
        let condition_sites = conditional::select(
            &mut project,
            &mut programs,
            &mut self.diags,
            &self.source_map,
            self.target,
            self.cancel.as_ref(),
        );
        let project = &project;
        if self.cancelled() {
            return Err(stopped());
        }
        self.report(Stage::Parse, DONE);
        self.report(Stage::Resolve, StageEvent::Begin);
        let resolved = lilac_resolve::resolve(project, &programs, &mut self.diags);
        if self.cancelled() {
            return Err(stopped());
        }
        if self.diags.has_errors() && strict {
            return Err(self.diags.already_reported());
        }
        self.report(Stage::Resolve, DONE);
        self.report(Stage::Check, StageEvent::Begin);
        let mut types = lilac_ty::Types::new();
        let options = lilac_typeck::Options {
            target: self.target,
            require_entry: self.require_entry,
            cancel: self.cancel.clone(),
        };
        let checked = lilac_typeck::check(&resolved, &mut types, &mut self.diags, options);
        if self.cancelled() {
            return Err(stopped());
        }
        let result = self.lower_checked(&checked.hir, &resolved, &types, on_stage);
        // Kept even when a later stage failed; a cancelled compile's is
        // incomplete, so it is not.
        if self.keep_analysis != Keep::Nothing && !self.cancelled() {
            self.analysis = Some(Analysis::new(resolved, types, checked, condition_sites));
        }
        result
    }

    /// The pipeline past type checking: lower `hir` (unless checking
    /// failed), inject the runtime, prune, then the live checks, stack-trace
    /// instrumentation and verification.
    fn lower_checked(
        &mut self,
        hir: &lilac_hir::HirProgram,
        resolved: &lilac_resolve::Resolved,
        types: &lilac_ty::Types,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        const DONE: StageEvent = StageEvent::End { count: None };
        if self.diags.has_errors() {
            return Err(self.diags.already_reported());
        }
        lilac_lower::check_extern_signatures(hir, resolved, types, &mut self.diags);
        if self.require_entry {
            self.check_platform_hooks(resolved);
        }
        if self.diags.has_errors() {
            return Err(self.diags.already_reported());
        }
        self.report(
            Stage::Check,
            StageEvent::End {
                count: Some(hir.funcs.len()),
            },
        );
        self.report(Stage::Lower, StageEvent::Begin);
        let lowered = lilac_lower::lower(
            hir,
            resolved,
            types,
            &self.source_map,
            self.target,
            self.cancel.as_deref(),
        );
        if self.cancelled() {
            return Err(stopped());
        }
        lilac_lower::check_constant_indices(&lowered, &mut self.diags);
        lilac_lower::check_constant_divisions(&lowered, &mut self.diags);
        lilac_lower::check_global_initializers(&lowered, &mut self.diags);
        let mut lir = lowered.module;
        on_stage("lower", &lir);
        self.report(Stage::Lower, DONE);
        self.report(Stage::Runtime, StageEvent::Begin);
        self.inject_runtime(&mut lir);
        on_stage("runtime", &lir);
        self.report(Stage::Runtime, DONE);
        self.report(Stage::Prune, StageEvent::Begin);
        let mut roots: HashSet<String> = HashSet::new();
        if self.require_entry {
            roots.insert(".lilac.start".to_owned());
        }
        // Every `export` is kept, but only once something roots the module:
        // with no entry (a library checked alone) nothing is pruned.
        if self.require_entry || lir.funcs.iter().any(|func| func.name == "main") {
            roots.extend(
                lir.funcs
                    .iter()
                    .filter(|func| func.export.is_some())
                    .map(|func| func.name.as_str().to_owned()),
            );
        }
        lilac_ir::prune_unreachable(&mut lir, &roots);
        if self.cancelled() {
            return Err(stopped());
        }
        self.report(
            Stage::Prune,
            StageEvent::End {
                count: Some(lir.funcs.len()),
            },
        );
        self.report(Stage::Verify, StageEvent::Begin);
        lilac_ir::check_live_compile_errors(&lir, &lowered.names, &mut self.diags)?;
        lilac_ir::check_live_syscalls(
            &lir,
            self.target.has_syscall(),
            &lowered.names,
            &mut self.diags,
        )?;
        if self.stack_traces {
            lilac_ir::trace::instrument(&mut lir, &self.source_map, &lowered.names);
        }
        lilac_ir::verify(&lir, &mut self.diags)?;
        if self.diags.has_errors() {
            return Err(self.diags.already_reported());
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

    /// Renders the recorded diagnostics to one string: for a terminal the
    /// first [`SHOWN_ERRORS`] errors, separated by blank lines; as JSON,
    /// every one.
    #[must_use]
    pub fn render_diagnostics(&self) -> String {
        match self.opts.error_format {
            ErrorFormat::Human => {
                let human = self.human_emitter();
                self.diags
                    .shown(SHOWN_ERRORS)
                    .into_iter()
                    .map(|diagnostic| human.render(diagnostic, &self.source_map))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            ErrorFormat::Json => self
                .diags
                .diagnostics()
                .iter()
                .map(|diagnostic| EmitterJson.render(diagnostic, &self.source_map))
                .collect(),
        }
    }

    /// How many errors [`Self::render_diagnostics`] leaves out.
    #[must_use]
    pub fn hidden_errors(&self) -> usize {
        self.diags.error_count().saturating_sub(SHOWN_ERRORS)
    }

    /// The human closing line (`error: aborting due to …`), preceded by a
    /// blank line; empty for JSON or when nothing was reported.
    #[must_use]
    pub fn render_summary(&self) -> String {
        if self.opts.error_format == ErrorFormat::Json {
            return String::new();
        }
        let warnings = self
            .diags
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.level == Level::Warning)
            .count();
        let summary =
            self.human_emitter()
                .summary(self.diags.error_count(), self.hidden_errors(), warnings);
        if summary.is_empty() {
            summary
        } else {
            format!("\n{summary}")
        }
    }
}

impl Session {
    /// `E033` for each platform hook (`lilac_hir::PLATFORM_HOOKS`) the
    /// program does not define. An executable needs all three; duplicates
    /// are `E034` in the checker.
    fn check_platform_hooks(&mut self, resolved: &lilac_resolve::Resolved) {
        for (hook, _) in lilac_hir::PLATFORM_HOOKS {
            let defined = resolved.hooks.iter().any(|&id| {
                let def = resolved.def(id);
                def.name == hook && def.parent.is_none()
            });
            if !defined {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E033,
                        format!("the program defines no `{hook}`; an executable needs one"),
                    )
                    .note("a runtime library supplies `@entry`, `@exit` and `@compiler_print`")
                    .help("mount `Standard` (`--lib=Standard=<dir>`), or define the hook yourself"),
                );
            }
        }
    }

    /// Appends the runtime helpers, the panic handler `.lilac.panic` and its
    /// text (data, or templates when `@compiler_print` takes a format), and, for an executable, the entry stub `.lilac.start`. An
    /// `@exit` or `@compiler_print` hook the program does not define (only
    /// allowed outside an executable) is declared `extern` under its fixed
    /// symbol, so the panic handler links against whatever defines it.
    fn inject_runtime(&self, lir: &mut LirModule) {
        let message = lilac_ir::PanicMessage::of(lir);
        lir.funcs.extend(lilac_ir::build_runtime(&self.target));
        for hook in lilac_ir::hook_externs() {
            if !lir.funcs.iter().any(|func| func.name == hook.name) {
                lir.funcs.push(hook);
            }
        }
        lilac_ir::panic_runtime(message).add_to(lir);
        if self.require_entry {
            lir.funcs.push(lilac_ir::entry_point());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generated `@builtin.Target`/`Build` constants follow the session.
    #[test]
    fn builtin_sources_follow_the_target() {
        let facts = |triple: &str, entry, mode| builtin::Facts {
            target: Target::parse(triple).expect("triple"),
            entry,
            mode,
        };
        let [(_, types), (_, target), (_, build)] = builtin::sources(facts(
            "x86_64-pc-windows-msvc",
            Entry::Native,
            OptimizeMode::Release,
        ));
        assert!(types.contains("Os: enum {\n    Windows\n"), "{types}");
        assert!(types.contains("    X86_64\n"), "{types}");
        assert!(target.contains("os: Os => .Windows\n"), "{target}");
        assert!(target.contains("arch: Arch => .X86_64\n"), "{target}");
        assert!(
            target.contains("pointer_size: @bit_native => 0x8\n"),
            "{target}"
        );
        assert!(
            target.contains("page_size: @bit_native => 0x1000\n"),
            "{target}"
        );
        assert!(target.contains("libc: @bit8 => 0x0\n"), "{target}");
        assert!(build.contains("mode: Mode => .Release\n"), "{build}");

        let [_, (_, target), (_, build)] = builtin::sources(facts(
            "aarch64-apple-darwin",
            Entry::Native,
            OptimizeMode::Debug,
        ));
        assert!(target.contains("os: Os => .MacOs\n"), "{target}");
        assert!(
            target.contains("page_size: @bit_native => 0x4000\n"),
            "{target}"
        );
        assert!(target.contains("libc: @bit8 => 0x1\n"), "{target}");
        assert!(build.contains("mode: Mode => .Debug\n"), "{build}");

        let [_, (_, target), _] = builtin::sources(facts(
            "x86_64-unknown-linux-gnu",
            Entry::CMain,
            OptimizeMode::Debug,
        ));
        assert!(target.contains("libc: @bit8 => 0x1\n"), "{target}");

        let [_, (_, target), _] = builtin::sources(facts(
            "riscv32-unknown-none",
            Entry::Native,
            OptimizeMode::Debug,
        ));
        assert!(target.contains("os: Os => .None\n"), "{target}");
        assert!(
            target.contains("pointer_size: @bit_native => 0x4\n"),
            "{target}"
        );
    }

    /// A `match` on `@builtin.Target.os` compiles only the target's arm, and
    /// a `@compile_error` in a dead arm is silent.
    #[test]
    fn builtin_target_match_keeps_the_live_arm() {
        let src = "win: function () nothing {\n    @compile_error(\"dead\")\n}\nlin: function () nothing {}\nmain: function () nothing {\n    match @builtin.Target.os {\n        .Windows => win()\n        .Linux => lin()\n    }\n}\n";
        let mut session = Session::new();
        session.target = Target::parse("x86_64-unknown-linux-gnu").expect("triple");
        let text = session
            .compile_lir("m.lilac", src)
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()))
            .to_text();
        assert!(text.contains("call @.root.lin"), "{text}");
        assert!(!text.contains("@.root.win"), "{text}");

        let mut session = Session::new();
        session.target = Target::parse("x86_64-pc-windows-msvc").expect("triple");
        assert!(session.compile_lir("m.lilac", src).is_err());
        assert!(session.render_diagnostics().contains("error[E031]: dead"));
    }

    /// Two externs linking one symbol with one signature become one
    /// declaration under the link name; a variadic one prints `in ...`.
    #[test]
    fn externs_merge_by_link_name() {
        let src = "a: extern \"abs\" function (v: @bit32) r: @bit32\nb: extern \"abs\" function (x: @bit32) y: @bit32\nopen: extern function (path: ref @bit8, flags: @bit32, ...) fd: @bit32\nmain: function () nothing {\n    p: @bit8 = 0x0\n    m: @bit32 = 0x1b6\n    s: @bit32 = @i_add_32(a(0x1), b(0x2))\n    fd: @bit32 = open(ref p, 0x0, m)\n}\n";
        let mut session = Session::new();
        let text = session
            .compile_lir("e.lilac", src)
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()))
            .to_text();
        assert_eq!(text.matches("extern @abs\n").count(), 1, "{text}");
        assert_eq!(text.matches("call @abs").count(), 2, "{text}");
        assert!(!text.contains("@a "), "{text}");
        assert!(text.contains("extern @open\n"), "{text}");
        assert!(text.contains("  in ...\n"), "{text}");
    }

    /// Two externs linking one symbol with different signatures are `E028`,
    /// labelling both declarations.
    #[test]
    fn extern_link_name_mismatch_is_e028() {
        let src = "a: extern \"abs\" function (v: @bit32) r: @bit32\nb: extern \"abs\" function (v: @bit64) r: @bit64\nmain: function () nothing {}\n";
        let mut session = Session::new();
        assert!(session.compile_lir("e.lilac", src).is_err());
        let text = session.render_diagnostics();
        assert!(
            text.contains("error[E028]: `abs` is declared with two different signatures"),
            "{text}"
        );
        assert!(
            text.contains("first declared as `(@bit32) @bit32`"),
            "{text}"
        );
        assert!(
            text.contains("declared here as `(@bit64) @bit64`"),
            "{text}"
        );
    }

    /// C sees only pointers, so externs of one symbol may differ in pointee:
    /// they merge into one `ref #unknown` declaration and each call casts.
    #[test]
    fn externs_differing_in_pointee_merge() {
        let src = "Text: object {
    first: @bit8
}
a: extern \"puts\" function (s: ref @bit8) r: @bit32
b: extern \"puts\" function (s: ref Text) r: @bit32
main: function () nothing {
    t: Text = Text { first = 0x0 }
    x: @bit32 = a(ref t.first)
    y: @bit32 = b(ref t)
}
";
        let mut session = Session::new();
        let text = session
            .compile_lir("e.lilac", src)
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()))
            .to_text();
        assert_eq!(
            text.matches(
                "extern @puts
"
            )
            .count(),
            1,
            "{text}"
        );
        assert!(text.contains("in ref #unknown %s"), "{text}");
        assert_eq!(text.matches("= bitcast").count(), 2, "{text}");
    }

    /// A root function's symbol is `.root.`-prefixed, so one named like an
    /// extern's C symbol does not take it; `main` stays bare.
    #[test]
    fn root_function_does_not_take_an_extern_symbol() {
        let src = "release: extern \"free\" function (p: ref unknown) nothing
free: function () nothing {}
main: function () nothing {
    free()
    release(@address_to_ptr(0x0))
}
";
        let mut session = Session::new();
        let lir = session
            .compile_lir("e.lilac", src)
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let names: Vec<&str> = lir.funcs.iter().map(|func| func.name.as_str()).collect();
        assert!(names.contains(&".root.free"), "{names:?}");
        assert!(names.contains(&"free"), "{names:?}");
        assert!(names.contains(&"main"), "{names:?}");
    }

    /// A live `@linux_syscall` off Linux x86-64/aarch64 is `E035`, with the call
    /// chain; an unreachable one, or one on Linux, is fine.
    #[test]
    fn syscall_needs_linux() {
        let src = "call: function () r: @bit_native {
    r = @linux_syscall(0x27, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0)
}
main: function () nothing {
    r: @bit_native = call()
}
";
        let mut session = Session::new();
        session.target = Target::parse("x86_64-pc-windows-msvc").expect("triple");
        assert!(session.compile_lir("s.lilac", src).is_err());
        let text = session.render_diagnostics();
        assert!(
            text.contains(
                "error[E035]: `@linux_syscall` is not available on `x86_64-pc-windows-msvc`"
            ),
            "{text}"
        );
        assert!(text.contains("at main (s.lilac:5:22)"), "{text}");

        for triple in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
            let mut session = Session::new();
            session.target = Target::parse(triple).expect("triple");
            assert!(
                session.compile_lir("s.lilac", src).is_ok(),
                "{}",
                session.render_diagnostics()
            );
        }

        let dead = "call: function () r: @bit_native {
    r = @linux_syscall(0x27, 0x0, 0x0, 0x0, 0x0, 0x0, 0x0)
}
main: function () nothing {}
";
        let mut session = Session::new();
        session.target = Target::parse("x86_64-pc-windows-msvc").expect("triple");
        assert!(
            session.compile_lir("s.lilac", dead).is_ok(),
            "{}",
            session.render_diagnostics()
        );
    }

    /// A program that never names `@builtin` gets none of its types.
    #[test]
    fn unused_builtin_adds_nothing() {
        let mut session = Session::new();
        let text = session
            .compile_lir("hello.lilac", "main: function () nothing {\n}\n")
            .expect("compiles")
            .to_text();
        assert!(!text.contains("@builtin"), "{text}");
    }

    /// A cancelled compile stops with no diagnostic and keeps no analysis,
    /// not even one an earlier compile of the session left.
    #[test]
    fn a_cancelled_compile_stops_and_keeps_nothing() {
        let src = "main: function () nothing {\n    x: @bit8 = nope\n}\n";
        let mut session = Session::new();
        session.keep_analysis = Keep::Partial;
        assert!(session.compile_lir("c.lilac", src).is_err());
        assert!(session.analysis().is_some());
        assert!(!session.cancelled());

        let cancel = Arc::new(AtomicBool::new(true));
        let mut session = Session::new();
        session.keep_analysis = Keep::Partial;
        session.cancel = Some(Arc::clone(&cancel));
        assert!(session.compile_lir("c.lilac", src).is_err());
        assert!(session.cancelled());
        assert!(session.analysis().is_none());
        assert_eq!(
            session.diags.error_count(),
            0,
            "{}",
            session.render_diagnostics()
        );

        // An analysis from before the cancel is not left for a reader.
        let mut session = Session::new();
        session.keep_analysis = Keep::Partial;
        session.cancel = Some(Arc::new(AtomicBool::new(false)));
        assert!(session.compile_lir("c.lilac", src).is_err());
        assert!(session.analysis().is_some());
        session.cancel = Some(cancel);
        assert!(session.compile_lir("c.lilac", src).is_err());
        assert!(session.analysis().is_none());
    }

    /// A valid program that takes seconds to check in a debug build: 15
    /// levels of generics, each instantiating the next at two new argument
    /// types, so 32,767 instances (below typeck's instance budget), each
    /// body copying its argument 40 times.
    fn slow_program() -> String {
        use std::fmt::Write as _;
        let mut src =
            String::from("A[T]: object {\n    value: T\n}\n\nB[T]: object {\n    value: T\n}\n\n");
        for level in 1..15 {
            let _ = writeln!(src, "f{level}[T]: function (x0: T) nothing {{");
            for copy in 1..=40 {
                let _ = writeln!(src, "    x{copy}: T = x{}", copy - 1);
            }
            let next = level + 1;
            let _ = writeln!(src, "    f{next}[A[T]](A[T] {{ value = x40 }})");
            let _ = writeln!(src, "    f{next}[B[T]](B[T] {{ value = x40 }})\n}}\n");
        }
        src.push_str("f15[T]: function (x0: T) nothing {\n}\n\n");
        src.push_str("main: function () nothing {\n    f1[@bit32](0x1)\n}\n");
        src
    }

    /// Cancelling a running check stops it at the next instance: the
    /// [`slow_program`] stops soon after the token is set.
    #[test]
    fn cancelling_stops_a_slow_check() {
        let src = slow_program();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = Arc::clone(&cancel);
        let compile = std::thread::Builder::new()
            .stack_size(COMPILE_STACK_BYTES)
            .spawn(move || {
                let mut session = Session::new();
                session.cancel = Some(token);
                let result = session.compile_lir("slow.lilac", &src);
                (result.is_err(), session.cancelled())
            })
            .expect("spawns a compile thread");
        std::thread::sleep(std::time::Duration::from_millis(300));
        let set = std::time::Instant::now();
        cancel.store(true, Ordering::Relaxed);
        let (failed, cancelled) = compile.join().expect("the compile ends");
        assert!(failed && cancelled);
        assert!(
            set.elapsed() < std::time::Duration::from_secs(2),
            "stopped {:?} after the cancel",
            set.elapsed()
        );
    }

    #[test]
    fn hello_compiles() {
        let mut session = Session::new();
        let module = session
            .compile_lir("hello.lilac", "main: function () nothing {\n}\n")
            .expect("hello should compile");
        assert!(module.funcs.iter().any(|func| func.name == "main"));
        assert_eq!(session.render_diagnostics(), "");
    }

    /// `@panic` calls the handler with its own site, then ends the block.
    #[test]
    fn panic_lowers_to_the_handler_with_its_site() {
        let mut session = Session::new();
        let module = session
            .compile_lir(
                "p.lilac",
                "main: function () nothing {\n    @panic(\"stop\")\n}\n",
            )
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let text = module.to_text();
        assert!(text.contains("call @.lilac.panic"), "{text}");
        assert!(text.contains("unreachable"), "{text}");
        assert!(
            module
                .data
                .iter()
                .any(|datum| datum.bytes == b"  at main (p.lilac:2:5)\n"),
            "{text}"
        );
    }

    /// Stack traces off (the default) adds no frame; on, `main` links one.
    #[test]
    fn stack_traces_instrument_only_when_on() {
        let src = "main: function () nothing {\n    @panic(\"stop\")\n}\n";
        let mut off = Session::new();
        let text = off.compile_lir("p.lilac", src).expect("compiles").to_text();
        assert!(!text.contains("%.lilac.frame"), "{text}");
        let mut on = Session::new();
        on.stack_traces = true;
        let text = on.compile_lir("p.lilac", src).expect("compiles").to_text();
        assert!(text.contains("call @trace_head"), "{text}");
        assert!(text.contains("%.lilac.frame"), "{text}");
    }

    /// The handler and its data are pruned from a program that never panics.
    #[test]
    fn no_panic_no_handler() {
        let mut session = Session::new();
        let module = session
            .compile_lir("p.lilac", "main: function () nothing {\n}\n")
            .expect("compiles");
        assert!(module.funcs.iter().all(|func| func.name != ".lilac.panic"));
        assert!(module.data.is_empty());
    }

    #[test]
    fn analysis_is_retained_on_a_type_error() {
        let mut session = Session::new();
        session.keep_analysis = Keep::Analysis;
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
        session.keep_analysis = Keep::Partial;
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
        session.keep_analysis = Keep::Partial;
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
        session.keep_analysis = Keep::Analysis;
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
        session.keep_analysis = Keep::Analysis;
        let result = session.compile_lir(
            "ok.lilac",
            "pi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n",
        );
        assert!(result.is_ok(), "{:?}", session.render_diagnostics());
        let analysis = session.analysis().expect("analysis retained");
        assert!(!analysis.hir.uses.is_empty(), "{:?}", analysis.hir.uses);
    }

    /// The CLI keeps no analysis: with `Keep::Nothing` the passes'
    /// outputs go once lowered. A compile drops the last one's at its start,
    /// so one that stops before checking holds none.
    #[test]
    fn analysis_is_kept_only_when_asked() {
        let src = "pi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n";
        let mut session = Session::new();
        assert!(session.compile_lir("ok.lilac", src).is_ok());
        assert!(session.analysis().is_none());

        let mut session = Session::new();
        session.keep_analysis = Keep::Analysis;
        assert!(session.compile_lir("ok.lilac", src).is_ok());
        assert!(session.analysis().is_some());
        assert!(session
            .compile_lir("bad.lilac", "main: function ( {\n}\n")
            .is_err());
        assert!(session.analysis().is_none());
    }

    /// An uninstantiated generic's checked body is kept beside the HIR, not
    /// in it: only `funcs` is lowered.
    #[test]
    fn templates_are_kept_apart_from_the_lowered_functions() {
        let src = "same[T]: function (value: T) result: T {\n    result = value\n}\nmain: function () nothing {}\n";
        let mut session = Session::new();
        session.keep_analysis = Keep::Analysis;
        let lir = session
            .compile_lir("t.lilac", src)
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let analysis = session.analysis().expect("analysis kept");
        let [template] = analysis.templates.as_slice() else {
            panic!("one template: {:?}", analysis.templates);
        };
        assert_eq!(analysis.resolved.def(template.key.def).name, "same");
        assert!(analysis
            .hir
            .funcs
            .iter()
            .all(|func| func.key.def != template.key.def));
        assert!(!lir.to_text().contains("same"), "{}", lir.to_text());
    }

    #[test]
    fn semantic_analysis_populated_on_success_and_type_error() {
        let mut session = Session::new();
        session.keep_analysis = Keep::Analysis;
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
        session.keep_analysis = Keep::Analysis;
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
    /// reachable prelude functions (and no others): the `@byte_buffer_ptr`
    /// call keeps its `.lilac.` body alive.
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
        assert!(!names.contains(&".lilac.format_args_piece"), "{names:?}");
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

    /// The integer constants of the runtime function `name` in `module`.
    fn runtime_constants(module: &LirModule, name: &str) -> Vec<u64> {
        let func = module
            .funcs
            .iter()
            .find(|func| func.name == name)
            .unwrap_or_else(|| panic!("missing runtime function `{name}`"));
        func.blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter_map(|inst| match inst {
                lilac_ir::LirInst::Const {
                    value: lilac_ir::LirConst::Int(value),
                    ..
                } => Some(*value),
                _ => None,
            })
            .collect()
    }

    /// The driver builds the f-string readers for the session's target, so
    /// a part's stride is the target's. Regression (of the stride the old
    /// runtime baked): `riscv64` once got 32-bit offsets.
    #[test]
    fn format_readers_follow_the_target() {
        for (triple, want) in [
            ("riscv64-unknown-none", vec![24, 8]),
            ("riscv32-unknown-none", vec![12, 4]),
        ] {
            let mut session = Session::new();
            session.target = Target::parse(triple).expect("valid triple");
            let module = session
                .compile_lir(
                    "fs.lilac",
                    "Out: object {}\n\n@format: function (value: ref @byte_buffer, out: ref Out) nothing {}\n\nmain: function () nothing {\n    part: @byte_buffer = \"x\"\n    msg: @format_args(Out) = $\"a{part}b\"\n    first: @byte_buffer = @format_args_piece(msg, 0x0)\n}\n",
                )
                .unwrap_or_else(|_| panic!("{triple}: {}", session.render_diagnostics()));
            assert_eq!(
                runtime_constants(&module, ".lilac.format_args_piece"),
                want,
                "{triple}"
            );
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
        assert_eq!(names, ["lower", "runtime", "final"]);
        assert_ne!(
            texts[0].1, texts[1].1,
            "runtime injection must change the module"
        );
        assert_eq!(
            texts[2].1,
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

    /// A `@compile_error` reachable from `main` still reports `E031`, with
    /// the call chain from `main` as a note.
    #[test]
    fn reachable_compile_error_is_e031() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "live.lilac",
            "boom: function () nothing {\n    @compile_error(\"live\")\n}\n\nmain: function () nothing {\n    boom()\n}\n",
        );
        assert!(result.is_err());
        let rendered = session.render_diagnostics();
        assert!(rendered.contains("error[E031]"), "{rendered}");
        assert!(
            rendered.contains("reached from:")
                && rendered.contains("at boom (live.lilac:2:5)")
                && rendered.contains("at main (live.lilac:6:5)"),
            "{rendered}"
        );
    }

    /// Every live call site is traced, not only the shortest chain; a
    /// function whose callers are already listed is marked `[see above]`.
    #[test]
    fn compile_error_traces_every_call_site() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "many.lilac",
            "boom: function () nothing {\n    @compile_error(\"many\")\n}\n\nagain: function () nothing {\n    boom()\n    again()\n}\n\nmain: function () nothing {\n    boom()\n    again()\n}\n",
        );
        assert!(result.is_err());
        let rendered = session.render_diagnostics();
        let expected = "reached from:
            at boom (many.lilac:2:5)
              at again (many.lilac:6:5)
                at again (many.lilac:7:5) [see above]
                at main (many.lilac:12:5)
              at main (many.lilac:11:5) [see above]";
        assert!(rendered.contains(expected), "{rendered}");
        assert_eq!(rendered.matches("error[E031]").count(), 1, "{rendered}");
    }

    /// A function reached only through `addr.func` marks that frame.
    #[test]
    fn address_taken_compile_error_marks_the_edge() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "addr.lilac",
            "boom: function () nothing {\n    @compile_error(\"addr\")\n}\n\nmain: function () nothing {\n    f: ref function () nothing = ref boom\n}\n",
        );
        let rendered = session.render_diagnostics();
        assert!(result.is_err(), "{rendered}");
        assert!(
            rendered.contains("at main (addr.lilac:6:") && rendered.contains("[address taken]"),
            "{rendered}"
        );
    }

    /// An `export` roots the module: a `@compile_error` in one nothing
    /// calls still reports `E031`.
    #[test]
    fn exported_compile_error_is_e031() {
        let mut session = Session::new();
        let result = session.compile_lir(
            "api.lilac",
            "api: export function () nothing {\n    @compile_error(\"api\")\n}\n\nmain: function () nothing {\n}\n",
        );
        assert!(result.is_err());
        let rendered = session.render_diagnostics();
        assert!(rendered.contains("error[E031]"), "{rendered}");
        assert!(rendered.contains("at api (api.lilac:2:5)"), "{rendered}");
        assert!(!rendered.contains("at main"), "{rendered}");
    }

    /// An `export` survives pruning beside `main`, under its own symbol; a
    /// function nothing reaches does not.
    #[test]
    fn exports_are_roots() {
        let mut session = Session::new();
        let module = session
            .compile_lir(
                "lib.lilac",
                "api: export \"lib_api\" function (x: @bit32) y: @bit32 {\n    y = x\n}\n\ndead: function () nothing {\n}\n\nmain: function () nothing {\n}\n",
            )
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let api = module.funcs.iter().find(|func| func.name == ".root.api");
        let export = api.and_then(|func| func.export.as_ref());
        assert_eq!(export.map(|export| export.symbol.as_str()), Some("lib_api"));
        assert!(module.funcs.iter().all(|func| func.name != ".root.dead"));
    }

    /// The platform hooks a program can define itself.
    const HOOKS: &str = "@entry: function (count: @bit_native, arguments: ref unknown, environment: ref unknown) nothing {
    main()
}

                         @exit: function (code: @bit32) nothing {
}

                         @compiler_print: function (text: @byte_buffer) nothing {
}
";

    fn codes(session: &Session) -> Vec<&str> {
        session
            .diags
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect()
    }

    /// `require_entry` (set by `lilac build`) needs all three platform hooks:
    /// each missing one is `E033`. Other compiles need none.
    #[test]
    fn require_entry_needs_every_hook() {
        let main = "main: function () nothing {
}
";
        let mut session = Session::new();
        assert!(session.compile_lir("main.lilac", main).is_ok());

        let mut session = Session::new();
        session.require_entry = true;
        assert!(session.compile_lir("main.lilac", main).is_err());
        assert_eq!(codes(&session), ["E033", "E033", "E033"]);

        let mut session = Session::new();
        session.require_entry = true;
        let module = session
            .compile_lir(
                "main.lilac",
                &format!(
                    "{HOOKS}
{main}"
                ),
            )
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let names: Vec<&str> = module.funcs.iter().map(|func| func.name.as_str()).collect();
        for name in [".lilac.start", ".lilac.entry", ".lilac.exit", "main"] {
            assert!(names.contains(&name), "{name}: {names:?}");
        }
    }

    /// An `@entry` that calls a missing `main` is `E032` in an executable;
    /// elsewhere the hook is dropped silently, so a library checks.
    #[test]
    fn entry_without_main_is_e032_only_in_an_executable() {
        let lib = format!(
            "{HOOKS}
helper: function () nothing {{
}}
"
        );
        let mut session = Session::new();
        let module = session
            .compile_lir("lib.lilac", &lib)
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        assert!(module.funcs.iter().all(|func| func.name != ".lilac.entry"));

        let mut session = Session::new();
        session.require_entry = true;
        assert!(session.compile_lir("lib.lilac", &lib).is_err());
        let rendered = session.render_diagnostics();
        assert_eq!(codes(&session), ["E032"], "{rendered}");
        assert!(
            rendered.contains("the program has no `main`, which `@entry` calls"),
            "{rendered}"
        );
    }

    /// A second definition of a platform hook is `E034`, and a wrong
    /// signature or a member hook is `E022`.
    #[test]
    fn platform_hook_duplicates_and_shapes() {
        let mut session = Session::new();
        let src = "@exit: function (code: @bit32) nothing {
}

                   @exit: function (code: @bit32) nothing {
}
";
        assert!(session.compile_lir("dup.lilac", src).is_err());
        assert_eq!(codes(&session), ["E034"]);

        for src in [
            "@exit: function (code: @bit8) nothing {
}
",
            "@entry: function () code: @bit32 => 0x0
",
            "@compiler_print: function (text: ref @byte_buffer) nothing {
}
",
            "@compiler_print: function (text: @bit32) nothing {
}
",
            "Out: object {}
@compiler_print: function (text: ref @format_args(Out)) nothing {
}
",
            "Thing: object {
    @entry: function () nothing {
    }
}
",
        ] {
            let mut session = Session::new();
            assert!(session.compile_lir("shape.lilac", src).is_err(), "{src}");
            assert_eq!(codes(&session), ["E022"], "{src}");
        }
    }

    /// With a `@compiler_print` that takes a format, `@panic` takes one too
    /// (a literal, an f-string, or one value), the handler takes it, and a
    /// generated message (a division by zero) is a template with no holes.
    #[test]
    fn panics_take_what_compiler_print_takes() {
        let hooks = "Out: object {}
@format: function (value: ref @bit32, out: ref Out) nothing {}
@compiler_print: function (text: @format_args(Out)) nothing {
}
";
        let mut session = Session::new();
        let module = session
            .compile_lir(
                "p.lilac",
                &format!(
                    "{hooks}
main: function () nothing {{
    n: @bit32 = 0x7
    if n == 0x1 {{ @panic(\"literal\") }}
    if n == 0x2 {{ @panic($\"bad {{n}}\") }}
    if n == 0x3 {{ @panic(n) }}
    m: @bit32 = @i_div_u_32(0x1, n)
}}
"
                ),
            )
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let text = module.to_text();
        assert!(
            text.contains("in #format_args %msg"),
            "the handler takes a format: {text}"
        );
        assert!(text.contains("format \"division by zero\""), "{text}");
        assert!(text.contains("format \"panic: \""), "{text}");
        assert!(!text.contains("str = \"panic: \""), "{text}");

        // One `@compiler_print`, whatever its type.
        let mut session = Session::new();
        let src = format!("{hooks}@compiler_print: function (text: @byte_buffer) nothing {{\n}}\n");
        assert!(session.compile_lir("dup.lilac", &src).is_err());
        assert_eq!(codes(&session), ["E034"]);

        // A bytes hook keeps `@panic` a `@byte_buffer`.
        let mut session = Session::new();
        let src = "@compiler_print: function (text: @byte_buffer) nothing {
}
main: function () nothing {
    n: @bit32 = 0x7
    @panic(n)
}
";
        assert!(session.compile_lir("bytes.lilac", src).is_err());
        assert_eq!(codes(&session), ["E043"]);
    }

    /// Without `@exit` or `@compiler_print`, outside an executable, the panic
    /// handler calls them as `extern` hook declarations that keep the Lilac
    /// place ABI.
    #[test]
    fn missing_hooks_are_declared_for_the_panic_handler() {
        let mut session = Session::new();
        let module = session
            .compile_lir(
                "p.lilac",
                "main: function () nothing {
    @panic(\"no\")
}
",
            )
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        for name in [".lilac.exit", ".lilac.compiler_print"] {
            let hook = module
                .funcs
                .iter()
                .find(|func| func.name == name)
                .unwrap_or_else(|| panic!("{name} is declared"));
            assert!(hook.is_extern && !hook.uses_c_abi(), "{name}");
        }
    }

    /// The progress hook sees every stage begin and end, in pipeline order,
    /// with the load, check and prune counts.
    #[test]
    fn progress_reports_every_stage_in_order() {
        use std::sync::{Arc, Mutex};
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let mut session = Session::new();
        session.progress = Some(Progress::new(move |stage, event| {
            sink.lock().expect("unpoisoned").push((stage, event));
        }));
        session
            .compile_lir("p.lilac", "main: function () nothing {\n}\n")
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let events = events.lock().expect("unpoisoned");
        let stages = [
            Stage::Load,
            Stage::Parse,
            Stage::Resolve,
            Stage::Check,
            Stage::Lower,
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
        assert_eq!(events[13].1, StageEvent::End { count: Some(1) });
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

        /// No links: each path is its own canonical form.
        fn canonicalize(&mut self, path: &Path) -> std::io::Result<PathBuf> {
            if self.files.contains_key(path) || self.dirs.contains_key(path) {
                Ok(path.to_path_buf())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no such path",
                ))
            }
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

    /// A library namespace compiled alone loads like a project: the Load
    /// stage is reported with its file count, and its files are recorded.
    #[test]
    fn a_namespace_compile_reports_its_load() {
        use std::sync::{Arc, Mutex};
        let lib = PathBuf::from("lib");
        let file = lib.join("util.lilac");
        let mut fs = TestFs {
            files: [(file.clone(), "helper: function () nothing {}\n".to_owned())]
                .into_iter()
                .collect(),
            dirs: [(lib.clone(), vec![file.clone()])].into_iter().collect(),
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let mut session = Session::new();
        session.libraries = vec![("Util".to_owned(), lib)];
        session.progress = Some(Progress::new(move |stage, event| {
            sink.lock().expect("unpoisoned").push((stage, event));
        }));
        session
            .compile_namespace_with_stages(Path::new("proj"), "Util", &mut fs, &mut |_, _| {})
            .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
        let events = events.lock().expect("unpoisoned");
        assert_eq!(
            events[..2],
            [
                (Stage::Load, StageEvent::Begin),
                (Stage::Load, StageEvent::End { count: Some(1) }),
            ],
            "{events:?}"
        );
        assert_eq!(session.project_files(), [(FileId(0), file)]);
    }
}
