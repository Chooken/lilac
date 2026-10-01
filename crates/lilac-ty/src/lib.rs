//! Types: interner, `TypeRef`, signatures (real checker lands in M1).

/// Interned type identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeId(pub u32);

impl TypeId {
    /// Placeholder for error-recovery poison.
    pub const ERROR: Self = Self(u32::MAX);
}

/// A type plus its `ref` flag (never doubly flagged).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TypeRef {
    /// Base type.
    pub id: TypeId,
    /// Whether this is `ref <base>`.
    pub is_ref: bool,
}

/// Function signature (checked in M1).
#[derive(Clone, Debug, Default)]
pub struct FuncSig {
    /// Parameter types.
    pub params: Vec<TypeId>,
    /// Named return-slot types.
    pub returns: Vec<TypeId>,
}

/// Type table (real interner lands in M1).
#[derive(Debug, Default)]
pub struct Types {
    next: u32,
}

impl Types {
    /// Empty type table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a fresh [`TypeId`].
    pub fn fresh(&mut self) -> TypeId {
        let id = TypeId(self.next);
        self.next += 1;
        id
    }
}
