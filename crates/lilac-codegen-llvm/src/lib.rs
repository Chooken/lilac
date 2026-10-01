//! LLVM backend behind [`Backend`]; real `inkwell` wiring lands in M5.
//!
//! This crate is the ONLY one allowed to link LLVM (enforced by CI grep gate).

use lilac_codegen::Backend;
use lilac_ir::LirModule;

/// LLVM backend placeholder (no LLVM linked in M0).
#[derive(Debug, Default)]
pub struct LlvmBackend;

impl LlvmBackend {
    /// Creates the backend.
    pub fn new() -> Self {
        Self
    }
}

impl Backend for LlvmBackend {
    fn name(&self) -> &'static str {
        "llvm"
    }

    fn codegen(&self, _module: &LirModule) -> Result<(), String> {
        Err("LLVM codegen lands in M5; M0 stops at `emit-ir`".to_owned())
    }
}
