//! Driver: `Session` plus pass orchestration.
//!
//! M0 runs lex + parse only. Later milestones insert passes here without
//! changing the shape: each pass is
//! `fn run(ctxt: &mut Ctxt, unit: &Unit) -> Result<(), ErrorGuaranteed>`.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use lilac_ast::Program;
use lilac_error::{Diags, Emitter, EmitterHuman, EmitterJson, ErrorGuaranteed};
use lilac_ir::LirModule;
use lilac_lex::Lexer;
use lilac_module::Fs;
use lilac_parse::Parser;
use lilac_span::{FileId, SourceMap, Span};

pub use lilac_target::Target;

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
}

/// Per-compilation options.
#[derive(Clone, Debug, Default)]
pub struct Opts {
    /// Show full instantiation stacks (default elides past 5 frames).
    pub verbose: bool,
    /// Output format.
    pub error_format: ErrorFormat,
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
    /// Optimization level (read by the M5b LLVM pipeline; inert until then).
    pub optimize: OptimizeMode,
    /// Cache directory (reserved for the post-v1 content cache).
    pub cache_dir: Option<PathBuf>,
    /// Warns `W020` on operator shape changes (silenced by `--allow`).
    pub warn_on_operator_type_change: bool,
    /// Extra reachability roots (`--export=<name>`): functions to keep alive
    /// even when nothing calls them, so a library can expose entry points.
    pub exports: HashSet<String>,
}

impl Default for Session {
    /// Session with the real build host, the canonical default target,
    /// human errors, and operator-type-change warnings on. Kept identical to
    /// [`Session::new`] so neither construction path surprises the other.
    fn default() -> Self {
        Self {
            source_map: SourceMap::default(),
            diags: Diags::default(),
            host: Target::host(),
            target: Target::default(),
            opts: Opts::default(),
            optimize: OptimizeMode::default(),
            cache_dir: None,
            warn_on_operator_type_change: true,
            exports: HashSet::new(),
        }
    }
}

impl Session {
    /// Default session (real host, canonical default target, human errors).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs the full pipeline but returns the AST (`check` entry point;
    /// diagnostics render separately).
    pub fn compile_source(&mut self, name: &str, src: &str) -> Result<Program, ErrorGuaranteed> {
        let (program, _) =
            self.run_pipeline(name, src, &mut |_: &'static str, _: &LirModule| {})?;
        Ok(program)
    }

    /// Runs the full pipeline through IR verification, returning the folded
    /// module (`check` runs this too but prints diagnostics only).
    pub fn compile_lir(&mut self, name: &str, src: &str) -> Result<LirModule, ErrorGuaranteed> {
        let (_, module) = self.run_pipeline(name, src, &mut |_: &'static str, _: &LirModule| {})?;
        Ok(module)
    }

    /// Like [`Session::compile_lir`], but invokes `on_stage` at every pipeline
    /// boundary with the stage name and the module as it stands there. Stages,
    /// in order: `lower` (after lowering, **before** const folding), `fold`
    /// (after folding, before runtime injection), `runtime` (after the
    /// `.lilac.*` prelude is injected, before DCE), `final` (after pruning and
    /// verification — the returned module). Used for debugging; the driver does
    /// no file I/O itself.
    pub fn compile_lir_with_stages(
        &mut self,
        name: &str,
        src: &str,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        let (_, module) = self.run_pipeline(name, src, on_stage)?;
        Ok(module)
    }

    /// Compiles a whole folder project rooted at `root`, loading every file
    /// reachable through `using` via the injected [`Fs`] (the driver itself
    /// does no I/O). Returns the folded, verified module.
    ///
    /// Sources are registered in `Project::files` order so the loader's
    /// `FileId(index)` spans resolve against this session's [`SourceMap`].
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
        let project = lilac_module::load(root, fs, &mut self.diags)?;
        let mut programs: Vec<(FileId, Program)> = Vec::with_capacity(project.files.len());
        let mut merged = Program {
            items: Vec::new(),
            span: Span::dummy(),
        };
        for (index, unit) in project.files.iter().enumerate() {
            let file = self.source_map.add_file(&unit.name, &unit.src);
            let (tokens, lex_errors) = Lexer::lex_all(&unit.src, file);
            for error in lex_errors {
                self.diags.emit(error);
            }
            let mut parser = Parser::new(tokens, file);
            let program = parser.parse_program(&mut self.diags)?;
            merged.items.extend(program.items.iter().cloned());
            let id = FileId(u32::try_from(index).unwrap_or(u32::MAX));
            programs.push((id, program));
        }
        let resolved = lilac_resolve::collect_project(&project, &programs, &mut self.diags);
        self.finish_pipeline(&merged, &resolved, on_stage)
    }

    /// Lex → parse → `collect` → `collect_sigs` → `check_bodies` → `lower` →
    /// `fold` → inject runtime → `prune` → check live `@compile_error` →
    /// `verify`. Stops at the first failing pass. The live-error check runs
    /// after `prune` so errors in functions unreachable from `main` or an
    /// `--export` root are silent.
    /// `on_stage` fires at each reached boundary (see
    /// [`Session::compile_lir_with_stages`]).
    fn run_pipeline(
        &mut self,
        name: &str,
        src: &str,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<(Program, LirModule), ErrorGuaranteed> {
        let file = self.source_map.add_file(name, src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        for error in lex_errors {
            self.diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser.parse_program(&mut self.diags)?;
        let module = lilac_resolve::collect(&program, &mut self.diags);
        let project = lilac_resolve::ResolvedProject::single(module, file);
        let lir = self.finish_pipeline(&program, &project, on_stage)?;
        Ok((program, lir))
    }

    /// The backend half of the pipeline, shared by the single-file and project
    /// entry points: `collect_sigs` → `check_bodies` → `lower` → `fold` →
    /// inject runtime → `prune` → check live `@compile_error` → `verify`.
    fn finish_pipeline(
        &mut self,
        program: &Program,
        project: &lilac_resolve::ResolvedProject,
        on_stage: &mut dyn FnMut(&'static str, &LirModule),
    ) -> Result<LirModule, ErrorGuaranteed> {
        let mut types = lilac_ty::Types::new();
        let sigs = lilac_typeck::collect_sigs(
            project,
            program,
            &mut types,
            &mut self.diags,
            self.warn_on_operator_type_change,
        );
        let hir = lilac_typeck::check_bodies(program, project, &sigs, &mut types, &mut self.diags);
        if self.diags.has_errors() {
            return Err(ErrorGuaranteed::new());
        }
        let triple = self.target.to_triple();
        let mut lir = lilac_lower::lower(&hir, project, &types, &mut self.diags, &triple)?;
        on_stage("lower", &lir);
        lilac_const::fold(&mut lir, &mut self.diags)?;
        on_stage("fold", &lir);
        let ptr_width = self.target.bit_native_width() / 8;
        lir.funcs
            .extend(lilac_ir::runtime::build_runtime(ptr_width));
        on_stage("runtime", &lir);
        lilac_ir::prune_unreachable(&mut lir, &self.exports);
        lilac_const::check_live_compile_errors(&lir, &mut self.diags)?;
        lilac_ir::verify(&lir, &mut self.diags)?;
        if self.diags.has_errors() {
            return Err(ErrorGuaranteed::new());
        }
        on_stage("final", &lir);
        Ok(lir)
    }

    /// Renders all recorded diagnostics to one string.
    #[must_use]
    pub fn render_diagnostics(&self) -> String {
        let human = EmitterHuman {
            verbose: self.opts.verbose,
            ..EmitterHuman::new()
        };
        let mut out = String::new();
        for diagnostic in self.diags.diagnostics() {
            let text = match self.opts.error_format {
                ErrorFormat::Human => human.render(diagnostic, &self.source_map),
                ErrorFormat::Json => EmitterJson.render(diagnostic, &self.source_map),
            };
            out.push_str(&text);
        }
        if self.opts.error_format == ErrorFormat::Human && self.diags.suppressed_errors() > 0 {
            let _ = writeln!(
                out,
                "error: {} more error(s) not shown (limit {})",
                self.diags.suppressed_errors(),
                self.diags.max_errors
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_compiles() {
        let mut session = Session::new();
        let program = session
            .compile_source("hello.lilac", "main: function () nothing {\n}\n")
            .expect("hello should compile");
        assert_eq!(program.items.len(), 1);
        assert_eq!(session.render_diagnostics(), "");
    }

    #[test]
    fn broken_source_reports_e001() {
        let mut session = Session::new();
        let result = session.compile_source("bad.lilac", "main: function ( {\n}\n");
        assert!(result.is_err());
        assert!(session.render_diagnostics().contains("error[E001]"));
    }

    #[test]
    fn check_runs_typeck() {
        let mut session = Session::new();
        let result = session.compile_source(
            "bad.lilac",
            "main: function () nothing {\n    x: @bit8 = nope\n}\n",
        );
        assert!(result.is_err());
        assert!(session.render_diagnostics().contains("error[E021]"));
    }

    #[test]
    fn check_accepts_clean_typed_source() {
        let mut session = Session::new();
        let result = session.compile_source(
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
                "main: function () nothing {\n    x: @bit32 = 0x0\n    if x {\n    }\n}\n",
            )
            .expect("branch source should lower");
        assert!(module.to_text().contains("branch"), "{}", module.to_text());
    }

    #[test]
    fn check_runs_through_verify() {
        let mut session = Session::new();
        let result = session.compile_source(
            "ok.lilac",
            "main: function () nothing {\n    x: @bit32 = 0x1\n    if x {\n    }\n}\n",
        );
        assert!(result.is_ok(), "{}", session.render_diagnostics());
        assert_eq!(session.render_diagnostics(), "");
    }

    const MATRIX_ADD: &str = "Matrix: object {\n    value: @bit32\n}\n@add: function (lhs: Matrix, rhs: Matrix) result: @bit32 {\n    result = 0x0\n}\nmain: function () nothing {\n}\n";

    #[test]
    fn warnings_do_not_fail_check() {
        let mut session = Session::new();
        let result = session.compile_source("warn.lilac", MATRIX_ADD);
        assert!(result.is_ok(), "{}", session.render_diagnostics());
        let rendered = session.render_diagnostics();
        assert!(rendered.contains("warning[W020]"), "{rendered}");
        assert!(!rendered.contains("error["), "{rendered}");
    }

    #[test]
    fn generic_inline_control_flow_defers_without_ice() {
        // Regression: a generic `=>` body with control flow must defer (`E030`)
        // instead of caching a temp-bearing inline body and panicking in
        // lowering (`use of unassigned join temp`).
        let mut session = Session::new();
        let result = session.compile_lir(
            "gen.lilac",
            "pick[T]: function (x: T, c: @bit8) T => if c x else x\nmain: function () nothing {\n    y: @bit32 = pick[@bit32](0x1, 0x1)\n}\n",
        );
        assert!(result.is_err());
        assert!(session.render_diagnostics().contains("error[E030]"));
    }

    #[test]
    fn allow_flag_silences_w020() {
        let mut session = Session::new();
        session.warn_on_operator_type_change = false;
        let result = session.compile_source("warn.lilac", MATRIX_ADD);
        assert!(result.is_ok(), "{}", session.render_diagnostics());
        assert_eq!(session.render_diagnostics(), "");
    }

    #[test]
    fn e010_shows_both_sides() {
        let mut session = Session::new();
        let result = session.compile_source(
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
                    if dst == "stride" && ty == "#bit_native" =>
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
                "main: function () nothing {\n    x: @bit_native = @i_add(0x1, 0x2)\n}\n",
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
}
