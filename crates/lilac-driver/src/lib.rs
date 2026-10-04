//! Driver: `Session` plus pass orchestration.
//!
//! M0 runs lex + parse only. Later milestones insert passes here without
//! changing the shape: each pass is
//! `fn run(ctxt: &mut Ctxt, unit: &Unit) -> Result<(), ErrorGuaranteed>`.

use std::fmt::Write as _;

use lilac_ast::Program;
use lilac_error::{Diags, Emitter, EmitterHuman, EmitterJson, ErrorGuaranteed};
use lilac_ir::LirModule;
use lilac_lex::Lexer;
use lilac_parse::Parser;
use lilac_span::SourceMap;

/// Compile target (fixes `@bitNative` width; host-vs-target split matters from M4).
#[derive(Clone, Debug)]
pub struct Target {
    /// Target triple, e.g. `x86_64-unknown-none`.
    pub triple: String,
}

impl Target {
    /// Native-host placeholder (real host detection lands in M4/M5).
    #[must_use]
    pub fn host() -> Self {
        Self {
            triple: "x86_64-unknown-none".to_owned(),
        }
    }

    /// `@bitNative` width in bits.
    #[must_use]
    pub fn bit_native_width(&self) -> u32 {
        if self.triple.starts_with("x86_64") {
            64
        } else {
            32
        }
    }
}

impl Default for Target {
    fn default() -> Self {
        Self::host()
    }
}

/// `--error-format=` selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ErrorFormat {
    /// `rustc`-style human output.
    #[default]
    Human,
    /// Newline-delimited JSON.
    Json,
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
#[derive(Debug, Default)]
pub struct Session {
    /// File contents.
    pub source_map: SourceMap,
    /// Collected diagnostics.
    pub diags: Diags,
    /// Host (build scripts run here).
    pub host: Target,
    /// Target (code runs here; fixes `@bitNative`).
    pub target: Target,
    /// Flags.
    pub opts: Opts,
    /// Warns `W020` on operator shape changes (silenced by `--allow`).
    pub warn_on_operator_type_change: bool,
}

impl Session {
    /// Default session (host == `x86_64` target, human errors).
    #[must_use]
    pub fn new() -> Self {
        Self {
            warn_on_operator_type_change: true,
            ..Self::default()
        }
    }

    /// Runs the full pipeline but returns the AST (`check` entry point;
    /// diagnostics render separately).
    pub fn compile_source(&mut self, name: &str, src: &str) -> Result<Program, ErrorGuaranteed> {
        let (program, _) = self.run_pipeline(name, src)?;
        Ok(program)
    }

    /// Runs the full pipeline through IR verification, returning the folded
    /// module (`check` runs this too but prints diagnostics only).
    pub fn compile_lir(&mut self, name: &str, src: &str) -> Result<LirModule, ErrorGuaranteed> {
        let (_, module) = self.run_pipeline(name, src)?;
        Ok(module)
    }

    /// Lex → parse → `collect` → `collect_sigs` → `check_bodies` → `lower` →
    /// `fold` → `verify`. Stops at the first failing pass.
    fn run_pipeline(
        &mut self,
        name: &str,
        src: &str,
    ) -> Result<(Program, LirModule), ErrorGuaranteed> {
        let file = self.source_map.add_file(name, src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        for error in lex_errors {
            self.diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser.parse_program(&mut self.diags)?;
        let module = lilac_resolve::collect(&program, &mut self.diags);
        let mut types = lilac_ty::Types::new();
        let sigs = lilac_typeck::collect_sigs(
            &module,
            &program,
            &mut types,
            &mut self.diags,
            self.warn_on_operator_type_change,
        );
        let hir = lilac_typeck::check_bodies(&program, &module, &sigs, &mut types, &mut self.diags);
        if self.diags.has_errors() {
            return Err(ErrorGuaranteed::new());
        }
        let triple = self.target.triple.clone();
        let mut lir = lilac_lower::lower(&hir, &module, &types, &mut self.diags, &triple)?;
        lilac_const::fold(&mut lir, &mut self.diags)?;
        lilac_ir::verify(&lir, &mut self.diags)?;
        if self.diags.has_errors() {
            return Err(ErrorGuaranteed::new());
        }
        Ok((program, lir))
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
}
