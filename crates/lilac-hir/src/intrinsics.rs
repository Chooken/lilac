//! The compiler intrinsic table (SPEC §3): one entry per `@`-name, shared by
//! type checking (signatures) and lowering (what each one becomes).
//!
//! Adding an intrinsic is one variant here plus its `name`/`signature` rows;
//! lowering only needs a new arm when it maps to a new IR operation.

/// Integer container width named by a `_W` suffix.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum IntWidth {
    /// `_8`.
    W8,
    /// `_16`.
    W16,
    /// `_32`.
    W32,
    /// `_64`.
    W64,
    /// `_native`.
    Native,
}

impl IntWidth {
    /// The `@bitW` container.
    #[must_use]
    pub fn builtin(self) -> &'static str {
        match self {
            Self::W8 => "@bit8",
            Self::W16 => "@bit16",
            Self::W32 => "@bit32",
            Self::W64 => "@bit64",
            Self::Native => "@bit_native",
        }
    }

    fn parse(suffix: &str) -> Option<Self> {
        Some(match suffix {
            "8" => Self::W8,
            "16" => Self::W16,
            "32" => Self::W32,
            "64" => Self::W64,
            "native" => Self::Native,
            _ => return None,
        })
    }
}

/// Float container width named by a `_W` suffix.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FloatWidth {
    /// `_16`.
    W16,
    /// `_32`.
    W32,
    /// `_64`.
    W64,
    /// `_128`.
    W128,
}

impl FloatWidth {
    /// The `@fbitW` container.
    #[must_use]
    pub fn builtin(self) -> &'static str {
        match self {
            Self::W16 => "@fbit16",
            Self::W32 => "@fbit32",
            Self::W64 => "@fbit64",
            Self::W128 => "@fbit128",
        }
    }

    /// The same-width `@bitW` (none for 128: there is no `@bit128`).
    #[must_use]
    pub fn bits(self) -> Option<&'static str> {
        match self {
            Self::W16 => Some("@bit16"),
            Self::W32 => Some("@bit32"),
            Self::W64 => Some("@bit64"),
            Self::W128 => None,
        }
    }

    fn parse(suffix: &str) -> Option<Self> {
        Some(match suffix {
            "16" => Self::W16,
            "32" => Self::W32,
            "64" => Self::W64,
            "128" => Self::W128,
            _ => return None,
        })
    }
}

/// A numeric operation shared by the integer and float families.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum NumOp {
    /// `add`.
    Add,
    /// `sub`.
    Sub,
    /// `mul`.
    Mul,
    /// `div` (signed for integers).
    Div,
    /// `div_u` (integers only).
    DivU,
    /// `mod` (signed for integers).
    Mod,
    /// `mod_u` (integers only).
    ModU,
    /// `negate` (unary).
    Negate,
    /// `less_than` (signed for integers).
    Lt,
    /// `greater_than`.
    Gt,
    /// `less_than_or_equal`.
    Le,
    /// `greater_than_or_equal`.
    Ge,
    /// `less_than_u` (integers only).
    LtU,
    /// `greater_than_u`.
    GtU,
    /// `less_than_or_equal_u`.
    LeU,
    /// `greater_than_or_equal_u`.
    GeU,
}

impl NumOp {
    fn parse(name: &str, float: bool) -> Option<Self> {
        let op = match name {
            "add" => Self::Add,
            "sub" => Self::Sub,
            "mul" => Self::Mul,
            "div" => Self::Div,
            "mod" => Self::Mod,
            "negate" => Self::Negate,
            "less_than" => Self::Lt,
            "greater_than" => Self::Gt,
            "less_than_or_equal" => Self::Le,
            "greater_than_or_equal" => Self::Ge,
            "div_u" if !float => Self::DivU,
            "mod_u" if !float => Self::ModU,
            "less_than_u" if !float => Self::LtU,
            "greater_than_u" if !float => Self::GtU,
            "less_than_or_equal_u" if !float => Self::LeU,
            "greater_than_or_equal_u" if !float => Self::GeU,
            _ => return None,
        };
        Some(op)
    }

    /// Whether the result is a `@bit8` truth value.
    #[must_use]
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Lt
                | Self::Gt
                | Self::Le
                | Self::Ge
                | Self::LtU
                | Self::GtU
                | Self::LeU
                | Self::GeU
        )
    }
}

/// A memory ordering named by an atomic intrinsic's `_<order>` part (none
/// means sequentially consistent).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Ordering {
    /// `relaxed`.
    Relaxed,
    /// `acquire`.
    Acquire,
    /// `release`.
    Release,
    /// `acq_rel`.
    AcqRel,
    /// No ordering in the name.
    SeqCst,
}

impl Ordering {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "relaxed" => Self::Relaxed,
            "acquire" => Self::Acquire,
            "release" => Self::Release,
            "acq_rel" => Self::AcqRel,
            _ => return None,
        })
    }
}

/// An atomic memory operation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum AtomicOp {
    /// `load`.
    Load,
    /// `store`.
    Store,
    /// `swap`.
    Swap,
    /// `compare_swap`.
    CompareSwap,
    /// `add`.
    Add,
    /// `sub`.
    Sub,
    /// `and`.
    And,
    /// `or`.
    Or,
    /// `xor`.
    Xor,
    /// `max` (signed).
    Max,
    /// `min` (signed).
    Min,
    /// `max_u`.
    MaxU,
    /// `min_u`.
    MinU,
}

impl AtomicOp {
    /// Operation names, `max_u`/`min_u` before `max`/`min` so a prefix match
    /// takes the longer name.
    const NAMES: [(&'static str, Self); 13] = [
        ("compare_swap", Self::CompareSwap),
        ("max_u", Self::MaxU),
        ("min_u", Self::MinU),
        ("load", Self::Load),
        ("store", Self::Store),
        ("swap", Self::Swap),
        ("add", Self::Add),
        ("sub", Self::Sub),
        ("and", Self::And),
        ("xor", Self::Xor),
        ("max", Self::Max),
        ("min", Self::Min),
        ("or", Self::Or),
    ];

    /// Whether `ordering` is valid for this operation: a load cannot
    /// release, and a store cannot acquire.
    #[must_use]
    pub fn allows(self, ordering: Ordering) -> bool {
        match self {
            Self::Load => !matches!(ordering, Ordering::Release | Ordering::AcqRel),
            Self::Store => !matches!(ordering, Ordering::Acquire | Ordering::AcqRel),
            _ => true,
        }
    }

    /// Parses `<op>[_<order>]_W` (the part after `@atomic_`).
    fn parse(rest: &str) -> Option<(Self, Ordering, IntWidth)> {
        let (head, width) = rest.rsplit_once('_')?;
        let width = IntWidth::parse(width)?;
        Self::NAMES.iter().find_map(|(name, op)| {
            let after = head.strip_prefix(name)?;
            let ordering = match after {
                "" => Ordering::SeqCst,
                _ => Ordering::parse(after.strip_prefix('_')?)?,
            };
            op.allows(ordering).then_some((*op, ordering, width))
        })
    }
}

/// One compiler intrinsic.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Intrinsic {
    /// `@i_<op>_W`.
    Int(NumOp, IntWidth),
    /// `@f_<op>_W`.
    Float(NumOp, FloatWidth),
    /// `@fbit_to_bits_W` (same-width reinterpret).
    FbitToBits(FloatWidth),
    /// `@bits_to_fbit_W` (same-width reinterpret).
    BitsToFbit(FloatWidth),
    /// `@truncate` (width from the expected type).
    Truncate,
    /// `@extend_zero` (width from the expected type).
    ExtendZero,
    /// `@size_of(Type)` / `@size_of(value)`.
    SizeOf,
    /// `@load_u8(ptr, offset)`.
    LoadU8,
    /// `@store_u8(ptr, offset, value)`.
    StoreU8,
    /// `@memcpy(dst, src, len)`.
    Memcpy,
    /// `@memmove(dst, src, len)`: `@memcpy` for overlapping ranges.
    Memmove,
    /// `@memset(dst, byte, len)`.
    Memset,
    /// `@align_of(Type)` / `@align_of(value)`.
    AlignOf,
    /// `@ptr_add(ptr, offset)`: the address `offset` bytes past `ptr`.
    PtrAdd,
    /// `@ptr_to_address(ptr)`: a reference's address as an integer.
    PtrToAddress,
    /// `@address_to_ptr(address)`: an integer address as a reference.
    AddressToPtr,
    /// `@syscall(number, a0, a1, a2, a3, a4, a5)`: the Linux system-call
    /// instruction (`E035` on other targets).
    Syscall,
    /// `@trap()`: stops the program at once with a trap instruction.
    Trap,
    /// `@byte_buffer_ptr(buffer)`.
    ByteBufferPtr,
    /// `@byte_buffer_len(buffer)`.
    ByteBufferLen,
    /// `@byte_buffer_from(ptr, len)`.
    ByteBufferFrom,
    /// `@format_args_new()`.
    FormatArgsNew,
    /// `@format_args_push(ref args, piece)`.
    FormatArgsPush,
    /// `@format_args_count(args)`.
    FormatArgsCount,
    /// `@format_args_piece(args, index)`.
    FormatArgsPiece,
    /// `@compile_error("message")`.
    CompileError,
    /// `@panic(message)`: ends the program with a message and a stack trace
    /// (see `.lilac.panic`).
    Panic,
    /// `@atomic_<op>[_<order>]_W`: an atomic memory operation, naturally
    /// aligned.
    Atomic(AtomicOp, Ordering, IntWidth),
    /// `@atomic_fence[_<order>]` (any ordering but `relaxed`).
    Fence(Ordering),
    /// `@array_at(ref array, index)`: a `ref` to one element, bounds
    /// checked.
    ArrayAt,
    /// `@array_len(Type)` / `@array_len(value)`: an array's length.
    ArrayLen,
}

/// A parameter or result type in an intrinsic signature.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    /// A builtin container by spelling (`@bit8`, `@byte_buffer`, ...).
    Builtin(&'static str),
    /// `ref` of a builtin container.
    Ref(&'static str),
    /// Any bit container (`@truncate`/`@extend_zero` operand).
    AnyBit,
    /// The expected bit type (`@truncate`/`@extend_zero` result).
    ExpectedBit,
}

/// A checked intrinsic signature.
#[derive(Clone, Debug)]
pub struct Signature {
    /// Parameter slots.
    pub params: Vec<Slot>,
    /// Result slot (`None` for statements).
    pub result: Option<Slot>,
}

impl Intrinsic {
    /// Looks an intrinsic up by `@`-name.
    #[must_use]
    pub fn lookup(name: &str) -> Option<Self> {
        if let Some(rest) = name.strip_prefix("@i_") {
            let (op, width) = rest.rsplit_once('_')?;
            return Some(Self::Int(NumOp::parse(op, false)?, IntWidth::parse(width)?));
        }
        if let Some(rest) = name.strip_prefix("@f_") {
            let (op, width) = rest.rsplit_once('_')?;
            return Some(Self::Float(
                NumOp::parse(op, true)?,
                FloatWidth::parse(width)?,
            ));
        }
        if name == "@atomic_fence" {
            return Some(Self::Fence(Ordering::SeqCst));
        }
        if let Some(order) = name.strip_prefix("@atomic_fence_") {
            return Ordering::parse(order)
                .filter(|order| *order != Ordering::Relaxed)
                .map(Self::Fence);
        }
        if let Some(rest) = name.strip_prefix("@atomic_") {
            let (op, ordering, width) = AtomicOp::parse(rest)?;
            return Some(Self::Atomic(op, ordering, width));
        }
        if let Some(width) = name.strip_prefix("@fbit_to_bits_") {
            let width = FloatWidth::parse(width).filter(|w| w.bits().is_some())?;
            return Some(Self::FbitToBits(width));
        }
        if let Some(width) = name.strip_prefix("@bits_to_fbit_") {
            let width = FloatWidth::parse(width).filter(|w| w.bits().is_some())?;
            return Some(Self::BitsToFbit(width));
        }
        Some(match name {
            "@truncate" => Self::Truncate,
            "@extend_zero" => Self::ExtendZero,
            "@size_of" => Self::SizeOf,
            "@load_u8" => Self::LoadU8,
            "@store_u8" => Self::StoreU8,
            "@memcpy" => Self::Memcpy,
            "@memmove" => Self::Memmove,
            "@memset" => Self::Memset,
            "@align_of" => Self::AlignOf,
            "@ptr_add" => Self::PtrAdd,
            "@ptr_to_address" => Self::PtrToAddress,
            "@address_to_ptr" => Self::AddressToPtr,
            "@syscall" => Self::Syscall,
            "@trap" => Self::Trap,
            "@byte_buffer_ptr" => Self::ByteBufferPtr,
            "@byte_buffer_len" => Self::ByteBufferLen,
            "@byte_buffer_from" => Self::ByteBufferFrom,
            "@format_args_new" => Self::FormatArgsNew,
            "@format_args_push" => Self::FormatArgsPush,
            "@format_args_count" => Self::FormatArgsCount,
            "@format_args_piece" => Self::FormatArgsPiece,
            "@compile_error" => Self::CompileError,
            "@panic" => Self::Panic,
            "@array_at" => Self::ArrayAt,
            "@array_len" => Self::ArrayLen,
            _ => return None,
        })
    }

    /// The signature every call is checked against. `@size_of`, `@align_of`,
    /// `@array_len` and `@compile_error` take a type/literal operand and
    /// `@array_at` any array, so they are checked specially and their
    /// parameter list is empty here.
    #[must_use]
    pub fn signature(self) -> Signature {
        use Slot::{AnyBit, Builtin, ExpectedBit, Ref};
        let sig = |params: Vec<Slot>, result: Option<Slot>| Signature { params, result };
        match self {
            Self::Int(op, width) => {
                let ty = Builtin(width.builtin());
                let result = if op.is_comparison() {
                    Builtin("@bit8")
                } else {
                    ty
                };
                let params = if op == NumOp::Negate {
                    vec![ty]
                } else {
                    vec![ty, ty]
                };
                sig(params, Some(result))
            }
            Self::Float(op, width) => {
                let ty = Builtin(width.builtin());
                let result = if op.is_comparison() {
                    Builtin("@bit8")
                } else {
                    ty
                };
                let params = if op == NumOp::Negate {
                    vec![ty]
                } else {
                    vec![ty, ty]
                };
                sig(params, Some(result))
            }
            Self::FbitToBits(width) => sig(
                vec![Builtin(width.builtin())],
                Some(Builtin(width.bits().unwrap_or("@bit64"))),
            ),
            Self::BitsToFbit(width) => sig(
                vec![Builtin(width.bits().unwrap_or("@bit64"))],
                Some(Builtin(width.builtin())),
            ),
            Self::Truncate | Self::ExtendZero => sig(vec![AnyBit], Some(ExpectedBit)),
            Self::SizeOf | Self::AlignOf | Self::ArrayLen => {
                sig(Vec::new(), Some(Builtin("@bit_native")))
            }
            Self::CompileError | Self::Trap | Self::Fence(_) | Self::ArrayAt => {
                sig(Vec::new(), None)
            }
            Self::Atomic(op, _, width) => {
                let ty = width.builtin();
                let params = match op {
                    AtomicOp::Load => vec![Ref(ty)],
                    AtomicOp::CompareSwap => vec![Ref(ty), Builtin(ty), Builtin(ty)],
                    _ => vec![Ref(ty), Builtin(ty)],
                };
                sig(params, (op != AtomicOp::Store).then_some(Builtin(ty)))
            }
            Self::LoadU8 => sig(
                vec![Ref("@bit8"), Builtin("@bit_native")],
                Some(Builtin("@bit8")),
            ),
            Self::StoreU8 => sig(
                vec![Ref("@bit8"), Builtin("@bit_native"), Builtin("@bit8")],
                None,
            ),
            Self::Memcpy | Self::Memmove => sig(
                vec![Ref("@bit8"), Ref("@bit8"), Builtin("@bit_native")],
                None,
            ),
            Self::Memset => sig(
                vec![Ref("@bit8"), Builtin("@bit8"), Builtin("@bit_native")],
                None,
            ),
            Self::PtrAdd => sig(
                vec![Ref("unknown"), Builtin("@bit_native")],
                Some(Ref("unknown")),
            ),
            Self::PtrToAddress => sig(vec![Ref("unknown")], Some(Builtin("@bit_native"))),
            Self::AddressToPtr => sig(vec![Builtin("@bit_native")], Some(Ref("unknown"))),
            Self::Syscall => sig(
                vec![Builtin("@bit_native"); 7],
                Some(Builtin("@bit_native")),
            ),
            Self::ByteBufferPtr => sig(vec![Builtin("@byte_buffer")], Some(Ref("@bit8"))),
            Self::ByteBufferLen => sig(vec![Builtin("@byte_buffer")], Some(Builtin("@bit_native"))),
            Self::ByteBufferFrom => sig(
                vec![Ref("@bit8"), Builtin("@bit_native")],
                Some(Builtin("@byte_buffer")),
            ),
            Self::FormatArgsNew => sig(Vec::new(), Some(Builtin("@format_args"))),
            Self::FormatArgsPush => sig(vec![Ref("@format_args"), Builtin("@byte_buffer")], None),
            Self::FormatArgsCount => {
                sig(vec![Builtin("@format_args")], Some(Builtin("@bit_native")))
            }
            Self::FormatArgsPiece => sig(
                vec![Builtin("@format_args"), Builtin("@bit_native")],
                Some(Builtin("@byte_buffer")),
            ),
            Self::Panic => sig(vec![Builtin("@byte_buffer")], None),
        }
    }

    /// The backend leaf builtin (`lilac_ir::builtin_sig`) this intrinsic
    /// calls, if it is one.
    #[must_use]
    pub fn leaf_builtin(self) -> Option<&'static str> {
        Some(match self {
            Self::Memcpy => "@memcpy",
            Self::Memmove => "@memmove",
            Self::Memset => "@memset",
            Self::Syscall => "@syscall",
            Self::Trap => "@lilac_abort",
            _ => return None,
        })
    }

    /// The LIR runtime function implementing this intrinsic, if it is one.
    #[must_use]
    pub fn runtime_symbol(self) -> Option<&'static str> {
        Some(match self {
            Self::LoadU8 => ".lilac.load_u8",
            Self::StoreU8 => ".lilac.store_u8",
            Self::ByteBufferPtr => ".lilac.byte_buffer_ptr",
            Self::ByteBufferLen => ".lilac.byte_buffer_len",
            Self::ByteBufferFrom => ".lilac.byte_buffer_from",
            Self::FormatArgsNew => ".lilac.format_args_new",
            Self::FormatArgsPush => ".lilac.format_args_push",
            Self::FormatArgsCount => ".lilac.format_args_count",
            Self::FormatArgsPiece => ".lilac.format_args_piece",
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_parses_families_and_widths() {
        assert_eq!(
            Intrinsic::lookup("@i_add_8"),
            Some(Intrinsic::Int(NumOp::Add, IntWidth::W8))
        );
        assert_eq!(
            Intrinsic::lookup("@i_less_than_u_native"),
            Some(Intrinsic::Int(NumOp::LtU, IntWidth::Native))
        );
        assert_eq!(
            Intrinsic::lookup("@f_greater_than_or_equal_32"),
            Some(Intrinsic::Float(NumOp::Ge, FloatWidth::W32))
        );
        assert_eq!(Intrinsic::lookup("@f_div_u_32"), None);
        assert_eq!(Intrinsic::lookup("@i_add_128"), None);
        assert_eq!(Intrinsic::lookup("@fbit_to_bits_128"), None);
        assert_eq!(Intrinsic::lookup("@i_add"), None);
        assert_eq!(Intrinsic::lookup("@compilerError"), None);
        assert_eq!(Intrinsic::lookup("@size_of"), Some(Intrinsic::SizeOf));
        assert_eq!(Intrinsic::lookup("@panic"), Some(Intrinsic::Panic));
        assert_eq!(Intrinsic::lookup("@align_of"), Some(Intrinsic::AlignOf));
        assert_eq!(Intrinsic::lookup("@ptr_add"), Some(Intrinsic::PtrAdd));
        assert_eq!(Intrinsic::lookup("@trap"), Some(Intrinsic::Trap));
    }

    #[test]
    fn lookup_parses_atomics() {
        use AtomicOp::{CompareSwap, Load, MaxU, Or, Store, Swap};
        let atomic = Intrinsic::lookup;
        assert_eq!(
            atomic("@atomic_load_32"),
            Some(Intrinsic::Atomic(Load, Ordering::SeqCst, IntWidth::W32))
        );
        assert_eq!(
            atomic("@atomic_load_acquire_native"),
            Some(Intrinsic::Atomic(Load, Ordering::Acquire, IntWidth::Native))
        );
        assert_eq!(
            atomic("@atomic_store_release_8"),
            Some(Intrinsic::Atomic(Store, Ordering::Release, IntWidth::W8))
        );
        assert_eq!(
            atomic("@atomic_compare_swap_acq_rel_64"),
            Some(Intrinsic::Atomic(
                CompareSwap,
                Ordering::AcqRel,
                IntWidth::W64
            ))
        );
        assert_eq!(
            atomic("@atomic_max_u_relaxed_16"),
            Some(Intrinsic::Atomic(MaxU, Ordering::Relaxed, IntWidth::W16))
        );
        assert_eq!(
            atomic("@atomic_or_32"),
            Some(Intrinsic::Atomic(Or, Ordering::SeqCst, IntWidth::W32))
        );
        assert_eq!(
            atomic("@atomic_swap_relaxed_32"),
            Some(Intrinsic::Atomic(Swap, Ordering::Relaxed, IntWidth::W32))
        );
        assert_eq!(
            atomic("@atomic_fence"),
            Some(Intrinsic::Fence(Ordering::SeqCst))
        );
        assert_eq!(
            atomic("@atomic_fence_acquire"),
            Some(Intrinsic::Fence(Ordering::Acquire))
        );
        for rejected in [
            "@atomic_load_release_32",
            "@atomic_load_acq_rel_32",
            "@atomic_store_acquire_32",
            "@atomic_store_acq_rel_32",
            "@atomic_fence_relaxed",
            "@atomic_add_seq_cst_32",
            "@atomic_add_128",
            "@atomic_add",
            "@atomic_nand_32",
            "@atomic_maxu_32",
            "@atomic_fence_32",
        ] {
            assert_eq!(atomic(rejected), None, "{rejected}");
        }
    }

    #[test]
    fn atomic_signatures() {
        let sig = Intrinsic::lookup("@atomic_compare_swap_32")
            .expect("known")
            .signature();
        assert_eq!(
            sig.params,
            vec![
                Slot::Ref("@bit32"),
                Slot::Builtin("@bit32"),
                Slot::Builtin("@bit32")
            ]
        );
        assert_eq!(sig.result, Some(Slot::Builtin("@bit32")));
        let sig = Intrinsic::lookup("@atomic_store_8")
            .expect("known")
            .signature();
        assert_eq!(sig.params, vec![Slot::Ref("@bit8"), Slot::Builtin("@bit8")]);
        assert_eq!(sig.result, None);
        let sig = Intrinsic::lookup("@atomic_load_native")
            .expect("known")
            .signature();
        assert_eq!(sig.params, vec![Slot::Ref("@bit_native")]);
        assert_eq!(sig.result, Some(Slot::Builtin("@bit_native")));
    }

    #[test]
    fn instruction_builtin_signatures() {
        let sig = Intrinsic::PtrAdd.signature();
        assert_eq!(
            sig.params,
            vec![Slot::Ref("unknown"), Slot::Builtin("@bit_native")]
        );
        assert_eq!(sig.result, Some(Slot::Ref("unknown")));
        assert_eq!(
            Intrinsic::AddressToPtr.signature().result,
            Some(Slot::Ref("unknown"))
        );
        let sig = Intrinsic::Syscall.signature();
        assert_eq!(sig.params, vec![Slot::Builtin("@bit_native"); 7]);
        assert_eq!(sig.result, Some(Slot::Builtin("@bit_native")));
        assert_eq!(
            Intrinsic::Memset.signature().params[1],
            Slot::Builtin("@bit8")
        );
        assert_eq!(Intrinsic::Trap.leaf_builtin(), Some("@lilac_abort"));
        assert_eq!(Intrinsic::Memmove.leaf_builtin(), Some("@memmove"));
    }

    #[test]
    fn comparisons_return_bit8() {
        let sig = Intrinsic::lookup("@i_less_than_32")
            .expect("known")
            .signature();
        assert_eq!(sig.result, Some(Slot::Builtin("@bit8")));
        assert_eq!(sig.params.len(), 2);
        let sig = Intrinsic::lookup("@f_negate_64")
            .expect("known")
            .signature();
        assert_eq!(sig.params, vec![Slot::Builtin("@fbit64")]);
    }
}
