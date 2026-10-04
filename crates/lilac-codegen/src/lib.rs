//! The `Backend` trait: the frontend never imports LLVM directly.
//!
//! Mirrors `rustc_codegen_ssa`: backends implement one trait, the driver picks
//! with `--backend=` (LLVM in M5, others later).

use lilac_ir::LirModule;

/// Optimization level for the LLVM pipeline (`-O0`..`-O3`, `-Os`).
///
/// Defined here so backend crates can take an optimization level without
/// depending on the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OptLevel {
    /// No optimization (`-O0`).
    #[default]
    O0,
    /// Light optimization (`-O1`).
    O1,
    /// Moderate optimization (`-O2`).
    O2,
    /// Aggressive optimization (`-O3`).
    O3,
    /// Optimize for size (`-Os`).
    Os,
}

/// Code-generation backend over verified [`LirModule`]s.
pub trait Backend {
    /// Backend name for `--backend=` selection.
    fn name(&self) -> &'static str;

    /// Lowers a verified module to the target.
    fn codegen(&self, module: &LirModule) -> Result<(), String>;

    /// Emits textual LLVM IR for a verified module (M5b).
    ///
    /// Backends that do not produce LLVM IR keep the default error.
    fn emit_llvm(&self, _module: &LirModule) -> Result<String, String> {
        Err("backend does not support LLVM IR emission".to_owned())
    }

    /// Builds and runs a host executable (build scripts; post-v1).
    fn build_exe_and_run(&self, _module: &LirModule) -> Result<i32, String> {
        Err("build-script execution is post-v1".to_owned())
    }
}
