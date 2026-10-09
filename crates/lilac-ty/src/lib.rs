//! Types: the interner, `TypeRef`, and function signatures.

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
    /// `@format_args` fixed-capacity piece table.
    FormatArgs,
    /// `unknown` opaque type (only behind `ref`).
    Unknown,
    /// User-declared `object`.
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
    },
    /// Poison for error recovery; never a real type.
    Error,
}

/// Every builtin type's spelling, in `TypeId` order (the first entries of
/// every type table).
pub const BUILTIN_TYPES: [&str; 12] = [
    "@bit8",
    "@bit16",
    "@bit32",
    "@bit64",
    "@bit_native",
    "@fbit16",
    "@fbit32",
    "@fbit64",
    "@fbit128",
    "@byte_buffer",
    "@format_args",
    "unknown",
];

/// Function signature.
#[derive(Clone, Debug, Default)]
pub struct FuncSig {
    /// Parameter types in order.
    pub params: Vec<TypeRef>,
    /// Named return-slot types in order.
    pub returns: Vec<TypeRef>,
}

/// The type interner.
#[derive(Clone, Debug, Default)]
pub struct Types {
    kinds: Vec<TypeKind>,
    objects: HashMap<DefId, TypeId>,
    object_names: HashMap<DefId, String>,
    funcs: HashMap<(Vec<TypeRef>, Vec<TypeRef>), TypeId>,
    params: HashMap<(u32, String), TypeId>,
    instances: HashMap<(DefId, Vec<TypeRef>), TypeId>,
    arrays: HashMap<(TypeId, u64), TypeId>,
}

impl Types {
    /// Table with the 12 builtin types pre-interned.
    pub fn new() -> Self {
        let mut table = Self::default();
        table.kinds.extend([
            TypeKind::Bit8,
            TypeKind::Bit16,
            TypeKind::Bit32,
            TypeKind::Bit64,
            TypeKind::BitNative,
            TypeKind::FBit16,
            TypeKind::FBit32,
            TypeKind::FBit64,
            TypeKind::FBit128,
            TypeKind::ByteBuffer,
            TypeKind::FormatArgs,
            TypeKind::Unknown,
        ]);
        table
    }

    /// Looks up a builtin by Lilac spelling (`@bit32`, `@byte_buffer`, `unknown`).
    pub fn builtin(&self, name: &str) -> Option<TypeId> {
        let id = BUILTIN_TYPES.iter().position(|builtin| *builtin == name)?;
        Some(TypeId(u32::try_from(id).ok()?))
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

    /// Interns (or finds) a generic parameter placeholder (`T` -> index 0).
    pub fn intern_param(&mut self, index: u32, name: &str) -> TypeId {
        if let Some(&id) = self.params.get(&(index, name.to_owned())) {
            return id;
        }
        let id = TypeId(next_id(self.kinds.len()));
        self.kinds.push(TypeKind::Param {
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
        let id = TypeId(next_id(self.kinds.len()));
        self.kinds.push(TypeKind::Instance {
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
        let id = TypeId(next_id(self.kinds.len()));
        self.kinds.push(TypeKind::Array { elem, len });
        self.arrays.insert((elem, len), id);
        id
    }

    /// Rewrites `Param(i)` to `args[i]` throughout `ty`, recursively
    /// re-interning nested `Instance`/`Func` types. The outer `ref` flag is
    /// preserved (and never doubled).
    pub fn substitute(&mut self, ty: TypeRef, args: &[TypeRef]) -> TypeRef {
        match self.kind(ty.id).clone() {
            TypeKind::Param { index, .. } => match args.get(index as usize) {
                Some(&arg) => TypeRef {
                    id: arg.id,
                    is_ref: ty.is_ref || arg.is_ref,
                },
                None => ty,
            },
            TypeKind::Instance {
                base,
                args: inst_args,
            } => {
                let args = inst_args
                    .iter()
                    .map(|arg| self.substitute(*arg, args))
                    .collect();
                let id = self.intern_instance(base, args);
                TypeRef {
                    id,
                    is_ref: ty.is_ref,
                }
            }
            TypeKind::Array { elem, len } => {
                let elem = self.substitute(TypeRef::value(elem), args);
                let id = self.intern_array(elem.id, len);
                TypeRef {
                    id,
                    is_ref: ty.is_ref,
                }
            }
            TypeKind::Func { params, returns } => {
                let params = params
                    .iter()
                    .map(|param| self.substitute(*param, args))
                    .collect();
                let returns = returns
                    .iter()
                    .map(|ret| self.substitute(*ret, args))
                    .collect();
                let id = self.intern_func(params, returns);
                TypeRef {
                    id,
                    is_ref: ty.is_ref,
                }
            }
            _ => ty,
        }
    }

    /// Whether `id` mentions an unsubstituted generic parameter anywhere.
    pub fn contains_param(&self, id: TypeId) -> bool {
        match self.kind(id) {
            TypeKind::Param { .. } => true,
            TypeKind::Instance { args, .. } => args.iter().any(|arg| self.contains_param(arg.id)),
            TypeKind::Array { elem, .. } => self.contains_param(*elem),
            TypeKind::Func { params, returns } => params
                .iter()
                .chain(returns.iter())
                .any(|slot| self.contains_param(slot.id)),
            _ => false,
        }
    }

    /// Deterministic, collision-free, arity-tagged symbol name for IR.
    /// User identifiers cannot contain `.`, so it is the reserved separator:
    /// builtins gain a leading `.`; `ref X` becomes `ref..X`; an `Instance` is
    /// `<Name>.<arity>.<arg>…`. A builtin argument already starts with `.`, so
    /// a builtin argument such as `@bit32` reads as `..bit32`.
    pub fn mangle(&self, id: TypeId) -> String {
        match self.kind(id) {
            TypeKind::Bit8 => ".bit8".to_owned(),
            TypeKind::Bit16 => ".bit16".to_owned(),
            TypeKind::Bit32 => ".bit32".to_owned(),
            TypeKind::Bit64 => ".bit64".to_owned(),
            TypeKind::BitNative => ".bit_native".to_owned(),
            TypeKind::FBit16 => ".fbit16".to_owned(),
            TypeKind::FBit32 => ".fbit32".to_owned(),
            TypeKind::FBit64 => ".fbit64".to_owned(),
            TypeKind::FBit128 => ".fbit128".to_owned(),
            TypeKind::ByteBuffer => ".byte_buffer".to_owned(),
            TypeKind::FormatArgs => ".format_args".to_owned(),
            TypeKind::Unknown => ".unknown".to_owned(),
            TypeKind::Object(def) => self.object_name(*def),
            TypeKind::Param { index, .. } => format!("param.{index}"),
            TypeKind::Instance { base, args } => {
                let mut out = format!("{}.{}", self.object_name(*base), args.len());
                for arg in args {
                    out.push('.');
                    out.push_str(&self.mangle_ref(*arg));
                }
                out
            }
            TypeKind::Array { elem, len } => format!("array.{len}.{}", self.mangle(*elem)),
            TypeKind::Func { params, returns } => {
                let mut out = format!("function.{}", params.len());
                for param in params {
                    out.push('.');
                    out.push_str(&self.mangle_ref(*param));
                }
                out.push_str(".returns.");
                out.push_str(&returns.len().to_string());
                for ret in returns {
                    out.push('.');
                    out.push_str(&self.mangle_ref(*ret));
                }
                out
            }
            TypeKind::Error => ".error".to_owned(),
        }
    }

    fn mangle_ref(&self, ty: TypeRef) -> String {
        if ty.is_ref {
            format!("ref.{}", self.mangle(ty.id))
        } else {
            self.mangle(ty.id)
        }
    }

    fn object_name(&self, def: DefId) -> String {
        self.object_names
            .get(&def)
            .cloned()
            .unwrap_or_else(|| format!("object#{}", def.0))
    }

    /// Interns (or finds) a `function (params) returns` type.
    pub fn intern_func(&mut self, params: Vec<TypeRef>, returns: Vec<TypeRef>) -> TypeId {
        let key = (params, returns);
        if let Some(&id) = self.funcs.get(&key) {
            return id;
        }
        let id = TypeId(next_id(self.kinds.len()));
        self.kinds.push(TypeKind::Func {
            params: key.0.clone(),
            returns: key.1.clone(),
        });
        self.funcs.insert(key, id);
        id
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

    /// Pretty-prints a type in Lilac syntax (`@bit32`, `Number`).
    pub fn display(&self, id: TypeId) -> String {
        match self.kind(id) {
            TypeKind::Bit8 => "@bit8".to_owned(),
            TypeKind::Bit16 => "@bit16".to_owned(),
            TypeKind::Bit32 => "@bit32".to_owned(),
            TypeKind::Bit64 => "@bit64".to_owned(),
            TypeKind::BitNative => "@bit_native".to_owned(),
            TypeKind::FBit16 => "@fbit16".to_owned(),
            TypeKind::FBit32 => "@fbit32".to_owned(),
            TypeKind::FBit64 => "@fbit64".to_owned(),
            TypeKind::FBit128 => "@fbit128".to_owned(),
            TypeKind::ByteBuffer => "@byte_buffer".to_owned(),
            TypeKind::FormatArgs => "@format_args".to_owned(),
            TypeKind::Unknown => "unknown".to_owned(),
            TypeKind::Object(def) => self.object_name(*def),
            TypeKind::Param { name, .. } => name.clone(),
            TypeKind::Instance { base, args } => {
                let args: Vec<String> = args.iter().map(|a| self.display_ref(*a)).collect();
                format!("{}[{}]", self.object_name(*base), args.join(", "))
            }
            TypeKind::Array { elem, len } => format!("@array({}, {len})", self.display(*elem)),
            TypeKind::Func { params, returns } => {
                let ps: Vec<String> = params.iter().map(|p| self.display_ref(*p)).collect();
                let rs: Vec<String> = returns.iter().map(|r| self.display_ref(*r)).collect();
                let rs = if rs.is_empty() {
                    "nothing".to_owned()
                } else {
                    rs.join(", ")
                };
                format!("function ({}) -> {rs}", ps.join(", "))
            }
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
            ("@fbit16", ".fbit16", "@fbit16"),
            ("@fbit32", ".fbit32", "@fbit32"),
            ("@fbit64", ".fbit64", "@fbit64"),
            ("@fbit128", ".fbit128", "@fbit128"),
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
        assert_eq!(types.display(one), "function (@bit8) -> @bit32");
        let nullary = types.intern_func(Vec::new(), Vec::new());
        assert_ne!(one, nullary);
        assert_eq!(types.display(nullary), "function () -> nothing");
    }

    #[test]
    fn arrays_intern_display_mangle_and_substitute() {
        let mut types = Types::new();
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        let array = types.intern_array(bit32, 4);
        assert_eq!(types.intern_array(bit32, 4), array);
        assert_ne!(types.intern_array(bit32, 5), array);
        assert_eq!(types.display(array), "@array(@bit32, 4)");
        assert_eq!(types.mangle(array), "array.4..bit32");
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
    fn instance_mangle_is_arity_tagged() {
        let mut types = Types::new();
        types.set_object_name(DefId(1), "GenObj");
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        let arg = TypeRef {
            id: bit32,
            is_ref: false,
        };
        let outer = types.intern_instance(DefId(1), vec![arg]);
        assert_eq!(types.mangle(outer), "GenObj.1..bit32");
        let nested = types.intern_instance(
            DefId(1),
            vec![TypeRef {
                id: outer,
                is_ref: false,
            }],
        );
        // `{base}.{argc}` then `.` + `mangle_ref(arg)`; a non-ref instance arg
        // carries no leading dot of its own, so the separator is a single dot.
        assert_eq!(types.mangle(nested), "GenObj.1.GenObj.1..bit32");
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
        assert_eq!(types.mangle(outer), "GenObj.1..bit_native");
    }

    #[test]
    fn underscore_in_base_name_is_not_a_separator() {
        // A user object may contain `_`; only the reserved `.` separates the
        // base name from the arity tag and arguments.
        let mut types = Types::new();
        types.set_object_name(DefId(1), "Gen_Obj");
        let bit32 = types.builtin("@bit32").expect("has @bit32");
        let arg = TypeRef {
            id: bit32,
            is_ref: false,
        };
        let instance = types.intern_instance(DefId(1), vec![arg]);
        assert_eq!(types.mangle(instance), "Gen_Obj.1..bit32");
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
}
