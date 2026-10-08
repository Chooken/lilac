//! The `Backend` trait: the frontend never imports LLVM directly.
//!
//! Mirrors `rustc_codegen_ssa`: backends implement one trait, the driver picks
//! with `--backend=` (LLVM today).

use lilac_ir::LirModule;

/// Optimization level for the LLVM pipeline (LLVM's `O0`..`O3`, `Os`).
///
/// Defined here so backend crates can take an optimization level without
/// depending on the driver. The driver's user-facing `OptimizeMode` maps onto
/// these in the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    /// No optimization.
    #[default]
    O0,
    /// Light optimization.
    O1,
    /// Moderate optimization.
    O2,
    /// Aggressive optimization.
    O3,
    /// Optimize for size.
    Os,
}

/// How a program enters: which entry symbol the backend emits around the
/// Lilac `main` (`.lilac.main`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    /// A C `i32 main()` wrapper, entered through the C runtime's startup code
    /// (`--libc` builds and `emit-llvm`).
    CMain,
    /// Lilac's own process entry, `.lilac.start`, with no C runtime.
    Native,
}

/// A step of object emission, reported to [`Backend::emit_object`]'s
/// callback before it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodegenStep {
    /// LIR to backend IR.
    Lower,
    /// Verifying the backend IR.
    Verify,
    /// The optimization pipeline.
    Optimize,
    /// Writing machine code.
    Emit,
}

/// Code-generation backend over verified [`LirModule`]s.
pub trait Backend {
    /// Backend name for `--backend=` selection.
    fn name(&self) -> &'static str;

    /// Compiles a verified module to an object file for the target, entered
    /// through `entry`. `on_step` is called before each [`CodegenStep`].
    ///
    /// Backends that cannot write objects keep the default error.
    fn emit_object(
        &self,
        _module: &LirModule,
        _entry: Entry,
        _on_step: &mut dyn FnMut(CodegenStep),
    ) -> Result<Vec<u8>, String> {
        Err(format!(
            "backend `{}` cannot emit object files",
            self.name()
        ))
    }

    /// Emits textual LLVM IR for a verified module.
    ///
    /// Backends that do not produce LLVM IR keep the default error, which names
    /// the backend so a `--backend=` mismatch is diagnosable.
    fn emit_llvm(&self, _module: &LirModule) -> Result<String, String> {
        Err(format!("backend `{}` cannot emit LLVM IR", self.name()))
    }

    /// Builds and runs a host executable (build scripts; post-v1).
    fn build_exe_and_run(&self, _module: &LirModule) -> Result<i32, String> {
        Err("build-script execution is post-v1".to_owned())
    }
}
