//! LLVM backend behind [`Backend`]; real lowering lands in later M5 tasks.
//!
//! This crate is the ONLY one allowed to link LLVM (enforced by CI grep gate).

use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{
    CodeModel, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use inkwell::OptimizationLevel;

use lilac_codegen::{Backend, OptLevel};
use lilac_ir::LirModule;

/// LLVM backend for a fixed target triple and optimization level.
#[derive(Debug)]
pub struct LlvmBackend {
    triple: String,
    optimize: OptLevel,
}

impl LlvmBackend {
    /// Creates the backend for `triple` at `optimize`.
    #[must_use]
    pub fn new(triple: &str, optimize: OptLevel) -> Self {
        Self {
            triple: triple.to_owned(),
            optimize,
        }
    }

    /// Maps the codegen [`OptLevel`] onto inkwell's optimization level.
    fn optimization_level(&self) -> OptimizationLevel {
        match self.optimize {
            OptLevel::O0 => OptimizationLevel::None,
            OptLevel::O1 => OptimizationLevel::Less,
            OptLevel::O2 | OptLevel::Os => OptimizationLevel::Default,
            OptLevel::O3 => OptimizationLevel::Aggressive,
        }
    }

    /// Builds a verified, empty module carrying the target triple and data
    /// layout. Real lowering populates it in later M5 tasks.
    fn empty_module<'ctx>(&self, context: &'ctx Context) -> Result<Module<'ctx>, String> {
        let module = context.create_module("lilac");
        let triple = TargetTriple::create(&self.triple);
        module.set_triple(&triple);

        Target::initialize_native(&InitializationConfig::default())?;
        let target = Target::from_triple(&triple)
            .map_err(|e| format!("unknown target triple `{}`: {e}", self.triple))?;
        let machine: TargetMachine = target
            .create_target_machine(
                &triple,
                "generic",
                "",
                self.optimization_level(),
                RelocMode::Default,
                CodeModel::Default,
            )
            .ok_or_else(|| format!("failed to create target machine for `{}`", self.triple))?;
        module.set_data_layout(&machine.get_target_data().get_data_layout());

        module
            .verify()
            .map_err(|e| format!("module verification failed: {e}"))?;
        Ok(module)
    }
}

impl Backend for LlvmBackend {
    fn name(&self) -> &'static str {
        "llvm"
    }

    fn codegen(&self, _module: &LirModule) -> Result<(), String> {
        Err("LLVM codegen lands in M5; M0 stops at `emit-ir`".to_owned())
    }

    fn emit_llvm(&self, _module: &LirModule) -> Result<String, String> {
        let context = Context::create();
        let module = self.empty_module(&context)?;
        Ok(module.print_to_string().to_string())
    }
}

#[cfg(test)]
mod tests {
    use crate as lilac_codegen_llvm;
    use lilac_codegen::Backend;

    #[test]
    fn default_emit_llvm_is_unsupported() {
        struct Noop;
        impl lilac_codegen::Backend for Noop {
            fn name(&self) -> &'static str {
                "noop"
            }
            fn codegen(&self, _m: &lilac_ir::LirModule) -> Result<(), String> {
                Ok(())
            }
        }
        let m = lilac_ir::LirModule::new("x86_64-pc-windows-msvc");
        assert!(Noop.emit_llvm(&m).is_err());
    }

    #[test]
    fn empty_module_emits_verifyable_ir() {
        let backend = lilac_codegen_llvm::LlvmBackend::new(
            "x86_64-pc-windows-msvc",
            lilac_codegen::OptLevel::O0,
        );
        let module = lilac_ir::LirModule::new("x86_64-pc-windows-msvc");
        let text = backend.emit_llvm(&module).expect("emits");
        assert!(
            text.contains("target triple = \"x86_64-pc-windows-msvc\""),
            "{text}"
        );
    }
}
