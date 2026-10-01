//! HIR -> LIR lowering with zero-init, RAII, and conversions.
//!
//! Real lowering lands in M2 (control + IR freeze) and M3 (strings/folding).

use lilac_error::ErrorGuaranteed;
use lilac_hir::HirProgram;
use lilac_ir::LirModule;

/// Lowers typed HIR to Lilac IR (stub until M2).
pub fn lower(_hir: &HirProgram, target: &str) -> Result<LirModule, ErrorGuaranteed> {
    Ok(LirModule::new(target))
}
