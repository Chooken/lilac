//! Name resolution: `DefId`, scopes, folder-modules (real pass lands in M1).

use lilac_span::Span;

/// HIR-level definition identity, stable across passes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DefId(pub u32);

impl DefId {
    /// Placeholder for error-recovery poison.
    pub const ERROR: Self = Self(u32::MAX);
}

/// Name-resolution context (stub until M1).
#[derive(Debug, Default)]
pub struct Resolver {
    next: u32,
}

impl Resolver {
    /// Empty resolver.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a fresh [`DefId`] for the item at `span`.
    pub fn fresh(&mut self, _span: Span) -> DefId {
        let id = DefId(self.next);
        self.next += 1;
        id
    }
}
