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

/// Code-generation backend over verified [`LirModule`]s.
pub trait Backend {
    /// Backend name for `--backend=` selection.
    fn name(&self) -> &'static str;

    /// Lowers a verified module to the target.
    fn codegen(&self, module: &LirModule) -> Result<(), String>;

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
