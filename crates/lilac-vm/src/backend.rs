//! The bytecode [`Backend`] and the standalone `.lbc` runner entry point.

use lilac_codegen::Backend;
use lilac_ir::LirModule;
use lilac_span::SourceMap;

use crate::compile::compile;
use crate::exec::Machine;
use crate::isa::Program;

/// The M5c code-generation backend: lowers verified LIR to canonical `.lbc`
/// text without touching LLVM.
pub struct VmBackend;

impl Backend for VmBackend {
    fn name(&self) -> &'static str {
        "vm"
    }

    /// The VM's real work happens in [`Backend::emit_bytecode`]; there is no
    /// separate lowering step to perform here.
    fn codegen(&self, _module: &LirModule) -> Result<(), String> {
        Ok(())
    }

    fn emit_bytecode(&self, module: &LirModule, source_map: &SourceMap) -> Result<String, String> {
        compile(module, source_map).map(|program| program.to_text())
    }
}

/// Parses, verifies, and runs a `.lbc` program to completion, entering at
/// `main`.
///
/// # Errors
///
/// Returns a human-readable message when the text is not a valid program or
/// when the machine traps at run time.
pub fn run_lbc(text: &str) -> Result<i32, String> {
    run_lbc_with_entry(text, None)
}

/// Parses, verifies, and runs a `.lbc` program to completion, entering at
/// `entry` when given and at `main` otherwise.
///
/// # Errors
///
/// Returns a human-readable message when the text is not a valid program, when
/// no function matches the entry, or when the machine traps at run time.
pub fn run_lbc_with_entry(text: &str, entry: Option<&str>) -> Result<i32, String> {
    let program = Program::parse_text(text)?;
    program.verify()?;
    let mut machine =
        Machine::with_entry(&program, entry.unwrap_or("main")).map_err(|err| err.to_string())?;
    machine.run().map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::{run_lbc, VmBackend};
    use crate::compile::compile_src;
    use crate::exec::VmError;
    use lilac_codegen::Backend as _;

    #[test]
    fn backend_name_is_vm() {
        assert_eq!(VmBackend.name(), "vm");
    }

    #[test]
    fn emit_bytecode_round_trips_through_run_lbc() {
        let program =
            compile_src("main: function () nothing {\n    x: @bit_native = @i_add(0x1, 0x2)\n}\n");
        let text = program.to_text();
        assert_eq!(run_lbc(&text), Ok(0));
    }

    #[test]
    fn run_lbc_reports_missing_entry() {
        let program = compile_src("helper: function () nothing {\n}\n");
        let err = run_lbc(&program.to_text()).expect_err("no main");
        assert_eq!(err, VmError::NoEntry("main".to_owned()).to_string());
    }

    #[test]
    fn run_lbc_rejects_malformed_text() {
        assert!(run_lbc("lbc = \"0.1\"\n").is_err());
    }
}
