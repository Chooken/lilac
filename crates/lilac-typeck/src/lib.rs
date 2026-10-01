//! Type checker: collect-sigs + check-bodies + duck-typed generics.
//!
//! Real checking lands in M1 (non-generic) and M4 (requirements/obligations).
//! Hook bindings must be concrete: blanket-generic hooks are rejected with
//! `E052` once checking exists.

use lilac_error::{Diagnostic, ErrorGuaranteed};

/// Body-checking entry point (stub until M1).
pub fn check_bodies() -> Result<(), ErrorGuaranteed> {
    Ok(())
}

/// Builds the `E052` diagnostic for blanket-generic hooks (used from M1).
#[must_use]
pub fn generic_hook_error() -> Diagnostic {
    Diagnostic::error(
        "E052",
        "hook bindings must be concrete; generic hooks are rejected",
    )
}
