//! The `Backend` trait: the frontend never imports LLVM directly.
//!
//! Mirrors `rustc_codegen_ssa`: backends implement one trait, the driver picks
//! with `--backend=` (LLVM in M5, others later).

use lilac_ir::LirModule;

/// Code-generation backend over verified [`LirModule`]s.
pub trait Backend {
    /// Backend name for `--backend=` selection.
    fn name(&self) -> &'static str;

    /// Lowers a verified module to the target.
    fn codegen(&self, module: &LirModule) -> Result<(), String>;

    /// Builds and runs a host executable (build scripts; post-v1).
    fn build_exe_and_run(&self, _module: &LirModule) -> Result<i32, String> {
        Err("build-script execution is post-v1".to_owned())
    }
}
