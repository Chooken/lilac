//! Driver: `Session` plus pass orchestration.
//!
//! M0 runs lex + parse only. Later milestones insert passes here without
//! changing the shape: each pass is
//! `fn run(ctxt: &mut Ctxt, unit: &Unit) -> Result<(), ErrorGuaranteed>`.

use lilac_ast::Program;
use lilac_error::{Diags, Emitter, EmitterHuman, EmitterJson, ErrorGuaranteed};
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
    /// Full obligation trees (M4); currently no-op.
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

    /// Lexes, parses, resolves, and typechecks one source file (M2+ inserts
    /// lowering after checking here).
    pub fn compile_source(&mut self, name: &str, src: &str) -> Result<Program, ErrorGuaranteed> {
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
        lilac_typeck::check_bodies(&program, &module, &sigs, &mut types, &mut self.diags);
        if self.diags.has_errors() {
            return Err(ErrorGuaranteed::new());
        }
        Ok(program)
    }

    /// Renders all recorded diagnostics to one string.
    #[must_use]
    pub fn render_diagnostics(&self) -> String {
        let mut out = String::new();
        for diagnostic in self.diags.diagnostics() {
            let text = match self.opts.error_format {
                ErrorFormat::Human => EmitterHuman::new().render(diagnostic, &self.source_map),
                ErrorFormat::Json => EmitterJson.render(diagnostic, &self.source_map),
            };
            out.push_str(&text);
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
    fn allow_flag_silences_w020() {
        let mut session = Session::new();
        session.warn_on_operator_type_change = false;
        let result = session.compile_source("warn.lilac", MATRIX_ADD);
        assert!(result.is_ok(), "{}", session.render_diagnostics());
        assert_eq!(session.render_diagnostics(), "");
    }
}
