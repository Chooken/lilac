//! Stable Lilac IR v2 types (printer + verifier land in M2).
//!
//! This crate is the stability boundary: frontend crates build it, backend
//! crates consume it, and nothing else links LLVM.

use lilac_span::Span;

/// IR version header; bump on any normative IR change.
pub const LILAC_IR_VERSION: &str = "0.2";

/// An IR module: versioned header plus per-function bodies (M2).
#[derive(Clone, Debug)]
pub struct LirModule {
    /// `lilac_ir = "..."` header value.
    pub version: String,
    /// `target = "..."` header value (fixes `@bitNative` width).
    pub target: String,
}

impl LirModule {
    /// Creates a module for `target` under the current IR version.
    #[must_use]
    pub fn new(target: &str) -> Self {
        Self {
            version: LILAC_IR_VERSION.to_owned(),
            target: target.to_owned(),
        }
    }

    /// Canonical header text.
    #[must_use]
    pub fn header_text(&self) -> String {
        format!(
            "lilac_ir = \"{}\"\ntarget = \"{}\"\n",
            self.version, self.target
        )
    }
}

impl Default for LirModule {
    fn default() -> Self {
        Self::new("x86_64-unknown-none")
    }
}

/// Verification stub: full `lilac-ir::verify` lands in M2.
#[must_use]
pub fn verify(_module: &LirModule, _span: Span) -> bool {
    true
}
