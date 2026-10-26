//! The compiler intrinsic table (SPEC §3): one entry per `@`-name, shared by
//! type checking (signatures) and lowering (what each one becomes).
//!
//! Adding an intrinsic is one variant here plus its `name`/`signature` rows;
//! lowering only needs a new arm when it maps to a new IR operation.

use lilac_ty::Builtin as Ty;

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
    pub fn builtin(self) -> Ty {
        match self {
            Self::W8 => Ty::Bit8,
            Self::W16 => Ty::Bit16,
            Self::W32 => Ty::Bit32,
            Self::W64 => Ty::Bit64,
            Self::Native => Ty::BitNative,
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
    pub fn builtin(self) -> Ty {
        match self {
            Self::W16 => Ty::FBit16,
            Self::W32 => Ty::FBit32,
            Self::W64 => Ty::FBit64,
            Self::W128 => Ty::FBit128,
        }
    }

    /// The same-width `@bitW` (none for 128: there is no `@bit128`).
    #[must_use]
    pub fn bits(self) -> Option<Ty> {
        match self {
            Self::W16 => Some(Ty::Bit16),
            Self::W32 => Some(Ty::Bit32),
            Self::W64 => Some(Ty::Bit64),
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
    /// `and` (integers only).
    And,
    /// `or` (integers only).
    Or,
    /// `xor` (integers only).
    Xor,
    /// `not` (unary, integers only).
    Not,
    /// `shift_left` (integers only; the amount is taken modulo the width).
    Shl,
    /// `shift_right` (arithmetic: copies the sign bit).
    Shr,
    /// `shift_right_u` (logical: shifts in zeros).
    ShrU,
    /// `rotate_left` (integers only; the amount is taken modulo the width).
    RotateLeft,
    /// `rotate_right`.
    RotateRight,
    /// `count_ones` (unary, integers only).
    CountOnes,
    /// `leading_zeros` (unary; the width for zero).
    LeadingZeros,
    /// `trailing_zeros` (unary; the width for zero).
    TrailingZeros,
    /// `swap_bytes` (unary, 16 bits and wider).
    SwapBytes,
    /// `reverse` (unary: the bits in reverse order).
    Reverse,
    /// `add_overflows` (signed): whether the wrapping sum overflowed.
    AddOverflows,
    /// `add_overflows_u`.
    AddOverflowsU,
    /// `sub_overflows`.
    SubOverflows,
    /// `sub_overflows_u`.
    SubOverflowsU,
    /// `mul_overflows`.
    MulOverflows,
    /// `mul_overflows_u`.
    MulOverflowsU,
    /// `add_saturating` (signed): the sum clamped to the range.
    AddSaturating,
    /// `add_saturating_u`.
    AddSaturatingU,
    /// `sub_saturating`.
    SubSaturating,
    /// `sub_saturating_u`.
    SubSaturatingU,
    /// `multiply_high` (signed): the upper half of the double-width product.
    MultiplyHigh,
    /// `multiply_high_u`.
    MultiplyHighU,
    /// `sqrt` (unary, floats only).
    Sqrt,
    /// `multiply_add` (floats only): `a * b + c`, rounded once.
    MultiplyAdd,
    /// `abs` (unary, floats only): clears the sign bit.
    Abs,
    /// `copy_sign` (floats only): the first magnitude with the second sign.
    CopySign,
    /// `floor` (unary, floats only).
    Floor,
    /// `ceil` (unary, floats only).
    Ceil,
    /// `trunc` (unary, floats only).
    Trunc,
    /// `round` (unary, floats only): ties away from zero.
    Round,
    /// `round_even` (unary, floats only): ties to even.
    RoundEven,
    /// `min` (floats only): IEEE 754-2019 `minimum`.
    Min,
    /// `max` (floats only): IEEE 754-2019 `maximum`.
    Max,
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
            _ if float => return Self::parse_float(name),
            _ => return Self::parse_int(name),
        };
        Some(op)
    }

    /// An operation only `@i_` has.
    fn parse_int(name: &str) -> Option<Self> {
        let op = match name {
            "div_u" => Self::DivU,
            "mod_u" => Self::ModU,
            "less_than_u" => Self::LtU,
            "greater_than_u" => Self::GtU,
            "less_than_or_equal_u" => Self::LeU,
            "greater_than_or_equal_u" => Self::GeU,
            "add_overflows" => Self::AddOverflows,
            "add_overflows_u" => Self::AddOverflowsU,
            "sub_overflows" => Self::SubOverflows,
            "sub_overflows_u" => Self::SubOverflowsU,
            "mul_overflows" => Self::MulOverflows,
            "mul_overflows_u" => Self::MulOverflowsU,
            "add_saturating" => Self::AddSaturating,
            "add_saturating_u" => Self::AddSaturatingU,
            "sub_saturating" => Self::SubSaturating,
            "sub_saturating_u" => Self::SubSaturatingU,
            "multiply_high" => Self::MultiplyHigh,
            "multiply_high_u" => Self::MultiplyHighU,
            _ => return None,
        };
        Some(op)
    }

    /// An operation only `@f_` has.
    fn parse_float(name: &str) -> Option<Self> {
        let op = match name {
            "sqrt" => Self::Sqrt,
            "multiply_add" => Self::MultiplyAdd,
            "abs" => Self::Abs,
            "copy_sign" => Self::CopySign,
            "floor" => Self::Floor,
            "ceil" => Self::Ceil,
            "trunc" => Self::Trunc,
            "round" => Self::Round,
            "round_even" => Self::RoundEven,
            "min" => Self::Min,
            "max" => Self::Max,
            _ => return None,
        };
        Some(op)
    }

    /// A bitwise op (`@bit_<op>_W`): integers only, no sign of their own.
    fn parse_bitwise(name: &str) -> Option<Self> {
        let op = match name {
            "and" => Self::And,
            "or" => Self::Or,
            "xor" => Self::Xor,
            "not" => Self::Not,
            "shift_left" => Self::Shl,
            "shift_right" => Self::Shr,
            "shift_right_u" => Self::ShrU,
            "rotate_left" => Self::RotateLeft,
            "rotate_right" => Self::RotateRight,
            "count_ones" => Self::CountOnes,
            "leading_zeros" => Self::LeadingZeros,
            "trailing_zeros" => Self::TrailingZeros,
            "swap_bytes" => Self::SwapBytes,
            "reverse" => Self::Reverse,
            _ => return None,
        };
        Some(op)
    }

    /// Whether it takes one operand.
    #[must_use]
    pub fn is_unary(self) -> bool {
        matches!(
            self,
            Self::Negate
                | Self::Not
                | Self::CountOnes
                | Self::LeadingZeros
                | Self::TrailingZeros
                | Self::SwapBytes
                | Self::Reverse
                | Self::Sqrt
                | Self::Abs
                | Self::Floor
                | Self::Ceil
                | Self::Trunc
                | Self::Round
                | Self::RoundEven
        )
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
                | Self::AddOverflows
                | Self::AddOverflowsU
                | Self::SubOverflows
                | Self::SubOverflowsU
                | Self::MulOverflows
                | Self::MulOverflowsU
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
    /// `@extend_sign` (width from the expected type).
    ExtendSign,
    /// `@int_to_float` (signed; width from the expected type).
    IntToFloat,
    /// `@uint_to_float` (unsigned; width from the expected type).
    UintToFloat,
    /// `@float_to_int` (signed, saturating; width from the expected type).
    FloatToInt,
    /// `@float_to_uint` (unsigned, saturating; width from the expected
    /// type).
    FloatToUint,
    /// `@float_convert` (width from the expected type).
    FloatConvert,
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
    /// `@linux_syscall(number, a0, a1, a2, a3, a4, a5)`: the Linux system-call
    /// instruction (`E035` on other targets).
    LinuxSyscall,
    /// `@linux_thread_clone(flags, stack, parent_tid, child_tid, tls_area,
    /// tls_size, entry, argument)`: Linux `clone` of a thread that calls the
    /// C function `entry(argument)` on `stack`, then exits. With
    /// `CLONE_SETTLS`, its thread-local storage is built in the zeroed
    /// `tls_area` (`E035` off Linux x86-64/aarch64).
    LinuxThreadClone,
    /// `@trap()`: stops the program at once with a trap instruction.
    Trap,
    /// `@spin_hint()`: tells the processor the thread is spinning on a
    /// value (x86 `pause`, Arm `yield`); nothing elsewhere.
    SpinHint,
    /// `@byte_buffer_ptr(buffer)`.
    ByteBufferPtr,
    /// `@byte_buffer_len(buffer)`.
    ByteBufferLen,
    /// `@byte_buffer_from(ptr, len)`.
    ByteBufferFrom,
    /// `@format_args_holes(args)`: how many holes a format has.
    FormatArgsHoles,
    /// `@format_args_piece(args, index)`: literal piece `index` (from 0
    /// to the hole count), bounds checked.
    FormatArgsPiece,
    /// `@format_args_hole(args, index, out)`: formats hole `index` into
    /// `out`, a `ref W` for the format's own `W`, bounds checked.
    FormatArgsHole,
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
    /// `@move(place)`: moves the place out, whatever its type (no copy
    /// hook runs; the place is empty after).
    Move,
    /// `@write(ref T, value)`: moves `value` into the empty slot at the
    /// address (nothing there is dropped).
    Write,
    /// `@read(ref T) T`: moves the `T` at the address out, leaving the slot
    /// empty (no copy hook runs).
    Read,
    /// `@drop(ref T)`: drops the `T` at the address, leaving the slot empty.
    Drop,
    /// `@needs_drop(Type)`: whether dropping a `Type` does anything (a
    /// constant `@bit8`).
    NeedsDrop,
    /// `@discard(place)`: inside a function of `T`, ends `T`'s whole life
    /// without its `@on_drop`; its fields become places of their own.
    Discard,
    /// `@has_field(T, name, F)`: whether `value.name` is a field of type
    /// `F` (a constant `@bit8`).
    HasField,
    /// `@has_method(T, name, function (R, P...) O)`: whether `value.name(...)`
    /// is a method of exactly that type, `R` its receiver.
    HasMethod,
    /// `@has_function(T, name, function (P...) O)`: whether `T.name(...)` is
    /// a static function of exactly that type.
    HasFunction,
    /// `@has_constant(T, NAME, C)`: whether `T.NAME` is a member constant
    /// of type `C`.
    HasConstant,
    /// `@has_variant(T, Name, P)`, or `@has_variant(T, Name)` for none:
    /// whether `.Name` is a variant of `T` with exactly that payload.
    HasVariant,
    /// `@has_hook(@name, function (P...) O)`: whether the hook `@name` has
    /// an overload of exactly that type.
    HasHook,
    /// `@is_copyable(T)`: whether a by-value use of a `T` copies.
    IsCopyable,
    /// `@is_same(T, U)`: whether `T` and `U` are one type.
    IsSame,
}

/// A parameter or result type in an intrinsic signature.
///
/// The first four are what typeck's common path checks a call against;
/// the rest only describe the operands of the intrinsics it checks
/// specially ([`Intrinsic::is_special`]), so an editor can show every
/// signature from this one table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slot {
    /// A builtin container (`@bit8`, `@byte_buffer`, ...).
    Builtin(Ty),
    /// `ref` of a builtin container.
    Ref(Ty),
    /// Any bit container (`@truncate`/`@extend_zero` operand).
    AnyBit,
    /// The expected bit type (`@truncate`/`@extend_zero` result).
    ExpectedBit,
    /// Any float container (`@float_convert` operand).
    AnyFloat,
    /// The expected float type (`@float_convert` result).
    ExpectedFloat,
    /// A type, or a value standing for its type (`@size_of`).
    Type,
    /// A value, or a place, of any type `name` (`@move`'s `T`).
    Param(&'static str),
    /// `ref` of any type `name` (`@read`'s `ref T`).
    RefParam(&'static str),
    /// `ref` of an array (`@array_at`).
    RefArray,
    /// A format of any writer `W` (`@format_args(W)`).
    FormatArgs,
    /// A string literal (`@compile_error`).
    StringLiteral,
    /// What the `@compiler_print` hook takes (`@panic`).
    Message,
    /// A member's bare name (the reflection tests).
    MemberName,
    /// A hook's `@`-name (`@has_hook`).
    HookName,
}

impl Slot {
    /// Whether typeck's common path checks a call against this slot (the
    /// others describe a specially checked operand).
    #[must_use]
    pub fn is_checked(self) -> bool {
        matches!(
            self,
            Self::Builtin(_)
                | Self::Ref(_)
                | Self::AnyBit
                | Self::ExpectedBit
                | Self::AnyFloat
                | Self::ExpectedFloat
        )
    }
}

impl std::fmt::Display for Slot {
    /// The slot as a Lilac signature spells it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Builtin(ty) => f.write_str(ty.name()),
            Self::Ref(ty) => write!(f, "ref {}", ty.name()),
            Self::AnyBit | Self::ExpectedBit => f.write_str("@bitN"),
            Self::AnyFloat | Self::ExpectedFloat => f.write_str("@fbitN"),
            Self::Type => f.write_str("Type"),
            Self::Param(name) => f.write_str(name),
            Self::RefParam(name) => write!(f, "ref {name}"),
            Self::RefArray => f.write_str("ref @array(T, N)"),
            Self::FormatArgs => f.write_str("@format_args(W)"),
            Self::StringLiteral => f.write_str("\"string literal\""),
            Self::Message => f.write_str("Message"),
            Self::MemberName => f.write_str("name"),
            Self::HookName => f.write_str("@hook"),
        }
    }
}

/// One named intrinsic parameter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Param {
    /// Its name, as a signature shows it.
    pub name: &'static str,
    /// What it takes.
    pub slot: Slot,
}

/// An intrinsic signature.
#[derive(Clone, Debug)]
pub struct Signature {
    /// The parameters, in order.
    pub params: Vec<Param>,
    /// Result slot (`None` for statements).
    pub result: Option<Slot>,
}

impl Signature {
    /// The parameter slots, in order.
    pub fn slots(&self) -> impl Iterator<Item = Slot> + '_ {
        self.params.iter().map(|param| param.slot)
    }
}

impl Intrinsic {
    /// The intrinsics with a fixed name.
    const PLAIN: [(&'static str, Self); 46] = [
        ("@truncate", Self::Truncate),
        ("@extend_zero", Self::ExtendZero),
        ("@extend_sign", Self::ExtendSign),
        ("@int_to_float", Self::IntToFloat),
        ("@uint_to_float", Self::UintToFloat),
        ("@float_to_int", Self::FloatToInt),
        ("@float_to_uint", Self::FloatToUint),
        ("@float_convert", Self::FloatConvert),
        ("@size_of", Self::SizeOf),
        ("@load_u8", Self::LoadU8),
        ("@store_u8", Self::StoreU8),
        ("@memcpy", Self::Memcpy),
        ("@memmove", Self::Memmove),
        ("@memset", Self::Memset),
        ("@align_of", Self::AlignOf),
        ("@ptr_add", Self::PtrAdd),
        ("@ptr_to_address", Self::PtrToAddress),
        ("@address_to_ptr", Self::AddressToPtr),
        ("@linux_syscall", Self::LinuxSyscall),
        ("@linux_thread_clone", Self::LinuxThreadClone),
        ("@trap", Self::Trap),
        ("@spin_hint", Self::SpinHint),
        ("@byte_buffer_ptr", Self::ByteBufferPtr),
        ("@byte_buffer_len", Self::ByteBufferLen),
        ("@byte_buffer_from", Self::ByteBufferFrom),
        ("@format_args_holes", Self::FormatArgsHoles),
        ("@format_args_piece", Self::FormatArgsPiece),
        ("@format_args_hole", Self::FormatArgsHole),
        ("@compile_error", Self::CompileError),
        ("@panic", Self::Panic),
        ("@array_at", Self::ArrayAt),
        ("@array_len", Self::ArrayLen),
        ("@move", Self::Move),
        ("@write", Self::Write),
        ("@read", Self::Read),
        ("@drop", Self::Drop),
        ("@needs_drop", Self::NeedsDrop),
        ("@discard", Self::Discard),
        ("@has_field", Self::HasField),
        ("@has_method", Self::HasMethod),
        ("@has_function", Self::HasFunction),
        ("@has_constant", Self::HasConstant),
        ("@has_variant", Self::HasVariant),
        ("@has_hook", Self::HasHook),
        ("@is_copyable", Self::IsCopyable),
        ("@is_same", Self::IsSame),
    ];

    /// Whether this is a memory builtin: `@move`, `@write`, `@read`, `@drop`,
    /// `@needs_drop` or `@discard` (checked specially, by typeck's
    /// `memory.rs`).
    #[must_use]
    pub fn is_memory(self) -> bool {
        matches!(
            self,
            Self::Move | Self::Write | Self::Read | Self::Drop | Self::NeedsDrop | Self::Discard
        )
    }

    /// Whether this is a reflection test: `@has_field`, `@has_method`,
    /// `@has_function`, `@has_constant`, `@has_variant`, `@has_hook`,
    /// `@is_copyable` or `@is_same` (checked specially, by typeck's
    /// `reflect.rs`).
    #[must_use]
    pub fn is_reflection(self) -> bool {
        matches!(
            self,
            Self::HasField
                | Self::HasMethod
                | Self::HasFunction
                | Self::HasConstant
                | Self::HasVariant
                | Self::HasHook
                | Self::IsCopyable
                | Self::IsSame
        )
    }

    /// Looks an intrinsic up by `@`-name.
    #[must_use]
    pub fn lookup(name: &str) -> Option<Self> {
        if let Some(rest) = name.strip_prefix("@i_") {
            let (op, width) = rest.rsplit_once('_')?;
            return Some(Self::Int(NumOp::parse(op, false)?, IntWidth::parse(width)?));
        }
        if let Some(rest) = name.strip_prefix("@bit_") {
            let (op, width) = rest.rsplit_once('_')?;
            let (op, width) = (NumOp::parse_bitwise(op)?, IntWidth::parse(width)?);
            // A byte has no bytes to swap.
            if op == NumOp::SwapBytes && width == IntWidth::W8 {
                return None;
            }
            return Some(Self::Int(op, width));
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
        Self::PLAIN
            .iter()
            .find(|(plain, _)| *plain == name)
            .map(|(_, intrinsic)| *intrinsic)
    }

    /// What replaced a removed intrinsic, for the `E021` on its old name.
    #[must_use]
    pub fn replaced(name: &str) -> Option<&'static str> {
        Some(match name {
            "@format_args_new" | "@format_args_push" => {
                "f-strings build formats now: write `$\"...\"` where an `@format_args(W)` is expected"
            }
            "@format_args_count" => {
                "`@format_args_count` is now `@format_args_holes`, the number of holes (one less than the pieces)"
            }
            _ => return None,
        })
    }

    /// Every intrinsic name, for editor completion.
    #[must_use]
    pub fn names() -> Vec<String> {
        const INT_WIDTHS: [&str; 5] = ["8", "16", "32", "64", "native"];
        const FLOAT_WIDTHS: [&str; 4] = ["16", "32", "64", "128"];
        const NUM_OPS: [&str; 39] = [
            "add_overflows",
            "add_overflows_u",
            "sub_overflows",
            "sub_overflows_u",
            "mul_overflows",
            "mul_overflows_u",
            "add_saturating",
            "add_saturating_u",
            "sub_saturating",
            "sub_saturating_u",
            "multiply_high",
            "multiply_high_u",
            "sqrt",
            "multiply_add",
            "abs",
            "copy_sign",
            "floor",
            "ceil",
            "trunc",
            "round",
            "round_even",
            "min",
            "max",
            "add",
            "sub",
            "mul",
            "div",
            "mod",
            "negate",
            "less_than",
            "greater_than",
            "less_than_or_equal",
            "greater_than_or_equal",
            "div_u",
            "mod_u",
            "less_than_u",
            "greater_than_u",
            "less_than_or_equal_u",
            "greater_than_or_equal_u",
        ];
        const BIT_OPS: [&str; 14] = [
            "and",
            "or",
            "xor",
            "not",
            "shift_left",
            "shift_right",
            "shift_right_u",
            "rotate_left",
            "rotate_right",
            "count_ones",
            "leading_zeros",
            "trailing_zeros",
            "swap_bytes",
            "reverse",
        ];
        const ORDERINGS: [&str; 5] = ["", "_relaxed", "_acquire", "_release", "_acq_rel"];
        let mut names: Vec<String> = Self::PLAIN
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        for op in NUM_OPS {
            names.extend(INT_WIDTHS.iter().map(|width| format!("@i_{op}_{width}")));
            names.extend(FLOAT_WIDTHS.iter().map(|width| format!("@f_{op}_{width}")));
        }
        for op in BIT_OPS {
            names.extend(INT_WIDTHS.iter().map(|width| format!("@bit_{op}_{width}")));
        }
        for (op, _) in AtomicOp::NAMES {
            for ordering in ORDERINGS {
                names.extend(
                    INT_WIDTHS
                        .iter()
                        .map(|width| format!("@atomic_{op}{ordering}_{width}")),
                );
            }
        }
        names.extend(
            ORDERINGS
                .iter()
                .map(|ordering| format!("@atomic_fence{ordering}")),
        );
        for width in FLOAT_WIDTHS {
            names.push(format!("@fbit_to_bits_{width}"));
            names.push(format!("@bits_to_fbit_{width}"));
        }
        names.retain(|name| Self::lookup(name).is_some());
        names.sort();
        names.dedup();
        names
    }

    /// Whether typeck checks a call specially, not against the signature:
    /// a type or literal operand (`@size_of`, `@align_of`, `@array_len`,
    /// `@needs_drop`, `@compile_error`), any array (`@array_at`), any format
    /// (the `@format_args_*` consumers), a place or `ref` of any type (the
    /// memory builtins), the panic hook's message (`@panic`), or types and
    /// member names (the reflection tests). Their
    /// signatures only describe the operands, with slots that are not
    /// [`Slot::is_checked`].
    #[must_use]
    pub fn is_special(self) -> bool {
        self.is_memory()
            || self.is_reflection()
            || matches!(
                self,
                Self::SizeOf
                    | Self::AlignOf
                    | Self::ArrayLen
                    | Self::ArrayAt
                    | Self::CompileError
                    | Self::Panic
                    | Self::FormatArgsHoles
                    | Self::FormatArgsPiece
                    | Self::FormatArgsHole
            )
    }

    /// The signature: what a call is checked against, or, for
    /// [`Intrinsic::is_special`] ones, what typeck checks it against
    /// spelled out. Every parameter is named.
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn signature(self) -> Signature {
        use Slot::{
            AnyBit, AnyFloat, Builtin, ExpectedBit, ExpectedFloat, FormatArgs, Message, Ref,
            RefArray, RefParam, StringLiteral, Type,
        };
        let native = Builtin(Ty::BitNative);
        let sig = |params: &[(&'static str, Slot)], result: Option<Slot>| Signature {
            params: params
                .iter()
                .map(|&(name, slot)| Param { name, slot })
                .collect(),
            result,
        };
        match self {
            Self::Int(op, width) => {
                let ty = Builtin(width.builtin());
                Self::numeric(op, ty)
            }
            Self::Float(op, width) => {
                let ty = Builtin(width.builtin());
                Self::numeric(op, ty)
            }
            Self::FbitToBits(width) => sig(
                &[("value", Builtin(width.builtin()))],
                Some(Builtin(width.bits().unwrap_or(Ty::Bit64))),
            ),
            Self::BitsToFbit(width) => sig(
                &[("value", Builtin(width.bits().unwrap_or(Ty::Bit64)))],
                Some(Builtin(width.builtin())),
            ),
            Self::Truncate | Self::ExtendZero | Self::ExtendSign => {
                sig(&[("value", AnyBit)], Some(ExpectedBit))
            }
            Self::IntToFloat | Self::UintToFloat => sig(&[("value", AnyBit)], Some(ExpectedFloat)),
            Self::FloatToInt | Self::FloatToUint => sig(&[("value", AnyFloat)], Some(ExpectedBit)),
            Self::FloatConvert => sig(&[("value", AnyFloat)], Some(ExpectedFloat)),
            Self::SizeOf | Self::AlignOf => sig(&[("type", Type)], Some(native)),
            Self::ArrayLen => sig(&[("array", Type)], Some(native)),
            Self::ArrayAt => sig(
                &[("array", RefArray), ("index", native)],
                Some(RefParam("T")),
            ),
            Self::NeedsDrop | Self::IsCopyable => sig(&[("type", Type)], Some(Builtin(Ty::Bit8))),
            Self::CompileError => sig(&[("message", StringLiteral)], None),
            Self::Panic => sig(&[("message", Message)], None),
            Self::Move => sig(&[("place", Slot::Param("T"))], Some(Slot::Param("T"))),
            Self::Discard => sig(&[("place", Slot::Param("T"))], None),
            Self::HasField | Self::HasMethod | Self::HasFunction | Self::HasConstant => sig(
                &[("type", Type), ("name", Slot::MemberName), ("member", Type)],
                Some(Builtin(Ty::Bit8)),
            ),
            Self::HasVariant => sig(
                &[
                    ("type", Type),
                    ("name", Slot::MemberName),
                    ("payload", Type),
                ],
                Some(Builtin(Ty::Bit8)),
            ),
            Self::HasHook => sig(
                &[("hook", Slot::HookName), ("type", Type)],
                Some(Builtin(Ty::Bit8)),
            ),
            Self::IsSame => sig(
                &[("first", Type), ("second", Type)],
                Some(Builtin(Ty::Bit8)),
            ),
            Self::Write => sig(
                &[("address", RefParam("T")), ("value", Slot::Param("T"))],
                None,
            ),
            Self::Read => sig(&[("address", RefParam("T"))], Some(Slot::Param("T"))),
            Self::Drop => sig(&[("address", RefParam("T"))], None),
            Self::Trap | Self::SpinHint | Self::Fence(_) => sig(&[], None),
            Self::Atomic(op, _, width) => {
                let ty = width.builtin();
                let address = ("address", Ref(ty));
                let params: &[(&'static str, Slot)] = match op {
                    AtomicOp::Load => &[address],
                    AtomicOp::Store => &[address, ("value", Builtin(ty))],
                    AtomicOp::CompareSwap => {
                        &[address, ("expected", Builtin(ty)), ("desired", Builtin(ty))]
                    }
                    _ => &[address, ("operand", Builtin(ty))],
                };
                sig(params, (op != AtomicOp::Store).then_some(Builtin(ty)))
            }
            Self::LoadU8 => sig(
                &[("base", Ref(Ty::Bit8)), ("offset", native)],
                Some(Builtin(Ty::Bit8)),
            ),
            Self::StoreU8 => sig(
                &[
                    ("base", Ref(Ty::Bit8)),
                    ("offset", native),
                    ("value", Builtin(Ty::Bit8)),
                ],
                None,
            ),
            Self::Memcpy | Self::Memmove => sig(
                &[
                    ("destination", Ref(Ty::Unknown)),
                    ("source", Ref(Ty::Unknown)),
                    ("length", native),
                ],
                None,
            ),
            Self::Memset => sig(
                &[
                    ("destination", Ref(Ty::Unknown)),
                    ("byte", Builtin(Ty::Bit8)),
                    ("length", native),
                ],
                None,
            ),
            Self::PtrAdd => sig(
                &[("pointer", Ref(Ty::Unknown)), ("offset", native)],
                Some(Ref(Ty::Unknown)),
            ),
            Self::PtrToAddress => sig(&[("pointer", Ref(Ty::Unknown))], Some(native)),
            Self::AddressToPtr => sig(&[("address", native)], Some(Ref(Ty::Unknown))),
            Self::LinuxSyscall => sig(
                &[
                    ("number", native),
                    ("a0", native),
                    ("a1", native),
                    ("a2", native),
                    ("a3", native),
                    ("a4", native),
                    ("a5", native),
                ],
                Some(native),
            ),
            Self::LinuxThreadClone => {
                let opaque = Ref(Ty::Unknown);
                sig(
                    &[
                        ("flags", native),
                        ("stack", opaque),
                        ("parent_tid", opaque),
                        ("child_tid", opaque),
                        ("tls_area", opaque),
                        ("tls_size", native),
                        ("entry", opaque),
                        ("argument", opaque),
                    ],
                    Some(native),
                )
            }
            Self::ByteBufferPtr => sig(&[("buffer", Builtin(Ty::ByteBuffer))], Some(Ref(Ty::Bit8))),
            Self::ByteBufferLen => sig(&[("buffer", Builtin(Ty::ByteBuffer))], Some(native)),
            Self::ByteBufferFrom => sig(
                &[("pointer", Ref(Ty::Bit8)), ("length", native)],
                Some(Builtin(Ty::ByteBuffer)),
            ),
            Self::FormatArgsHoles => sig(&[("format", FormatArgs)], Some(native)),
            Self::FormatArgsPiece => sig(
                &[("format", FormatArgs), ("index", native)],
                Some(Builtin(Ty::ByteBuffer)),
            ),
            Self::FormatArgsHole => sig(
                &[
                    ("format", FormatArgs),
                    ("index", native),
                    ("out", RefParam("W")),
                ],
                None,
            ),
        }
    }

    /// An integer or float operation's signature over its container `ty`.
    fn numeric(op: NumOp, ty: Slot) -> Signature {
        let param = |name| Param { name, slot: ty };
        let params = match op {
            _ if op.is_unary() => vec![param("value")],
            NumOp::Shl | NumOp::Shr | NumOp::ShrU | NumOp::RotateLeft | NumOp::RotateRight => {
                vec![param("value"), param("amount")]
            }
            NumOp::CopySign => vec![param("magnitude"), param("sign")],
            NumOp::MultiplyAdd => vec![param("left"), param("right"), param("addend")],
            _ => vec![param("left"), param("right")],
        };
        let result = if op.is_comparison() {
            Slot::Builtin(Ty::Bit8)
        } else {
            ty
        };
        Signature {
            params,
            result: Some(result),
        }
    }

    /// What the intrinsic does, as Markdown for an editor's hover.
    #[allow(clippy::too_many_lines)]
    #[must_use]
    pub fn doc(self) -> String {
        let text = match self {
            Self::Int(op, _) => return Self::numeric_doc(op, false),
            Self::Float(op, _) => return Self::numeric_doc(op, true),
            Self::FbitToBits(_) => "The float's bits as an integer of the same width.",
            Self::BitsToFbit(_) => "The integer's bits as a float of the same width.",
            Self::Truncate => {
                "Keeps the low bits of `value` that fit the expected bit type, which must not be wider."
            }
            Self::ExtendZero => {
                "Widens `value` to the expected bit type, filling the new high bits with zeros."
            }
            Self::ExtendSign => {
                "Widens `value` to the expected bit type, filling the new high bits with its sign bit."
            }
            Self::IntToFloat => {
                "`value` as a signed integer, converted to the expected float type, rounded to nearest even."
            }
            Self::UintToFloat => {
                "`value` as an unsigned integer, converted to the expected float type, rounded to nearest even."
            }
            Self::FloatToInt => {
                "`value` truncated toward zero to the expected bit type as a signed integer; past the range it saturates at `MIN` or `MAX`, and NaN is 0."
            }
            Self::FloatToUint => {
                "`value` truncated toward zero to the expected bit type as an unsigned integer; past the range it saturates at 0 or `MAX`, and NaN is 0."
            }
            Self::FloatConvert => {
                "`value` converted to the expected float type: widened exactly, or narrowed rounding to nearest even."
            }
            Self::SizeOf => {
                "The size in bytes of a type, or of a value's type: the C `sizeof` on the target."
            }
            Self::AlignOf => {
                "The alignment in bytes of a type, or of a value's type: the C `_Alignof` on the target."
            }
            Self::LoadU8 => "Reads the byte `offset` bytes past `base`.",
            Self::StoreU8 => "Writes `value` to the byte `offset` bytes past `base`.",
            Self::Memcpy => {
                "Copies `length` bytes from `source` to `destination`. The ranges must not overlap; use `@memmove` when they can."
            }
            Self::Memmove => {
                "Copies `length` bytes from `source` to `destination`, which may overlap."
            }
            Self::Memset => "Sets `length` bytes at `destination` to `byte`.",
            Self::PtrAdd => "The address `offset` bytes past `pointer`.",
            Self::PtrToAddress => "A reference's address as an integer.",
            Self::AddressToPtr => "An integer address as a reference.",
            Self::LinuxSyscall => {
                "The Linux system-call instruction: system call `number` with six arguments, returning its raw result (a negative errno on failure). Only on Linux x86-64 and aarch64 (`E035` elsewhere)."
            }
            Self::LinuxThreadClone => {
                "Linux `clone` of a thread that calls the C function `entry(argument)` on `stack`, then exits. With `CLONE_SETTLS`, its thread-local storage is built in the zeroed `tls_area` of `tls_size` bytes. Only on Linux x86-64 and aarch64 (`E035` elsewhere)."
            }
            Self::Trap => "Stops the program at once with a trap instruction.",
            Self::SpinHint => {
                "Tells the processor this thread is waiting in a spin loop (x86 `pause`, Arm `yield`), which saves power and frees a shared core; does nothing on other targets. Use one in each turn of a loop that rereads a value another thread will change."
            }
            Self::ByteBufferPtr => "Where the buffer's bytes start.",
            Self::ByteBufferLen => "How many bytes the buffer holds.",
            Self::ByteBufferFrom => "A buffer over the `length` bytes at `pointer`.",
            Self::FormatArgsHoles => {
                "How many holes the format has (it has one more literal piece)."
            }
            Self::FormatArgsPiece => {
                "Literal piece `index` of the format, from 0 to the hole count; panics out of bounds."
            }
            Self::FormatArgsHole => {
                "Formats hole `index` into `out`, a `ref W` for the format's own writer `W`; panics out of bounds."
            }
            Self::CompileError => {
                "Reports `message` as a compile error when a live path reaches it (after constant conditions and dead code are removed)."
            }
            Self::Panic => {
                "Ends the program: writes `panic: message` and a stack trace through `@compiler_print`, then exits with 101. No drops run. `Message` is what `@compiler_print` takes."
            }
            Self::Atomic(op, _, _) => return Self::atomic_doc(op),
            Self::Fence(_) => {
                "A memory fence: orders the memory operations around it (sequentially consistent when the name has no ordering)."
            }
            Self::ArrayAt => "A `ref` to element `index` of the array; panics out of bounds.",
            Self::ArrayLen => "The length of an array type, or of a value's array type.",
            Self::Move => {
                "Moves `place` out, whatever its type: no copy hook runs, and the place is empty after."
            }
            Self::Write => {
                "Moves `value` into the empty slot at `address`; nothing there is dropped."
            }
            Self::Read => {
                "Moves the `T` at `address` out, leaving the slot empty; no copy hook runs."
            }
            Self::Drop => "Drops the `T` at `address`, leaving the slot empty.",
            Self::NeedsDrop => "Whether dropping a `type` does anything: a constant `@bit8`.",
            Self::Discard => {
                "Inside a function of `T`, ends `place`'s whole life without its `@on_drop`; its fields become places of their own."
            }
            Self::HasField => {
                "Whether a value of `type` has a field `name` of type `member`: a constant `@bit8`."
            }
            Self::HasMethod => {
                "Whether a value of `type` has a method `name` of exactly the function type `member`, its receiver first: a constant `@bit8`."
            }
            Self::HasFunction => {
                "Whether `type` has a static function `name` of exactly the function type `member`: a constant `@bit8`."
            }
            Self::HasConstant => {
                "Whether `type` has a member constant `name` of type `member`: a constant `@bit8`."
            }
            Self::HasVariant => {
                "Whether `type` has a variant `name` with exactly the payload type `payload` (left out: none): a constant `@bit8`."
            }
            Self::HasHook => {
                "Whether the hook `hook` has an overload of exactly the function type `type`, the one an operator, a hole or a call of it would bind: a constant `@bit8`."
            }
            Self::IsCopyable => "Whether a by-value use of a `type` copies: a constant `@bit8`.",
            Self::IsSame => "Whether `first` and `second` are one type: a constant `@bit8`.",
        };
        text.to_owned()
    }

    /// The doc of an `@i_`, `@bit_` or `@f_` operation.
    fn numeric_doc(op: NumOp, float: bool) -> String {
        let signed = if float { "" } else { " (signed)" };
        match op {
            NumOp::Add => "`left + right`, wrapping.".to_owned(),
            NumOp::Sub => "`left - right`, wrapping.".to_owned(),
            NumOp::Mul => "`left * right`, wrapping.".to_owned(),
            NumOp::Div if float => "`left / right`.".to_owned(),
            NumOp::Div => {
                "`left / right` (signed, rounding toward zero); panics on a zero divisor or `MIN / -1`."
                    .to_owned()
            }
            NumOp::DivU => {
                "`left / right` (unsigned); panics on a zero divisor.".to_owned()
            }
            NumOp::Mod if float => "The remainder of `left / right`, as C's `fmod`.".to_owned(),
            NumOp::Mod => {
                "The remainder of `left / right` (signed, the sign of `left`); panics on a zero divisor or `MIN / -1`."
                    .to_owned()
            }
            NumOp::ModU => {
                "The remainder of `left / right` (unsigned); panics on a zero divisor.".to_owned()
            }
            NumOp::Negate => "`-value`.".to_owned(),
            NumOp::Lt => format!("Whether `left < right`{signed}: `0x1` or `0x0`."),
            NumOp::Gt => format!("Whether `left > right`{signed}: `0x1` or `0x0`."),
            NumOp::Le => format!("Whether `left <= right`{signed}: `0x1` or `0x0`."),
            NumOp::Ge => format!("Whether `left >= right`{signed}: `0x1` or `0x0`."),
            NumOp::LtU => "Whether `left < right` (unsigned): `0x1` or `0x0`.".to_owned(),
            NumOp::GtU => "Whether `left > right` (unsigned): `0x1` or `0x0`.".to_owned(),
            NumOp::LeU => "Whether `left <= right` (unsigned): `0x1` or `0x0`.".to_owned(),
            NumOp::GeU => "Whether `left >= right` (unsigned): `0x1` or `0x0`.".to_owned(),
            NumOp::And => "Bitwise `left & right`.".to_owned(),
            NumOp::Or => "Bitwise `left | right`.".to_owned(),
            NumOp::Xor => "Bitwise `left ^ right`.".to_owned(),
            NumOp::Not => "Bitwise `~value`.".to_owned(),
            NumOp::Shl => {
                "`value << amount`, the amount taken modulo the width.".to_owned()
            }
            NumOp::Shr => {
                "`value >> amount`, copying the sign bit in (arithmetic); the amount is taken modulo the width."
                    .to_owned()
            }
            NumOp::ShrU => {
                "`value >> amount`, shifting zeros in (logical); the amount is taken modulo the width."
                    .to_owned()
            }
            _ => Self::numeric_doc_more(op).to_owned(),
        }
    }

    /// The doc of the operations past the basic arithmetic.
    fn numeric_doc_more(op: NumOp) -> &'static str {
        match op {
            NumOp::RotateLeft => "`value` rotated left by `amount`, taken modulo the width.",
            NumOp::RotateRight => "`value` rotated right by `amount`, taken modulo the width.",
            NumOp::CountOnes => "How many bits of `value` are set.",
            NumOp::LeadingZeros => {
                "How many zero bits `value` has above its highest set bit: the width for zero."
            }
            NumOp::TrailingZeros => {
                "How many zero bits `value` has below its lowest set bit: the width for zero."
            }
            NumOp::SwapBytes => "`value` with its bytes in reverse order.",
            NumOp::Reverse => "`value` with its bits in reverse order.",
            NumOp::AddOverflows => {
                "Whether `left + right` (signed) overflows, so the wrapped sum is wrong: `0x1` or `0x0`."
            }
            NumOp::AddOverflowsU => {
                "Whether `left + right` (unsigned) overflows, so the wrapped sum is wrong: `0x1` or `0x0`."
            }
            NumOp::SubOverflows => {
                "Whether `left - right` (signed) overflows, so the wrapped difference is wrong: `0x1` or `0x0`."
            }
            NumOp::SubOverflowsU => {
                "Whether `left - right` (unsigned) goes below zero, so the wrapped difference is wrong: `0x1` or `0x0`."
            }
            NumOp::MulOverflows => {
                "Whether `left * right` (signed) overflows, so the wrapped product is wrong: `0x1` or `0x0`."
            }
            NumOp::MulOverflowsU => {
                "Whether `left * right` (unsigned) overflows, so the wrapped product is wrong: `0x1` or `0x0`."
            }
            NumOp::AddSaturating => "`left + right` (signed), clamped to `MIN` and `MAX`.",
            NumOp::AddSaturatingU => "`left + right` (unsigned), clamped to `MAX`.",
            NumOp::SubSaturating => "`left - right` (signed), clamped to `MIN` and `MAX`.",
            NumOp::SubSaturatingU => "`left - right` (unsigned), clamped to 0.",
            NumOp::MultiplyHigh => {
                "The upper half of the double-width product `left * right` (signed)."
            }
            NumOp::MultiplyHighU => {
                "The upper half of the double-width product `left * right` (unsigned)."
            }
            NumOp::Sqrt => "The square root of `value`, rounded once; NaN below zero.",
            NumOp::MultiplyAdd => "`left * right + addend`, rounded once.",
            NumOp::Abs => "`value` with its sign bit cleared.",
            NumOp::CopySign => "`magnitude` with the sign bit of `sign`.",
            NumOp::Floor => "The greatest integral value not above `value`.",
            NumOp::Ceil => "The least integral value not below `value`.",
            NumOp::Trunc => "`value` rounded toward zero to an integral value.",
            NumOp::Round => {
                "`value` rounded to the nearest integral value, ties away from zero."
            }
            NumOp::RoundEven => "`value` rounded to the nearest integral value, ties to even.",
            NumOp::Min => {
                "The lesser of `left` and `right` (IEEE `minimum`): NaN when either is NaN, and `-0` is less than `+0`."
            }
            NumOp::Max => {
                "The greater of `left` and `right` (IEEE `maximum`): NaN when either is NaN, and `+0` is greater than `-0`."
            }
            _ => "",
        }
    }

    /// The doc of an `@atomic_` operation.
    fn atomic_doc(op: AtomicOp) -> String {
        let text = match op {
            AtomicOp::Load => "Atomically reads the value at `address`.",
            AtomicOp::Store => "Atomically writes `value` to `address`.",
            AtomicOp::Swap => {
                "Atomically writes `operand` to `address`, returning the old value."
            }
            AtomicOp::CompareSwap => {
                "Atomically writes `desired` to `address` if it holds `expected`, returning the old value (equal to `expected` exactly when it wrote)."
            }
            AtomicOp::Add => "Atomically adds `operand` at `address` (wrapping), returning the old value.",
            AtomicOp::Sub => {
                "Atomically subtracts `operand` at `address` (wrapping), returning the old value."
            }
            AtomicOp::And => "Atomically ands `operand` into `address`, returning the old value.",
            AtomicOp::Or => "Atomically ors `operand` into `address`, returning the old value.",
            AtomicOp::Xor => "Atomically xors `operand` into `address`, returning the old value.",
            AtomicOp::Max => {
                "Atomically keeps the greater (signed) of `operand` and the value at `address`, returning the old value."
            }
            AtomicOp::Min => {
                "Atomically keeps the lesser (signed) of `operand` and the value at `address`, returning the old value."
            }
            AtomicOp::MaxU => {
                "Atomically keeps the greater (unsigned) of `operand` and the value at `address`, returning the old value."
            }
            AtomicOp::MinU => {
                "Atomically keeps the lesser (unsigned) of `operand` and the value at `address`, returning the old value."
            }
        };
        format!(
            "{text} Naturally aligned; sequentially consistent unless the name gives an ordering."
        )
    }

    /// The backend leaf builtin (`lilac_ir::builtin_sig`) this intrinsic
    /// calls, if it is one.
    #[must_use]
    pub fn leaf_builtin(self) -> Option<&'static str> {
        Some(match self {
            Self::Memcpy => "@memcpy",
            Self::Memmove => "@memmove",
            Self::Memset => "@memset",
            Self::PtrAdd => "@ptr_add",
            Self::LinuxSyscall => "@linux_syscall",
            Self::LinuxThreadClone => "@linux_thread_clone",
            Self::Trap => "@lilac_abort",
            Self::SpinHint => "@spin_hint",

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
            Self::FormatArgsHoles => ".lilac.format_args_holes",
            Self::FormatArgsPiece => ".lilac.format_args_piece",
            Self::FormatArgsHole => ".lilac.format_args_hole",
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
        assert_eq!(
            Intrinsic::lookup("@bit_shift_right_u_64"),
            Some(Intrinsic::Int(NumOp::ShrU, IntWidth::W64))
        );
        assert_eq!(
            Intrinsic::lookup("@bit_not_native"),
            Some(Intrinsic::Int(NumOp::Not, IntWidth::Native))
        );
        assert_eq!(Intrinsic::lookup("@i_and_32"), None);
        assert_eq!(Intrinsic::lookup("@bit_add_32"), None);
        assert_eq!(Intrinsic::lookup("@bit_native"), None);
        assert_eq!(Intrinsic::lookup("@f_xor_32"), None);
        assert_eq!(Intrinsic::lookup("@i_add_128"), None);
        assert_eq!(Intrinsic::lookup("@fbit_to_bits_128"), None);
        assert_eq!(Intrinsic::lookup("@i_add"), None);
        assert_eq!(Intrinsic::lookup("@compilerError"), None);
        assert_eq!(Intrinsic::lookup("@size_of"), Some(Intrinsic::SizeOf));
        assert_eq!(Intrinsic::lookup("@panic"), Some(Intrinsic::Panic));
        assert_eq!(Intrinsic::lookup("@align_of"), Some(Intrinsic::AlignOf));
        assert_eq!(Intrinsic::lookup("@ptr_add"), Some(Intrinsic::PtrAdd));
        assert_eq!(Intrinsic::lookup("@trap"), Some(Intrinsic::Trap));
        assert_eq!(Intrinsic::lookup("@move"), Some(Intrinsic::Move));
        assert_eq!(Intrinsic::lookup("@needs_drop"), Some(Intrinsic::NeedsDrop));
        assert_eq!(Intrinsic::lookup("@has_method"), Some(Intrinsic::HasMethod));
        assert_eq!(Intrinsic::lookup("@is_same"), Some(Intrinsic::IsSame));
        assert_eq!(Intrinsic::lookup("@has_member"), None);
    }

    #[test]
    fn names_are_all_intrinsics() {
        let names = Intrinsic::names();
        assert!(names.iter().all(|name| Intrinsic::lookup(name).is_some()));
        for name in [
            "@i_add_8",
            "@f_div_32",
            "@atomic_load_acquire_64",
            "@atomic_fence",
            "@memcpy",
        ] {
            assert!(names.iter().any(|known| known == name), "{name}");
        }
        assert!(!names.iter().any(|known| known == "@f_div_u_32"));
        assert!(!names.iter().any(|known| known == "@atomic_load_release_64"));
    }

    #[test]
    fn phase_1_operations_are_named_and_typed() {
        let names = Intrinsic::names();
        for name in [
            "@f_sqrt_16",
            "@f_multiply_add_64",
            "@f_round_even_32",
            "@f_min_128",
            "@f_copy_sign_64",
            "@bit_count_ones_native",
            "@bit_rotate_left_8",
            "@bit_swap_bytes_16",
            "@bit_reverse_8",
            "@i_mul_overflows_u_64",
            "@i_sub_saturating_32",
            "@i_multiply_high_u_native",
            "@int_to_float",
            "@uint_to_float",
            "@float_to_int",
            "@float_to_uint",
            "@float_convert",
        ] {
            assert!(names.iter().any(|known| known == name), "{name}");
            assert!(Intrinsic::lookup(name).is_some(), "{name}");
        }
        // A byte swap needs two bytes; the float forms have no `_u`, and
        // the bit forms no float.
        for name in [
            "@bit_swap_bytes_8",
            "@f_sqrt_u_32",
            "@i_sqrt_32",
            "@f_rotate_left_32",
            "@f_count_ones_32",
            "@i_multiply_add_32",
            "@f_add_saturating_32",
        ] {
            assert_eq!(Intrinsic::lookup(name), None, "{name}");
            assert!(!names.iter().any(|known| known == name), "{name}");
        }
        let flag = Intrinsic::lookup("@i_add_overflows_u_16")
            .expect("known")
            .signature();
        assert_eq!(flag.result, Some(Slot::Builtin(Ty::Bit8)));
        assert_eq!(
            slots("@i_add_overflows_u_16"),
            vec![Slot::Builtin(Ty::Bit16), Slot::Builtin(Ty::Bit16)]
        );
        assert_eq!(
            slots("@f_multiply_add_32"),
            vec![Slot::Builtin(Ty::FBit32); 3]
        );
        let count = Intrinsic::lookup("@bit_leading_zeros_64")
            .expect("known")
            .signature();
        assert_eq!(count.result, Some(Slot::Builtin(Ty::Bit64)));
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

    fn slots(intrinsic: &str) -> Vec<Slot> {
        Intrinsic::lookup(intrinsic)
            .expect("known")
            .signature()
            .slots()
            .collect()
    }

    #[test]
    fn atomic_signatures() {
        assert_eq!(
            slots("@atomic_compare_swap_32"),
            vec![
                Slot::Ref(Ty::Bit32),
                Slot::Builtin(Ty::Bit32),
                Slot::Builtin(Ty::Bit32)
            ]
        );
        let sig = Intrinsic::lookup("@atomic_compare_swap_32")
            .expect("known")
            .signature();
        assert_eq!(sig.result, Some(Slot::Builtin(Ty::Bit32)));
        assert_eq!(
            slots("@atomic_store_8"),
            vec![Slot::Ref(Ty::Bit8), Slot::Builtin(Ty::Bit8)]
        );
        let sig = Intrinsic::lookup("@atomic_store_8")
            .expect("known")
            .signature();
        assert_eq!(sig.result, None);
        assert_eq!(slots("@atomic_load_native"), vec![Slot::Ref(Ty::BitNative)]);
    }

    #[test]
    fn instruction_builtin_signatures() {
        let sig = Intrinsic::PtrAdd.signature();
        assert_eq!(
            sig.slots().collect::<Vec<_>>(),
            vec![Slot::Ref(Ty::Unknown), Slot::Builtin(Ty::BitNative)]
        );
        assert_eq!(sig.result, Some(Slot::Ref(Ty::Unknown)));
        assert_eq!(
            Intrinsic::AddressToPtr.signature().result,
            Some(Slot::Ref(Ty::Unknown))
        );
        let sig = Intrinsic::LinuxSyscall.signature();
        assert_eq!(
            sig.slots().collect::<Vec<_>>(),
            vec![Slot::Builtin(Ty::BitNative); 7]
        );
        assert_eq!(sig.result, Some(Slot::Builtin(Ty::BitNative)));
        assert_eq!(
            slots("@memset"),
            vec![
                Slot::Ref(Ty::Unknown),
                Slot::Builtin(Ty::Bit8),
                Slot::Builtin(Ty::BitNative)
            ]
        );
        assert_eq!(slots("@memcpy")[..2], [Slot::Ref(Ty::Unknown); 2]);
        assert_eq!(Intrinsic::Trap.leaf_builtin(), Some("@lilac_abort"));
        assert_eq!(Intrinsic::Memmove.leaf_builtin(), Some("@memmove"));
    }

    #[test]
    fn comparisons_return_bit8() {
        let sig = Intrinsic::lookup("@i_less_than_32")
            .expect("known")
            .signature();
        assert_eq!(sig.result, Some(Slot::Builtin(Ty::Bit8)));
        assert_eq!(sig.params.len(), 2);
        assert_eq!(slots("@f_negate_64"), vec![Slot::Builtin(Ty::FBit64)]);
    }

    /// Every intrinsic has a signature with named parameters (distinct
    /// within it) and a doc, and the specially checked ones are exactly
    /// those whose signature describes an operand typeck's common path
    /// cannot check.
    #[test]
    fn every_signature_is_named_and_documented() {
        for name in Intrinsic::names() {
            let intrinsic = Intrinsic::lookup(&name).expect("known");
            let sig = intrinsic.signature();
            let names: Vec<&str> = sig.params.iter().map(|param| param.name).collect();
            for (at, param) in names.iter().enumerate() {
                assert!(
                    !param.is_empty() && !names[..at].contains(param),
                    "{name}: parameter `{param}`"
                );
            }
            assert!(!intrinsic.doc().is_empty(), "{name} has no doc");
            let described = sig.slots().chain(sig.result).any(|slot| !slot.is_checked());
            if intrinsic.is_special() {
                assert!(
                    described || sig.params.is_empty(),
                    "{name} is checked specially, so its signature must describe its operands"
                );
            } else {
                assert!(
                    !described,
                    "{name} is checked against its signature, so every slot must be checkable"
                );
            }
        }
    }

    #[test]
    fn slots_spell_as_source_does() {
        let spell = |intrinsic: Intrinsic| {
            let sig = intrinsic.signature();
            let params: Vec<String> = sig
                .params
                .iter()
                .map(|param| format!("{}: {}", param.name, param.slot))
                .collect();
            let result = sig
                .result
                .map_or_else(|| "nothing".to_owned(), |slot| slot.to_string());
            format!("({}) {result}", params.join(", "))
        };
        assert_eq!(
            spell(Intrinsic::Memcpy),
            "(destination: ref unknown, source: ref unknown, length: @bit_native) nothing"
        );
        assert_eq!(
            spell(Intrinsic::FormatArgsHole),
            "(format: @format_args(W), index: @bit_native, out: ref W) nothing"
        );
        assert_eq!(
            spell(Intrinsic::ArrayAt),
            "(array: ref @array(T, N), index: @bit_native) ref T"
        );
        assert_eq!(spell(Intrinsic::Read), "(address: ref T) T");
    }
}
