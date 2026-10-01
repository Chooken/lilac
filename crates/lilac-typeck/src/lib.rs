//! Type checker: collect-sigs + check-bodies + duck-typed generics.
//!
//! Real checking lands in M1 (non-generic) and M4 (requirements/obligations).
//! Hook bindings must be concrete: blanket-generic hooks are rejected with
//! `E052` once checking exists.

pub mod check;
pub mod ops;
pub mod sigs;

pub use check::check_bodies;
pub use sigs::*;
