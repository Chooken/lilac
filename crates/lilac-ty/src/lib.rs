//! Types: the interner, `TypeRef`, and function signatures.

mod mangle;

use std::collections::HashMap;
use std::fmt::Write as _;

use lilac_resolve::DefId;

pub use mangle::{demangle, demangle_bounded, demangle_text, DEMANGLE_LIMIT};

/// Types in diagnostics and editor text are cut past this many bytes
/// ([`Types::display_bounded`]): a type nested `n` deep can be `2^n` long.
pub const DISPLAY_LIMIT: usize = 200;

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

impl TypeRef {
    /// Poison for error recovery.
    pub const ERROR: Self = Self {
        id: TypeId::ERROR,
        is_ref: false,
    };

    /// The by-value type `id`.
    #[must_use]
    pub fn value(id: TypeId) -> Self {
        Self { id, is_ref: false }
    }

    /// `ref id`.
    #[must_use]
    pub fn reference(id: TypeId) -> Self {
        Self { id, is_ref: true }
    }

    /// Whether this is poison.
    #[must_use]
    pub fn is_error(self) -> bool {
        self.id == TypeId::ERROR
    }

    /// The same base type without `ref`.
    #[must_use]
    pub fn deref(self) -> Self {
        Self::value(self.id)
    }
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
    /// `@bit_native` word-sized container.
    BitNative,
    /// `@fbit16` half-precision float.
    FBit16,
    /// `@fbit32` single-precision float.
    FBit32,
    /// `@fbit64` double-precision float.
    FBit64,
    /// `@fbit128` quad-precision float.
    FBit128,
    /// `@byte_buffer` borrowed byte view.
    ByteBuffer,
    /// `@format_args(W)`: an f-string, formatted into a `W` (two
    /// pointers: its template and its holes).
    FormatArgs {
        /// The type every formatter in it writes into.
        writer: TypeId,
    },
    /// `unknown` opaque type (only behind `ref`).
    Unknown,
    /// User-declared `structure`.
    Object(DefId),
    /// A declaration's generic parameter placeholder (`T` is index 0).
    /// Appears only in templates, never in concrete output.
    Param {
        /// Position in the declaration's parameter list.
        index: u32,
        /// Source name, for diagnostics.
        name: String,
    },
    /// An applied user type (`GenObject[@bit32]`).
    Instance {
        /// The generic declaration's `DefId`.
        base: DefId,
        /// Concrete or parameter arguments in order.
        args: Vec<TypeRef>,
    },
    /// `@array(elem, len)`: `len` by-value elements in a row.
    Array {
        /// Element type (never a `ref`).
        elem: TypeId,
        /// Element count.
        len: u64,
    },
    /// `function (params) returns` value type.
    Func {
        /// Parameter types in order.
        params: Vec<TypeRef>,
        /// Return-slot types in order.
        returns: Vec<TypeRef>,
        /// `else` outs (`function (…) T else E`): the function can fail,
        /// and these are what a failure gives. `else nothing` is an empty
        /// list. Only an alias's target and a reflection test name one; no
        /// value has such a type.
        failures: Option<Vec<TypeRef>>,
        /// `export function (…)`: a C function, called with the C ABI.
        c_abi: bool,
    },
    /// Poison for error recovery; never a real type.
    Error,
}

/// A builtin type. Each is interned first, in this order, in every type
/// table, so its [`TypeId`] is fixed ([`Builtin::id`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Builtin {
    /// `@bit8`.
    Bit8,
    /// `@bit16`.
    Bit16,
    /// `@bit32`.
    Bit32,
    /// `@bit64`.
    Bit64,
    /// `@bit_native`.
    BitNative,
    /// `@fbit16`.
    FBit16,
    /// `@fbit32`.
    FBit32,
    /// `@fbit64`.
    FBit64,
    /// `@fbit128`.
    FBit128,
    /// `@byte_buffer`.
    ByteBuffer,
    /// `unknown`.
    Unknown,
}

impl Builtin {
    /// Every builtin, in `TypeId` order.
    pub const ALL: [Self; 11] = [
        Self::Bit8,
        Self::Bit16,
        Self::Bit32,
        Self::Bit64,
        Self::BitNative,
        Self::FBit16,
        Self::FBit32,
        Self::FBit64,
        Self::FBit128,
        Self::ByteBuffer,
        Self::Unknown,
    ];

    /// Its spelling (`@bit32`, `unknown`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Bit8 => "@bit8",
            Self::Bit16 => "@bit16",
            Self::Bit32 => "@bit32",
            Self::Bit64 => "@bit64",
            Self::BitNative => "@bit_native",
            Self::FBit16 => "@fbit16",
            Self::FBit32 => "@fbit32",
            Self::FBit64 => "@fbit64",
            Self::FBit128 => "@fbit128",
            Self::ByteBuffer => "@byte_buffer",
            Self::Unknown => "unknown",
        }
    }

    /// What the type is, as Markdown for an editor's hover.
    #[must_use]
    pub fn doc(self) -> &'static str {
        match self {
            Self::Bit8 => "8 bits, with no sign or meaning of their own: the operation says how to read them. A `@bit8` truth value is `0x1` or `0x0`.",
            Self::Bit16 => "16 bits, with no sign or meaning of their own: the operation says how to read them.",
            Self::Bit32 => "32 bits, with no sign or meaning of their own: the operation says how to read them.",
            Self::Bit64 => "64 bits, with no sign or meaning of their own: the operation says how to read them.",
            Self::BitNative => "A pointer's width of bits (32 or 64, by target): sizes, lengths, indices and addresses.",
            Self::FBit16 => "An IEEE 754 half-precision (16-bit) float.",
            Self::FBit32 => "An IEEE 754 single-precision (32-bit) float.",
            Self::FBit64 => "An IEEE 754 double-precision (64-bit) float.",
            Self::FBit128 => "An IEEE 754 quadruple-precision (128-bit) float.",
            Self::ByteBuffer => "A borrowed run of bytes: a pointer and a length (`@byte_buffer_ptr`, `@byte_buffer_len`). A string literal is one.",
            Self::Unknown => "A type nothing is known of: only `ref unknown` exists, an address of anything (C's `void *`). Convert a `ref T` to and from it with `as`.",
        }
    }

    /// The builtin spelled `name`.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|builtin| builtin.name() == name)
    }

    /// Its type in every table.
    #[must_use]
    pub fn id(self) -> TypeId {
        TypeId(self as u32)
    }

    /// Its type, by value.
    #[must_use]
    pub fn value(self) -> TypeRef {
        TypeRef::value(self.id())
    }

    fn kind(self) -> TypeKind {
        match self {
            Self::Bit8 => TypeKind::Bit8,
            Self::Bit16 => TypeKind::Bit16,
            Self::Bit32 => TypeKind::Bit32,
            Self::Bit64 => TypeKind::Bit64,
            Self::BitNative => TypeKind::BitNative,
            Self::FBit16 => TypeKind::FBit16,
            Self::FBit32 => TypeKind::FBit32,
            Self::FBit64 => TypeKind::FBit64,
            Self::FBit128 => TypeKind::FBit128,
            Self::ByteBuffer => TypeKind::ByteBuffer,
            Self::Unknown => TypeKind::Unknown,
        }
    }
}

/// Function signature.
#[derive(Clone, Debug, Default)]
pub struct FuncSig {
    /// Parameter types in order.
    pub params: Vec<TypeRef>,
    /// Named return-slot types in order.
    pub returns: Vec<TypeRef>,
}

/// A function type's identity: parameters, returns, failure outs, C ABI.
type FuncKey = (Vec<TypeRef>, Vec<TypeRef>, Option<Vec<TypeRef>>, bool);

/// The type interner.
#[derive(Clone, Debug, Default)]
pub struct Types {
    kinds: Vec<TypeKind>,
    /// Per type, whether it mentions a generic parameter
    /// ([`Types::contains_param`]), decided once when it is interned: a type
    /// nested `n` deep is `n` entries here but can be a tree of `2^n` nodes
    /// (`P[T, T]` applied again and again), too many to walk.
    has_param: Vec<bool>,
    objects: HashMap<DefId, TypeId>,
    object_names: HashMap<DefId, String>,
    /// Each type alias, by its definition: the type it names.
    aliases: HashMap<DefId, TypeId>,
    /// The name a type displays as: the one alias that names it, `None`
    /// when two do ([`Types::add_alias`]).
    alias_names: HashMap<TypeId, Option<String>>,
    funcs: HashMap<FuncKey, TypeId>,
    params: HashMap<(u32, String), TypeId>,
    instances: HashMap<(DefId, Vec<TypeRef>), TypeId>,
    arrays: HashMap<(TypeId, u64), TypeId>,
    format_args: HashMap<TypeId, TypeId>,
}

impl Types {
    /// Table with the 11 builtin types pre-interned.
    pub fn new() -> Self {
        let mut table = Self::default();
        for builtin in Builtin::ALL {
            table.push(builtin.kind());
        }
        table
    }

    /// Appends a new type, recording whether it mentions a parameter (its
    /// parts are interned before it, so each is one lookup).
    fn push(&mut self, kind: TypeKind) -> TypeId {
        let has_param = match &kind {
            TypeKind::Param { .. } => true,
            TypeKind::Instance { args, .. } => args.iter().any(|arg| self.contains_param(arg.id)),
            TypeKind::Array { elem, .. } => self.contains_param(*elem),
            TypeKind::FormatArgs { writer } => self.contains_param(*writer),
            TypeKind::Func {
                params,
                returns,
                failures,
                ..
            } => params
                .iter()
                .chain(returns.iter())
                .chain(failures.iter().flatten())
                .any(|slot| self.contains_param(slot.id)),
            _ => false,
        };
        let id = TypeId(next_id(self.kinds.len()));
        self.kinds.push(kind);
        self.has_param.push(has_param);
        id
    }

    /// The builtin type spelled `name` (`@bit32`, `@byte_buffer`, `unknown`).
    pub fn builtin(&self, name: &str) -> Option<TypeId> {
        Builtin::named(name).map(Builtin::id)
    }

    /// Interns (or finds) the type for a user `structure`.
    pub fn intern_object(&mut self, def: DefId) -> TypeId {
        if let Some(&id) = self.objects.get(&def) {
            return id;
        }
        let id = self.push(TypeKind::Object(def));
        self.objects.insert(def, id);
        id
    }

    /// Interns (or finds) a generic parameter placeholder (`T` -> index 0).
    pub fn intern_param(&mut self, index: u32, name: &str) -> TypeId {
        if let Some(&id) = self.params.get(&(index, name.to_owned())) {
            return id;
        }
        let id = self.push(TypeKind::Param {
            index,
            name: name.to_owned(),
        });
        self.params.insert((index, name.to_owned()), id);
        id
    }

    /// Interns (or finds) an applied user type (`GenObject[@bit32]`).
    /// Canonical: equal `(base, args)` always yields the same `TypeId`.
    pub fn intern_instance(&mut self, base: DefId, args: Vec<TypeRef>) -> TypeId {
        let key = (base, args);
        if let Some(&id) = self.instances.get(&key) {
            return id;
        }
        let id = self.push(TypeKind::Instance {
            base: key.0,
            args: key.1.clone(),
        });
        self.instances.insert(key, id);
        id
    }

    /// Interns (or finds) `@array(elem, len)`.
    pub fn intern_array(&mut self, elem: TypeId, len: u64) -> TypeId {
        if let Some(&id) = self.arrays.get(&(elem, len)) {
            return id;
        }
        let id = self.push(TypeKind::Array { elem, len });
        self.arrays.insert((elem, len), id);
        id
    }

    /// Interns (or finds) `@format_args(writer)`.
    pub fn intern_format_args(&mut self, writer: TypeId) -> TypeId {
        if let Some(&id) = self.format_args.get(&writer) {
            return id;
        }
        let id = self.push(TypeKind::FormatArgs { writer });
        self.format_args.insert(writer, id);
        id
    }

    /// Rewrites `Param(i)` to `args[i]` throughout `ty`, recursively
    /// re-interning nested `Instance`/`Func` types. The outer `ref` flag is
    /// preserved (and never doubled). Each distinct type inside `ty` is
    /// rewritten once, and one without a parameter is kept as it is, so the
    /// cost is linear in the types `ty` names, however large its text.
    pub fn substitute(&mut self, ty: TypeRef, args: &[TypeRef]) -> TypeRef {
        self.substitute_in(ty, args, &mut HashMap::new())
    }

    /// [`Types::substitute`], with `done` holding each by-value type
    /// already rewritten. Recursion follows the type's nesting, which
    /// typeck bounds (`MAX_TYPE_NESTING`).
    fn substitute_in(
        &mut self,
        ty: TypeRef,
        args: &[TypeRef],
        done: &mut HashMap<TypeId, TypeRef>,
    ) -> TypeRef {
        if !self.contains_param(ty.id) {
            return ty;
        }
        let value = if let Some(&value) = done.get(&ty.id) {
            value
        } else {
            let value = match self.kind(ty.id).clone() {
                TypeKind::Param { index, .. } => {
                    args.get(index as usize).copied().unwrap_or(ty.deref())
                }
                TypeKind::Instance {
                    base,
                    args: inst_args,
                } => {
                    let inst_args = inst_args
                        .iter()
                        .map(|arg| self.substitute_in(*arg, args, done))
                        .collect();
                    TypeRef::value(self.intern_instance(base, inst_args))
                }
                TypeKind::Array { elem, len } => {
                    let elem = self.substitute_in(TypeRef::value(elem), args, done);
                    TypeRef::value(self.intern_array(elem.id, len))
                }
                TypeKind::FormatArgs { writer } => {
                    let writer = self.substitute_in(TypeRef::value(writer), args, done);
                    TypeRef::value(self.intern_format_args(writer.id))
                }
                TypeKind::Func {
                    params,
                    returns,
                    failures,
                    c_abi,
                } => {
                    let mut slots = |slots: &[TypeRef]| -> Vec<TypeRef> {
                        slots
                            .iter()
                            .map(|slot| self.substitute_in(*slot, args, done))
                            .collect()
                    };
                    let params = slots(&params);
                    let returns = slots(&returns);
                    let failures = failures.as_deref().map(slots);
                    TypeRef::value(self.intern_function(params, returns, failures, c_abi))
                }
                _ => ty.deref(),
            };
            done.insert(ty.id, value);
            value
        };
        TypeRef {
            id: value.id,
            is_ref: ty.is_ref || value.is_ref,
        }
    }

    /// Whether `id` mentions an unsubstituted generic parameter anywhere
    /// (`false` for poison).
    pub fn contains_param(&self, id: TypeId) -> bool {
        self.has_param.get(id.0 as usize).copied().unwrap_or(false)
    }

    fn object_name(&self, def: DefId) -> String {
        self.object_names
            .get(&def)
            .cloned()
            .unwrap_or_else(|| format!("structure#{}", def.0))
    }

    /// Interns (or finds) a `function (params) returns` type.
    pub fn intern_func(&mut self, params: Vec<TypeRef>, returns: Vec<TypeRef>) -> TypeId {
        self.intern_func_abi(params, returns, false)
    }

    /// Interns (or finds) a function type, an `export function` one (C
    /// ABI) when `c_abi`.
    pub fn intern_func_abi(
        &mut self,
        params: Vec<TypeRef>,
        returns: Vec<TypeRef>,
        c_abi: bool,
    ) -> TypeId {
        self.intern_function(params, returns, None, c_abi)
    }

    /// Interns (or finds) a function type that can fail when `failures`
    /// is given (`function (…) T else E`), an `export function` one when
    /// `c_abi`.
    pub fn intern_function(
        &mut self,
        params: Vec<TypeRef>,
        returns: Vec<TypeRef>,
        failures: Option<Vec<TypeRef>>,
        c_abi: bool,
    ) -> TypeId {
        let key = (params, returns, failures, c_abi);
        if let Some(&id) = self.funcs.get(&key) {
            return id;
        }
        let id = self.push(TypeKind::Func {
            params: key.0.clone(),
            returns: key.1.clone(),
            failures: key.2.clone(),
            c_abi,
        });
        self.funcs.insert(key, id);
        id
    }

    /// Records the type alias `def`, named `name`, of `id`. A type written
    /// with arguments (an instance, an array or a function type) that
    /// exactly one alias names displays as that alias; a plain name never
    /// does, so an alias of `@bit8` leaves every `@bit8` as written.
    /// Symbols and layouts never see aliases: an alias is its type.
    pub fn add_alias(&mut self, def: DefId, name: &str, id: TypeId) {
        if self.aliases.insert(def, id).is_some() {
            return;
        }
        let spelled_with_arguments = matches!(
            self.kind(id),
            TypeKind::Instance { .. }
                | TypeKind::Array { .. }
                | TypeKind::Func { .. }
                | TypeKind::FormatArgs { .. }
        );
        if !spelled_with_arguments {
            return;
        }
        self.alias_names
            .entry(id)
            .and_modify(|shown| *shown = None)
            .or_insert_with(|| Some(name.to_owned()));
    }

    /// The type the alias `def` names, once typeck resolved it.
    pub fn alias(&self, def: DefId) -> Option<TypeId> {
        self.aliases.get(&def).copied()
    }

    /// Records the source name of an object type for diagnostics.
    pub fn set_object_name(&mut self, def: DefId, name: &str) {
        self.object_names.insert(def, name.to_owned());
    }

    /// Reads back a kind. Total: `TypeId::ERROR` (which is never interned)
    /// maps to `Error` instead of panicking, so poison is always cheap.
    pub fn kind(&self, id: TypeId) -> &TypeKind {
        if id == TypeId::ERROR {
            return &POISON_KIND;
        }
        &self.kinds[id.0 as usize]
    }

    /// Whether `ty` is a reference that is never dereferenced: `ref
    /// unknown`, which points at nothing known, and a function reference
    /// (`ref function (…)`), which points at code. Neither auto-derefs, has
    /// fields, or is read or written through.
    pub fn is_opaque_ref(&self, ty: TypeRef) -> bool {
        ty.is_ref && matches!(self.kind(ty.id), TypeKind::Unknown | TypeKind::Func { .. })
    }

    /// Whether `id` is a function type (`function (…)`, either ABI).
    pub fn is_func(&self, id: TypeId) -> bool {
        matches!(self.kind(id), TypeKind::Func { .. })
    }

    /// The type a read of `ty` gives where a value is wanted (an `if` or
    /// `match` value, an auto-deref): the pointee of a `ref`, except an
    /// opaque reference ([`Self::is_opaque_ref`]), which is read as itself.
    pub fn read_type(&self, ty: TypeRef) -> TypeRef {
        if self.is_opaque_ref(ty) {
            ty
        } else {
            ty.deref()
        }
    }

    /// The type a value of `id` has: `unknown` and a function type live
    /// only behind `ref` (`ref unknown`, `ref function (…)`), every other
    /// type by value. An expected type travels as a [`TypeId`], so this
    /// restores the `ref`.
    pub fn value_type(&self, id: TypeId) -> TypeRef {
        if self.is_opaque_ref(TypeRef::reference(id)) {
            TypeRef::reference(id)
        } else {
            TypeRef::value(id)
        }
    }

    /// Pretty-prints a type in Lilac syntax (`@bit32`, `Number`), cut at
    /// [`DISPLAY_LIMIT`].
    pub fn display(&self, id: TypeId) -> String {
        self.display_bounded(TypeRef::value(id), DISPLAY_LIMIT)
    }

    /// Pretty-prints a possibly-`ref` type (`ref @bit8`), cut at
    /// [`DISPLAY_LIMIT`].
    pub fn display_ref(&self, ty: TypeRef) -> String {
        self.display_bounded(ty, DISPLAY_LIMIT)
    }

    /// The text of `ty` cut to `limit` bytes, ending in `...` when cut. It
    /// stops walking there, so it is cheap for a type whose full text would
    /// be huge (each level of `P[T, T]` doubles it): every display is
    /// bounded, and a type's identity in symbols is [`Types::mangle`].
    pub fn display_bounded(&self, ty: TypeRef, limit: usize) -> String {
        let mut out = String::new();
        self.write_type(ty, &mut out, limit);
        if out.len() > limit {
            let mut end = limit;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
            out.push_str("...");
        }
        out
    }

    /// Writes `ty`'s text to `out`, stopping once `out` is past `limit`.
    fn write_type(&self, ty: TypeRef, out: &mut String, limit: usize) {
        if out.len() > limit {
            return;
        }
        if ty.is_ref {
            out.push_str("ref ");
        }
        if let Some(Some(name)) = self.alias_names.get(&ty.id) {
            out.push_str(name);
            return;
        }
        let list = |types: &Self, slots: &[TypeRef], out: &mut String| {
            for (index, slot) in slots.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                types.write_type(*slot, out, limit);
                if out.len() > limit {
                    return;
                }
            }
        };
        match self.kind(ty.id) {
            TypeKind::Object(def) => out.push_str(&self.object_name(*def)),
            TypeKind::Param { name, .. } => out.push_str(name),
            TypeKind::Instance { base, args } => {
                out.push_str(&self.object_name(*base));
                out.push('[');
                list(self, args, out);
                out.push(']');
            }
            TypeKind::Array { elem, len } => {
                out.push_str("@array(");
                self.write_type(TypeRef::value(*elem), out, limit);
                // Writing to a `String` cannot fail.
                let _ = write!(out, ", {len})");
            }
            TypeKind::Func {
                params,
                returns,
                failures,
                c_abi,
            } => {
                if *c_abi {
                    out.push_str("export ");
                }
                out.push_str("function (");
                list(self, params, out);
                out.push_str(") ");
                let outs = |slots: &[TypeRef], out: &mut String| match slots.len() {
                    0 => out.push_str("nothing"),
                    1 => list(self, slots, out),
                    // More than one out is parenthesized, as written.
                    _ => {
                        out.push('(');
                        list(self, slots, out);
                        out.push(')');
                    }
                };
                outs(returns, out);
                if let Some(failures) = failures {
                    out.push_str(" else ");
                    outs(failures, out);
                }
            }
            TypeKind::Error => out.push_str("<error>"),
            TypeKind::Bit8 => out.push_str("@bit8"),
            TypeKind::Bit16 => out.push_str("@bit16"),
            TypeKind::Bit32 => out.push_str("@bit32"),
            TypeKind::Bit64 => out.push_str("@bit64"),
            TypeKind::BitNative => out.push_str("@bit_native"),
            TypeKind::FBit16 => out.push_str("@fbit16"),
            TypeKind::FBit32 => out.push_str("@fbit32"),
            TypeKind::FBit64 => out.push_str("@fbit64"),
            TypeKind::FBit128 => out.push_str("@fbit128"),
            TypeKind::ByteBuffer => out.push_str("@byte_buffer"),
            TypeKind::FormatArgs { writer } => {
                out.push_str("@format_args(");
                self.write_type(TypeRef::value(*writer), out, limit);
                out.push(')');
            }
            TypeKind::Unknown => out.push_str("unknown"),
        }
    }
}

/// Saturating `usize` -> `u32` for table indices.
fn next_id(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// Shared `Error` kind for [`Types::kind`] on poison ids.
static POISON_KIND: TypeKind = TypeKind::Error;

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_resolve::DefId;

    #[test]
    fn float_builtins_intern_parse_mangle_and_display() {
        let types = Types::new();
        for (name, mangle, display) in [
            ("@fbit16", "[k]", "@fbit16"),
            ("@fbit32", "[f]", "@fbit32"),
            ("@fbit64", "[d]", "@fbit64"),
            ("@fbit128", "[q]", "@fbit128"),
        ] {
            let id = types.builtin(name).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(types.mangle(id), mangle);
            assert_eq!(types.display(id), display);
        }
    }

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
    fn each_builtin_is_its_own_kind() {
        let types = Types::new();
        for builtin in Builtin::ALL {
            assert_eq!(Builtin::named(builtin.name()), Some(builtin));
            assert_eq!(types.kind(builtin.id()), &builtin.kind());
            assert_eq!(types.display(builtin.id()), builtin.name());
        }
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

    #[test]
    fn func_types_intern_and_display() {
        let mut types = Types::new();
        let bit8 = types.builtin("@bit8").expect("has @bit8");
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        let params = vec![TypeRef {
            id: bit8,
            is_ref: false,
        }];
        let returns = vec![TypeRef {
            id: bit32,
            is_ref: false,
        }];
        let one = types.intern_func(params.clone(), returns.clone());
        let again = types.intern_func(params, returns);
        assert_eq!(one, again);
        assert_eq!(types.display(one), "function (@bit8) @bit32");
        let nullary = types.intern_func(Vec::new(), Vec::new());
        assert_ne!(one, nullary);
        assert_eq!(types.display(nullary), "function () nothing");
        // Failure outs are part of the type, and substitution walks them.
        let param = TypeRef::value(types.intern_param(0, "T"));
        let failing = types.intern_function(Vec::new(), Vec::new(), Some(vec![param]), false);
        assert_ne!(failing, nullary);
        assert!(types.contains_param(failing));
        assert_eq!(types.display(failing), "function () nothing else T");
        let done = types.substitute(TypeRef::value(failing), &[TypeRef::value(bit8)]);
        assert_eq!(types.display(done.id), "function () nothing else @bit8");
        let silent = types.intern_function(Vec::new(), Vec::new(), Some(Vec::new()), false);
        assert_eq!(types.display(silent), "function () nothing else nothing");
    }

    #[test]
    fn arrays_intern_display_mangle_and_substitute() {
        let mut types = Types::new();
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        let array = types.intern_array(bit32, 4);
        assert_eq!(types.intern_array(bit32, 4), array);
        assert_ne!(types.intern_array(bit32, 5), array);
        assert_eq!(types.display(array), "@array(@bit32, 4)");
        assert_eq!(types.mangle(array), "[Am3_]");
        let param = types.intern_param(0, "T");
        let template = types.intern_array(param, 4);
        assert!(types.contains_param(template));
        assert_eq!(
            types.substitute(TypeRef::reference(template), &[TypeRef::value(bit32)]),
            TypeRef::reference(array)
        );
    }

    #[test]
    fn param_interns_dedups_and_displays() {
        let mut types = Types::new();
        let first = types.intern_param(0, "T");
        let again = types.intern_param(0, "T");
        assert_eq!(first, again);
        assert_eq!(types.display(first), "T");
    }

    #[test]
    fn instance_interns_and_dedups() {
        let mut types = Types::new();
        let bit8 = types.builtin("@bit8").expect("has @bit8");
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        let arg = TypeRef {
            id: bit32,
            is_ref: false,
        };
        let first = types.intern_instance(DefId(1), vec![arg]);
        let again = types.intern_instance(DefId(1), vec![arg]);
        assert_eq!(first, again);
        let other = types.intern_instance(
            DefId(1),
            vec![TypeRef {
                id: bit8,
                is_ref: false,
            }],
        );
        assert_ne!(first, other);
    }

    #[test]
    fn instance_mangle_nests_regions() {
        let mut types = Types::new();
        types.set_object_name(DefId(1), "GenObj");
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        let arg = TypeRef {
            id: bit32,
            is_ref: false,
        };
        let outer = types.intern_instance(DefId(1), vec![arg]);
        assert_eq!(types.mangle(outer), "GenObj[m]");
        let nested = types.intern_instance(
            DefId(1),
            vec![TypeRef {
                id: outer,
                is_ref: false,
            }],
        );
        // An argument is written inside the region: `I`, its path, its
        // arguments, `E`.
        assert_eq!(types.mangle(nested), "GenObj[I6GenObjmE]");
    }

    #[test]
    fn instance_over_native_mangles_snake() {
        let mut types = Types::new();
        types.set_object_name(DefId(1), "GenObj");
        let native = types.builtin("@bit_native").expect("has @bit_native");
        let arg = TypeRef {
            id: native,
            is_ref: false,
        };
        let outer = types.intern_instance(DefId(1), vec![arg]);
        assert_eq!(types.mangle(outer), "GenObj[j]");
    }

    #[test]
    fn underscore_in_base_name_is_not_a_separator() {
        // A user object may contain `_`; the readable part keeps it, and
        // inside a region an identifier is length-prefixed.
        let mut types = Types::new();
        types.set_object_name(DefId(1), "Gen_Obj");
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        let arg = TypeRef {
            id: bit32,
            is_ref: false,
        };
        let instance = types.intern_instance(DefId(1), vec![arg]);
        assert_eq!(types.mangle(instance), "Gen_Obj[m]");
        let inside = types.intern_instance(DefId(2), vec![TypeRef::value(instance)]);
        types.set_object_name(DefId(2), "_Wrap");
        assert_eq!(types.mangle(inside), "_Wrap[I7Gen_ObjmE]");
    }

    #[test]
    fn substitute_rewrites_params_and_nested_instances() {
        let mut types = Types::new();
        types.set_object_name(DefId(1), "GenObj");
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        let bit32_arg = TypeRef {
            id: bit32,
            is_ref: false,
        };
        let param0 = types.intern_param(0, "T");
        let param_arg = TypeRef {
            id: param0,
            is_ref: false,
        };

        assert_eq!(types.substitute(param_arg, &[bit32_arg]), bit32_arg);
        assert_eq!(
            types.substitute(
                TypeRef {
                    id: param0,
                    is_ref: true
                },
                &[bit32_arg]
            ),
            TypeRef {
                id: bit32,
                is_ref: true
            }
        );

        let generic = types.intern_instance(DefId(1), vec![param_arg]);
        let concrete = types.intern_instance(DefId(1), vec![bit32_arg]);
        assert_eq!(
            types.substitute(
                TypeRef {
                    id: generic,
                    is_ref: false
                },
                &[bit32_arg]
            ),
            TypeRef {
                id: concrete,
                is_ref: false
            }
        );

        let template = types.intern_func(
            vec![TypeRef {
                id: param0,
                is_ref: true,
            }],
            vec![param_arg],
        );
        let expected = types.intern_func(
            vec![TypeRef {
                id: bit32,
                is_ref: true,
            }],
            vec![bit32_arg],
        );
        assert_eq!(
            types.substitute(
                TypeRef {
                    id: template,
                    is_ref: true
                },
                &[bit32_arg]
            ),
            TypeRef {
                id: expected,
                is_ref: true
            }
        );
    }

    /// `P[x, x]` applied `depth` times to `@bit32`: `depth` table entries,
    /// a tree of `2^depth` leaves.
    fn doubled(types: &mut Types, depth: usize, leaf: TypeRef) -> TypeRef {
        let pair = DefId(7);
        types.set_object_name(pair, "P");
        (0..depth).fold(leaf, |inner, _| {
            TypeRef::value(types.intern_instance(pair, vec![inner, inner]))
        })
    }

    #[test]
    fn huge_types_are_cheap_to_ask_about() {
        let mut types = Types::new();
        let param = TypeRef::value(types.intern_param(0, "T"));
        let concrete = doubled(&mut types, 200, Builtin::Bit32.value());
        let template = doubled(&mut types, 200, param);
        assert!(!types.contains_param(concrete.id));
        assert!(types.contains_param(template.id));
        assert!(!types.contains_param(TypeId::ERROR));
        // Each distinct type is rewritten once: 200 steps, not 2^200.
        assert_eq!(
            types.substitute(template, &[Builtin::Bit32.value()]),
            concrete
        );
        assert_eq!(
            types.substitute(TypeRef::reference(template.id), &[param]),
            TypeRef::reference(template.id)
        );

        let shown = types.display_bounded(concrete, 40);
        assert_eq!(shown, format!("{}...", "P[".repeat(20)));
        let small = doubled(&mut types, 1, Builtin::Bit32.value());
        assert_eq!(types.display_bounded(small, 40), "P[@bit32, @bit32]");
        assert_eq!(
            types.display_bounded(small, 40),
            types.display_ref(small),
            "an uncut display is the full one"
        );
        let reference = TypeRef {
            id: small.id,
            is_ref: true,
        };
        assert_eq!(types.display_bounded(reference, 6), "ref P[...");
    }
}
