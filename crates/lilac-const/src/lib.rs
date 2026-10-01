//! Constant evaluation + folding on LIR (real evaluator lands in M2/M3).
//!
//! Foldable v1 set, fuel caps, and the miss-is-ok rule are fixed by plan §10:
//! a missed fold costs performance only, never correctness.

/// Fuel cap per call-eval (`MAX_FOLD_STEPS`).
pub const MAX_FOLD_STEPS: u64 = 100_000;

/// Recursion cap (`MAX_CALL_DEPTH`).
pub const MAX_CALL_DEPTH: u32 = 16;

/// Constant values (the full lattice lands in M3).
#[derive(Clone, Debug)]
pub enum ConstValue {
    /// Not constant (left as a runtime value).
    Unknown,
}
