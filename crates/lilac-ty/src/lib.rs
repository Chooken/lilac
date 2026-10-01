//! Types: interner, `TypeRef`, signatures (real checker lands in M1).

use std::collections::HashMap;

use lilac_resolve::DefId;

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

/// What a `TypeId` stands for.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TypeKind {
    /// `@bit8` raw container.
    Bit8,
    /// `@bit16` raw container.
    Bit16,
    /// `@bit32` raw container.
    Bit32,
    /// `@bit64` raw container.
    Bit64,
    /// `@bitNative` word-sized container.
    BitNative,
    /// `@byte_buffer` borrowed byte view.
    ByteBuffer,
    /// `@format_args` fixed-capacity piece table.
    FormatArgs,
    /// `unknown` opaque type (only behind `ref`).
    Unknown,
    /// User-declared `object`.
    Object(DefId),
    /// Poison for error recovery; never a real type.
    Error,
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
    kinds: Vec<TypeKind>,
    objects: HashMap<DefId, TypeId>,
    object_names: HashMap<DefId, String>,
}

impl Types {
    /// Table with the 8 builtin types pre-interned.
    pub fn new() -> Self {
        let mut table = Self::default();
        table.kinds.extend([
            TypeKind::Bit8,
            TypeKind::Bit16,
            TypeKind::Bit32,
            TypeKind::Bit64,
            TypeKind::BitNative,
            TypeKind::ByteBuffer,
            TypeKind::FormatArgs,
            TypeKind::Unknown,
        ]);
        table
    }

    /// Looks up a builtin by Lilac spelling (`@bit32`, `@byte_buffer`, `unknown`).
    pub fn builtin(&self, name: &str) -> Option<TypeId> {
        let id = match name {
            "@bit8" => 0,
            "@bit16" => 1,
            "@bit32" => 2,
            "@bit64" => 3,
            "@bitNative" => 4,
            "@byte_buffer" => 5,
            "@format_args" => 6,
            "unknown" => 7,
            _ => return None,
        };
        Some(TypeId(id))
    }

    /// Interns (or finds) the type for a user `object`.
    pub fn intern_object(&mut self, def: DefId) -> TypeId {
        if let Some(&id) = self.objects.get(&def) {
            return id;
        }
        let id = TypeId(next_id(self.kinds.len()));
        self.kinds.push(TypeKind::Object(def));
        self.objects.insert(def, id);
        id
    }

    /// Records the source name of an object type for diagnostics.
    pub fn set_object_name(&mut self, def: DefId, name: &str) {
        self.object_names.insert(def, name.to_owned());
    }

    /// Reads back a kind (all `TypeId`s come from this table).
    pub fn kind(&self, id: TypeId) -> &TypeKind {
        &self.kinds[id.0 as usize]
    }

    /// Pretty-prints a type in Lilac syntax (`@bit32`, `Number`).
    pub fn display(&self, id: TypeId) -> String {
        match self.kind(id) {
            TypeKind::Bit8 => "@bit8".to_owned(),
            TypeKind::Bit16 => "@bit16".to_owned(),
            TypeKind::Bit32 => "@bit32".to_owned(),
            TypeKind::Bit64 => "@bit64".to_owned(),
            TypeKind::BitNative => "@bitNative".to_owned(),
            TypeKind::ByteBuffer => "@byte_buffer".to_owned(),
            TypeKind::FormatArgs => "@format_args".to_owned(),
            TypeKind::Unknown => "unknown".to_owned(),
            TypeKind::Object(def) => self
                .object_names
                .get(def)
                .cloned()
                .unwrap_or_else(|| format!("object#{}", def.0)),
            TypeKind::Error => "<error>".to_owned(),
        }
    }

    /// Pretty-prints a possibly-`ref` type (`ref @bit8`).
    pub fn display_ref(&self, ty: TypeRef) -> String {
        if ty.is_ref {
            format!("ref {}", self.display(ty.id))
        } else {
            self.display(ty.id)
        }
    }

    /// Allocates a fresh poison id (`Error` kind) for recovery.
    pub fn fresh(&mut self) -> TypeId {
        let id = TypeId(next_id(self.kinds.len()));
        self.kinds.push(TypeKind::Error);
        id
    }
}

/// Saturating `usize` -> `u32` for table indices.
fn next_id(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_resolve::DefId;

    #[test]
    fn builtins_resolve_by_name() {
        let types = Types::new();
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        let bit8 = types.builtin("@bit8").expect("has @bit8");
        assert_ne!(bit32, bit8);
        assert_eq!(types.builtin("@bit64"), types.builtin("@bit64"));
        assert_eq!(types.builtin("@nope"), None);
        assert_eq!(types.builtin("bit32"), None);
    }

    #[test]
    fn objects_intern_per_def_id() {
        let mut types = Types::new();
        let first = types.intern_object(DefId(1));
        let again = types.intern_object(DefId(1));
        let second = types.intern_object(DefId(2));
        assert_eq!(first, again);
        assert_ne!(first, second);
        assert!(matches!(types.kind(first), TypeKind::Object(DefId(1))));
    }

    #[test]
    fn display_prints_lilac_syntax() {
        let types = Types::new();
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        assert_eq!(types.display(bit32), "@bit32");
        assert_eq!(
            types.display_ref(TypeRef {
                id: bit32,
                is_ref: true
            }),
            "ref @bit32"
        );
    }
}
