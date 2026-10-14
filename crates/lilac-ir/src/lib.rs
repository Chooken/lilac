//! Lilac IR types, printer and verifier.
//!
//! This crate is the stability boundary: frontend crates build it, backend
//! crates consume it, and nothing else links LLVM.
//!
//! LIR types print exactly as they read (`#bit32`, `ref #bit8`). Names are
//! typed by what they name ([`Reg`], [`PlaceName`], [`Label`], [`Symbol`])
//! and constants are values ([`LirConst`]). Layouts ([`LirLayout`]: each
//! object's fields with their offsets, size and alignment) are computed once
//! by `lilac-layout` for the module's target; everything after reads them.

use std::collections::HashMap;
use std::fmt;

use lilac_layout::{SizeAlign, TooLarge};
use lilac_span::Span;
use lilac_target::{Scalar, Target};

mod dce;
mod names;
mod print;
pub mod runtime;
pub mod trace;
mod ty;
mod verify;

pub use dce::{prune_unreachable, reach, CallGraph, Edge, EdgeKind, Reach};
pub use lilac_float::{FloatBits, FloatWidth};
pub use names::{Label, PlaceName, Reg, Symbol};
pub use runtime::{
    build_runtime, entry_point, hook_externs, panic_runtime, Entry, PanicMessage, PanicRuntime,
};
pub use ty::LirTy;
pub use verify::{check_live_compile_errors, check_live_syscalls, check_live_wide_floats, verify};

/// IR version header; bump on any normative IR change.
pub const LILAC_IR_VERSION: &str = "0.5";

/// An IR module: versioned header plus data, functions, and layouts.
#[derive(Clone, Debug)]
pub struct LirModule {
    /// `lilac_ir = "..."` header value.
    pub version: String,
    /// `target = "..."` header value: fixes `@bit_native`'s width and every
    /// layout.
    pub target: Target,
    /// Typed constants (`!c0: str`, `!c1: numlit`).
    pub data: Vec<LirData>,
    /// f-string templates (`!fmt0: format "a" {@f} "b"`).
    pub formats: Vec<LirFormat>,
    /// Global variables (`global !name: #t = …`).
    pub globals: Vec<LirGlobal>,
    /// Functions in source order.
    pub funcs: Vec<LirFunc>,
    /// Layout name (without `#`) → its layout on `target`.
    pub layouts: HashMap<String, LirLayout>,
}

/// One named aggregate's layout on the module's target, as `lilac-layout`
/// computed it: the verifier checks it, and the printer, lowering and the
/// backends read it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LirLayout {
    /// How the fields are placed.
    pub kind: LirLayoutKind,
    /// `sizeof`, in bytes.
    pub size: u64,
    /// `_Alignof`, in bytes.
    pub align: u64,
    /// The fields, in order, each with its offset.
    pub fields: Vec<LirField>,
}

/// How a [`LirLayout`] places its fields.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum LirLayoutKind {
    /// A C struct: each field after the last ([`lilac_layout::struct_layout`]).
    Struct,
    /// A tagged union: the first field is the integer tag, and every other
    /// field is a payload at one shared offset
    /// ([`lilac_layout::tagged_layout`]).
    Tagged,
}

/// One field of a [`LirLayout`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LirField {
    /// Field name.
    pub name: String,
    /// Field type.
    pub ty: LirTy,
    /// Byte offset from the start of the aggregate.
    pub offset: u64,
}

/// Why a layout could not be computed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LayoutError {
    /// A field's type has no size: `#unknown`, or a name with no layout
    /// (yet).
    Unsized(LirTy),
    /// A tagged layout's first field is not an integer.
    BadTag(LirTy),
    /// A tagged layout has no fields, so no tag.
    NoTag,
    /// Larger than the target allows.
    TooLarge(TooLarge),
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsized(ty) => write!(f, "`{ty}` has no size"),
            Self::BadTag(ty) => write!(f, "the tag `{ty}` is not an integer"),
            Self::NoTag => f.write_str("a tagged layout has no tag field"),
            Self::TooLarge(too_large) => too_large.fmt(f),
        }
    }
}

impl LirLayout {
    /// Lays `fields` out on `target` as `kind` says. Every named type a
    /// field holds by value must be in `layouts` or be a builtin aggregate.
    ///
    /// # Errors
    ///
    /// [`LayoutError`] when a field is unsized, a tag is no integer, or the
    /// aggregate is larger than the target allows.
    pub fn new<S: std::hash::BuildHasher>(
        kind: LirLayoutKind,
        fields: Vec<(String, LirTy)>,
        layouts: &HashMap<String, LirLayout, S>,
        target: &Target,
    ) -> Result<Self, LayoutError> {
        let sizes = |fields: &[(String, LirTy)]| {
            fields
                .iter()
                .map(|(_, ty)| {
                    size_align(layouts, ty, target).ok_or_else(|| LayoutError::Unsized(ty.clone()))
                })
                .collect::<Result<Vec<_>, _>>()
        };
        let placed = match kind {
            LirLayoutKind::Struct => lilac_layout::struct_layout(target, &sizes(&fields)?),
            LirLayoutKind::Tagged => {
                let Some(((_, tag), payloads)) = fields.split_first() else {
                    return Err(LayoutError::NoTag);
                };
                let tag = match tag {
                    LirTy::Bit(bits) => Scalar::bit(u32::from(*bits)),
                    _ => None,
                }
                .ok_or_else(|| LayoutError::BadTag(tag.clone()))?;
                lilac_layout::tagged_layout(target, tag, &sizes(payloads)?)
            }
        }
        .map_err(LayoutError::TooLarge)?;
        Ok(Self {
            kind,
            size: placed.size,
            align: placed.align,
            fields: fields
                .into_iter()
                .zip(placed.fields)
                .map(|((name, ty), offset)| LirField { name, ty, offset })
                .collect(),
        })
    }

    /// A C struct of `fields` ([`LirLayout::new`] with
    /// [`LirLayoutKind::Struct`]).
    ///
    /// # Errors
    ///
    /// As [`LirLayout::new`].
    pub fn of_struct<S: std::hash::BuildHasher>(
        fields: Vec<(String, LirTy)>,
        layouts: &HashMap<String, LirLayout, S>,
        target: &Target,
    ) -> Result<Self, LayoutError> {
        Self::new(LirLayoutKind::Struct, fields, layouts, target)
    }

    /// The `(name, type)` of each field, in order.
    #[must_use]
    pub fn members(&self) -> Vec<(String, LirTy)> {
        self.fields
            .iter()
            .map(|field| (field.name.clone(), field.ty.clone()))
            .collect()
    }
}

/// Canonical module text plus the source span of each physical line.
///
/// `lines[i]` is the span for `text`'s `i`-th line (0-based, matching
/// [`str::lines`]); `None` marks a compiler-generated line (header, `in`/`out`
/// signatures, braces, type sections) with no single source location.
#[derive(Clone, Debug)]
pub struct SpannedText {
    /// Canonical module text, byte-identical to [`LirModule::to_text`].
    pub text: String,
    /// One entry per line of `text`; `None` when no source span applies.
    pub lines: Vec<Option<Span>>,
}

/// One typed constant.
#[derive(Clone, Debug)]
pub struct LirData {
    /// Constant name (`c0` prints as `!c0`).
    pub name: Symbol,
    /// Raw bytes (`!c0: str = "hi"`; any byte, not necessarily UTF-8).
    pub bytes: Vec<u8>,
    /// Definition span.
    pub span: Span,
}

/// One f-string template: its literal pieces and, for each hole, the
/// function that formats it. Printed `!fmt0: format "a" {@f} "b"`, as the
/// f-string reads. In memory it is a [`format_template_layout`]: the hole
/// count, then one [`LirTy::format_part`] per piece (the piece's text and
/// the formatter of the hole after it, null after the last piece).
///
/// A formatter is an ordinary place-ABI function with ins `ref #T, ref #W`
/// and no outs, where `#T` is its hole's type and `#W` the one type every
/// formatter of the template writes into.
#[derive(Clone, Debug)]
pub struct LirFormat {
    /// Template name (`fmt0` prints as `!fmt0`).
    pub name: Symbol,
    /// The literal pieces, one more than the holes; piece `i` comes before
    /// hole `i`. Any may be empty.
    pub pieces: Vec<Vec<u8>>,
    /// Each hole's formatter.
    pub formats: Vec<Symbol>,
    /// The f-string's span.
    pub span: Span,
}

/// One global variable: mutable static storage of one type.
#[derive(Clone, Debug)]
pub struct LirGlobal {
    /// Global name (prints as `!name`).
    pub name: Symbol,
    /// Stored type.
    pub ty: LirTy,
    /// The initial bytes, in the target's layout (little-endian), as runs
    /// over an object that is otherwise zero: `(offset, bytes)` pairs in
    /// increasing offset order, none empty, overlapping or reaching past
    /// the type's size. Empty starts the global zeroed. A run costs only
    /// its bytes, so a large zero array in an initialized global is free.
    pub init: Vec<(u64, Vec<u8>)>,
    /// The addresses in the initial value, in increasing offset order:
    /// each a pointer-sized slot, aligned to a pointer, that no run or
    /// other address overlaps (`ref f` or `ref OTHER` in the initializer).
    pub addresses: Vec<LirAddress>,
    /// Declaration span.
    pub span: Span,
}

impl LirGlobal {
    /// The functions and globals the initial value points at.
    pub fn address_targets(&self) -> impl Iterator<Item = &AddressTarget> {
        self.addresses.iter().map(|address| &address.target)
    }
}

/// One address in a global's initial value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LirAddress {
    /// Byte offset of the pointer in the global.
    pub offset: u64,
    /// What it points at.
    pub target: AddressTarget,
}

/// What an address in a global's initial value points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddressTarget {
    /// A function of the module (`@f`; an `export`'s is its C entry).
    Func(Symbol),
    /// A global of the module (`!g`).
    Global(Symbol),
}

impl fmt::Display for AddressTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Func(name) => write!(f, "@{}", name.as_str()),
            Self::Global(name) => write!(f, "!{}", name.as_str()),
        }
    }
}

/// One function: signature places plus labeled blocks.
#[derive(Clone, Debug)]
pub struct LirFunc {
    /// Function name (`main` prints as `@main`).
    pub name: Symbol,
    /// `(place, type)` inputs (print as `in #t %p`).
    pub ins: Vec<(PlaceName, LirTy)>,
    /// `(place, type)` outputs (print as `out #t %p`).
    pub outs: Vec<(PlaceName, LirTy)>,
    /// Whether this is an `extern` declaration with no body (see
    /// [`Self::uses_c_abi`]).
    pub is_extern: bool,
    /// The C symbol an `export` defines, calling this function: a C-ABI
    /// entry the backend writes around it (see [`LirExport`]).
    pub export: Option<LirExport>,
    /// Whether an `extern` takes C varargs after its `ins` (`...`); a call
    /// may then pass extra `ins` of a [`is_variadic_arg_type`] type.
    pub variadic: bool,
    /// Whether backends must always inline it (`=>` functions).
    pub inline: bool,
    /// Blocks in layout order (empty only for extern declarations).
    pub blocks: Vec<LirBlock>,
}

impl LirFunc {
    /// Whether calls to this function use the platform C ABI: every
    /// `extern` except a `.lilac.`-named one, which declares a Lilac function
    /// defined elsewhere (a platform hook another object supplies) and keeps
    /// the place ABI.
    #[must_use]
    pub fn uses_c_abi(&self) -> bool {
        self.is_extern && !self.name.as_str().starts_with(".lilac.")
    }
}

/// The C entry of an `export`: the backend defines `symbol` with the C
/// signature `ins` -> `outs` (at most one), which stores each argument in
/// a stack slot, calls the function with the place ABI, and returns its
/// out place. Every type is an [`is_c_abi_type`]; a wrapper object's slot
/// has its field's type, which has the object's layout.
#[derive(Clone, Debug, PartialEq)]
pub struct LirExport {
    /// The unmangled C symbol.
    pub symbol: Symbol,
    /// The C parameter types, one per `in`.
    pub ins: Vec<LirTy>,
    /// The C result type, one per `out` (none or one).
    pub outs: Vec<LirTy>,
}

/// One labeled block: straight-line instructions plus one terminator.
#[derive(Clone, Debug)]
pub struct LirBlock {
    /// Block label (`entry` prints as `$entry:`).
    pub label: Label,
    /// Instructions in order.
    pub insts: Vec<LirInst>,
    /// Block terminator (exactly one).
    pub term: LirTerm,
    /// Block span.
    pub span: Span,
}

/// A value operand: register (`&1`) or place (`%x`).
#[derive(Clone, Debug)]
pub enum LirOperand {
    /// SSA value register.
    Reg(Reg),
    /// Mutable place.
    Place(PlaceName),
}

/// A call target: register (indirect), function (direct), or builtin.
#[derive(Clone, Debug)]
pub enum LirCallee {
    /// Indirect call through a register holding a function address.
    Reg(Reg),
    /// Indirect call through a register holding a C function's address
    /// (`export function` values), with the C ABI: each `in` place's type
    /// is its C parameter type, and the `out` place (at most one) holds
    /// the result. Prints `c &reg`.
    CReg(Reg),
    /// Direct call by function name.
    Func(Symbol),
    /// Direct call to a compiler builtin leaf (name keeps its `@` sigil,
    /// e.g. `@ptr_add`). The runtime's other operations are `.lilac.*` LIR
    /// functions, not builtins.
    Builtin(String),
}

/// Arithmetic/comparison operators (all explicitly typed at site).
#[derive(Clone, Debug)]
pub enum ArithOp {
    /// `i.add` (integer add).
    IAdd,
    /// `i.sub`.
    ISub,
    /// `i.mul`.
    IMul,
    /// `i.div`.
    IDiv,
    /// `i.div.u` (unsigned division).
    IDivU,
    /// `i.mod`.
    IMod,
    /// `i.mod.u` (unsigned remainder).
    IModU,
    /// `and` (bitwise).
    And,
    /// `or`.
    Or,
    /// `xor`.
    Xor,
    /// `shift.l`: shifts left by the right operand modulo the width.
    Shl,
    /// `shift.r`: arithmetic right shift (the amount modulo the width).
    Shr,
    /// `shift.r.u`: logical right shift (the amount modulo the width).
    ShrU,
    /// `f.add` (bitwise-float add).
    FAdd,
    /// `f.sub`.
    FSub,
    /// `f.mul`.
    FMul,
    /// `f.div`.
    FDiv,
    /// `f.mod`.
    FMod,
    /// `cmp.eql` (bit-equality).
    Eq,
    /// `i.cmp.lt`.
    ILt,
    /// `i.cmp.lt.u` (unsigned less-than).
    ILtU,
    /// `i.cmp.gt`.
    IGt,
    /// `i.cmp.gt.u` (unsigned greater-than).
    IGtU,
    /// `f.cmp.lt`.
    FLt,
    /// `f.cmp.gt`.
    FGt,
    /// `i.cmp.le`.
    ILe,
    /// `i.cmp.le.u`.
    ILeU,
    /// `i.cmp.ge`.
    IGe,
    /// `i.cmp.ge.u`.
    IGeU,
    /// `f.cmp.le` (false when either side is NaN).
    FLe,
    /// `f.cmp.ge` (false when either side is NaN).
    FGe,
}

impl ArithOp {
    /// Whether the result is a `#bit8` truth value.
    #[must_use]
    pub fn is_comparison(&self) -> bool {
        matches!(
            self,
            Self::Eq
                | Self::ILt
                | Self::ILtU
                | Self::IGt
                | Self::IGtU
                | Self::ILe
                | Self::ILeU
                | Self::IGe
                | Self::IGeU
                | Self::FLt
                | Self::FGt
                | Self::FLe
                | Self::FGe
        )
    }

    /// Whether this is a float operation (arithmetic or comparison).
    #[must_use]
    pub fn is_float(&self) -> bool {
        matches!(
            self,
            Self::FAdd
                | Self::FSub
                | Self::FMul
                | Self::FDiv
                | Self::FMod
                | Self::FLt
                | Self::FGt
                | Self::FLe
                | Self::FGe
        )
    }
}

/// One Lilac IR instruction (every variant carries its span).
#[derive(Clone, Debug)]
pub enum LirInst {
    /// `&d: #t = copy src` (register definition).
    CopyReg {
        /// Source operand.
        src: LirOperand,
        /// Destination register.
        dst: Reg,
        /// Value type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `copy src -> %d` (place-to-place or value-to-place copy).
    CopyPlace {
        /// Source operand.
        src: LirOperand,
        /// Destination place.
        dst: PlaceName,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = const v` (constant materialization).
    Const {
        /// Destination register.
        dst: Reg,
        /// The value.
        value: LirConst,
        /// Value type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `copy ref %s -> %d`: copies the value the reference place `%s`
    /// points to into the place `%d`. Registers hold only scalars, so this
    /// is how an aggregate is read through a reference.
    CopyDeref {
        /// Reference place holding the source address.
        src: PlaceName,
        /// Destination place.
        dst: PlaceName,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = load %s` (a scalar read through the reference in `%s`).
    Load {
        /// Source place.
        src: PlaceName,
        /// Destination register.
        dst: Reg,
        /// Value type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = addr %p` (place address).
    Addr {
        /// Place.
        place: PlaceName,
        /// Destination register.
        dst: Reg,
        /// Pointer type (`ref #t`).
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = addr.func @f` (function address).
    AddrFunc {
        /// Function name.
        func: Symbol,
        /// Destination register.
        dst: Reg,
        /// Pointer type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `&d: ref #t = addr.global !g` (a global variable's address).
    AddrGlobal {
        /// Global name.
        global: Symbol,
        /// Destination register.
        dst: Reg,
        /// Pointer type (`ref #t`, or `ref #unknown` for a `ref` global).
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `call &f (in %a) (out %b)` (arity carried at site).
    Call {
        /// Call target.
        func: LirCallee,
        /// Input places.
        ins: Vec<PlaceName>,
        /// Output places.
        outs: Vec<PlaceName>,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = field %o .name  // idx i` (name required).
    Field {
        /// Object place.
        obj: PlaceName,
        /// Field name.
        name: String,
        /// Field index (also rendered as a comment).
        idx: u32,
        /// Destination register.
        dst: Reg,
        /// Field type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `copy value -> %o.name  // idx i` (field store).
    StoreField {
        /// Value operand.
        value: LirOperand,
        /// Object place.
        obj: PlaceName,
        /// Field name.
        name: String,
        /// Field index (also rendered as a comment).
        idx: u32,
        /// Instruction span.
        span: Span,
    },
    /// `copy value -> ref %p` (store through a reference place).
    Store {
        /// Value operand.
        src: LirOperand,
        /// Reference place holding the address.
        dst: PlaceName,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = trunc|extend.zero|extend.sign &s` (width conversion).
    Convert {
        /// Conversion operator.
        op: ConvertOp,
        /// Source register.
        src: Reg,
        /// Destination register.
        dst: Reg,
        /// Result type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = field.addr %o .name  // idx i` (field address).
    FieldAddr {
        /// Object place.
        obj: PlaceName,
        /// Field name.
        name: String,
        /// Field index (also rendered as a comment).
        idx: u32,
        /// Destination register.
        dst: Reg,
        /// Pointer type (`ref #t`).
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `&d: ref #t = elem.addr &a, &i`: the address of element `&i` (a
    /// `#bit_native`) of the array that `&a` (a `ref [N x #t]`) points to.
    /// Lowering checks the bounds first.
    ElemAddr {
        /// Array address register.
        array: Reg,
        /// Index register.
        index: Reg,
        /// Destination register.
        dst: Reg,
        /// Pointer type (`ref #t`).
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `alloca #t %x : zeroed` (every place starts zeroed).
    Alloca {
        /// Place type.
        ty: LirTy,
        /// Place.
        place: PlaceName,
        /// Instruction span.
        span: Span,
    },
    /// `drop %x` marks a place destroyed. Where the type declares an
    /// `@on_drop`, its destructor call precedes this marker (the hook runs on
    /// live memory; `drop` records that the place is dead).
    Drop {
        /// Place.
        place: PlaceName,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = op &l, &r` (typed arithmetic).
    Arith {
        /// Operator.
        op: ArithOp,
        /// Left register.
        lhs: Reg,
        /// Right register.
        rhs: Reg,
        /// Destination register.
        dst: Reg,
        /// Result type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `@compile_error` site. Lowering never emits one in the dead arm of a
    /// constant condition; [`check_live_compile_errors`] reports every site
    /// that survives DCE as `E031`, so reaching `verify` is an internal error.
    CompileError {
        /// Error message.
        msg: String,
        /// Message literal span.
        msg_span: Span,
        /// Call-site span.
        call_span: Span,
    },
    /// `zero %x`: overwrites a place with all-zero bits (return slots start
    /// zeroed on every call).
    Zero {
        /// Place.
        place: PlaceName,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = atomic.load.<order> &p` (an atomic load through the
    /// reference in `&p`, aligned to its width).
    AtomicLoad {
        /// Address register (`ref #t`).
        ptr: Reg,
        /// Destination register.
        dst: Reg,
        /// Integer type.
        ty: LirTy,
        /// Memory ordering (not `release`/`acq_rel`).
        order: AtomicOrdering,
        /// Instruction span.
        span: Span,
    },
    /// `atomic.store.<order> #t &v -> &p`.
    AtomicStore {
        /// Value register.
        value: Reg,
        /// Address register (`ref #t`).
        ptr: Reg,
        /// Integer type.
        ty: LirTy,
        /// Memory ordering (not `acquire`/`acq_rel`).
        order: AtomicOrdering,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = atomic.<op>.<order> &p, &v`: a read-modify-write that
    /// yields the old value.
    AtomicRmw {
        /// Operation.
        op: RmwOp,
        /// Address register (`ref #t`).
        ptr: Reg,
        /// Operand register.
        value: Reg,
        /// Destination register: the old value.
        dst: Reg,
        /// Integer type.
        ty: LirTy,
        /// Memory ordering.
        order: AtomicOrdering,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = atomic.cmpxchg.<order> &p, &e, &n`: a strong compare and
    /// swap that yields the old value (it succeeded iff that equals `&e`).
    /// The failure ordering is the strongest one valid for `order`.
    CmpXchg {
        /// Address register (`ref #t`).
        ptr: Reg,
        /// Expected-value register.
        expected: Reg,
        /// Replacement-value register.
        desired: Reg,
        /// Destination register: the old value.
        dst: Reg,
        /// Integer type.
        ty: LirTy,
        /// Success ordering.
        order: AtomicOrdering,
        /// Instruction span.
        span: Span,
    },
    /// `fence.<order>` (not `relaxed`).
    Fence {
        /// Memory ordering.
        order: AtomicOrdering,
        /// Instruction span.
        span: Span,
    },
    /// `format !fmt0 (&1, &2) -> %d`: points the `#format_args`
    /// place `%d` at the template `!fmt0` and at the hole addresses, which
    /// it stores in an array that lives as long as the function's frame.
    /// Register `i` is a `ref` to hole `i`, of the type its formatter takes.
    Format {
        /// The template.
        template: Symbol,
        /// One address register per hole.
        holes: Vec<Reg>,
        /// Destination place.
        dst: PlaceName,
        /// Instruction span.
        span: Span,
    },
}

/// The value of a `const`.
#[derive(Clone, Debug, PartialEq)]
pub enum LirConst {
    /// An integer of an integer type, or `0` as the null reference; prints
    /// in hex (`0x2a`).
    Int(u64),
    /// A float's exact bit pattern, of its type's width. `#fbit32` and
    /// `#fbit64` print as the shortest decimal that reads back exactly
    /// (`5.25`), `#fbit16` and `#fbit128` as their bits (`0xH3C00`,
    /// `0xQ3FFF0000000000000000000000000000`).
    Float(FloatBits),
    /// A data constant's address, as a `ref #bit8` (`!c0`). A
    /// `#byte_buffer` over it is built in a place from the address and the
    /// length ([`runtime::fill_byte_buffer`]).
    Data(Symbol),
}

impl fmt::Display for LirConst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(value) => write!(f, "{value:#x}"),
            Self::Float(bits) => write!(f, "{bits}"),
            Self::Data(name) => write!(f, "!{}", name.as_str()),
        }
    }
}

/// The memory ordering of an atomic instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AtomicOrdering {
    /// `relaxed` (LLVM `monotonic`).
    Relaxed,
    /// `acquire`.
    Acquire,
    /// `release`.
    Release,
    /// `acq_rel`.
    AcqRel,
    /// `seq_cst`.
    SeqCst,
}

impl AtomicOrdering {
    /// Every ordering.
    pub const ALL: [Self; 5] = [
        Self::Relaxed,
        Self::Acquire,
        Self::Release,
        Self::AcqRel,
        Self::SeqCst,
    ];

    /// The printed name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Relaxed => "relaxed",
            Self::Acquire => "acquire",
            Self::Release => "release",
            Self::AcqRel => "acq_rel",
            Self::SeqCst => "seq_cst",
        }
    }

    /// The failure ordering of a compare-swap with this success ordering:
    /// the strongest one that does not release.
    #[must_use]
    pub fn failure(self) -> Self {
        match self {
            Self::Relaxed | Self::Release => Self::Relaxed,
            Self::Acquire | Self::AcqRel => Self::Acquire,
            Self::SeqCst => Self::SeqCst,
        }
    }
}

/// An `atomic.<op>` read-modify-write operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RmwOp {
    /// `swap`.
    Swap,
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
    /// `max.u`.
    MaxU,
    /// `min.u`.
    MinU,
}

impl RmwOp {
    /// Every operation.
    pub const ALL: [Self; 10] = [
        Self::Swap,
        Self::Add,
        Self::Sub,
        Self::And,
        Self::Or,
        Self::Xor,
        Self::Max,
        Self::Min,
        Self::MaxU,
        Self::MinU,
    ];

    /// The printed name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Swap => "swap",
            Self::Add => "add",
            Self::Sub => "sub",
            Self::And => "and",
            Self::Or => "or",
            Self::Xor => "xor",
            Self::Max => "max",
            Self::Min => "min",
            Self::MaxU => "max.u",
            Self::MinU => "min.u",
        }
    }
}

/// Width-conversion operators.
#[derive(Clone, Debug)]
pub enum ConvertOp {
    /// `trunc` (narrowing).
    Trunc,
    /// `extend.zero` (zero-extension).
    ExtendZero,
    /// `extend.sign` (sign-extension).
    ExtendSign,
    /// `bitcast` (same-width reinterpret: `@bit32` <-> `@fbit32`, or a
    /// reference <-> `#bit_native` address).
    Bitcast,
}

/// A block terminator (exactly one per block).
#[derive(Clone, Debug)]
pub enum LirTerm {
    /// `return`.
    Return {
        /// Terminator span.
        span: Span,
    },
    /// `branch $l`.
    Branch {
        /// Target label.
        target: Label,
        /// Terminator span.
        span: Span,
    },
    /// `branch.if &c, $t [, $e]`.
    BranchIf {
        /// Condition register.
        cond: Reg,
        /// Taken label.
        then: Label,
        /// Untaken label, if any.
        els: Option<Label>,
        /// Terminator span.
        span: Span,
    },
    /// `unreachable`.
    Unreachable {
        /// Terminator span.
        span: Span,
    },
}

impl LirInst {
    /// The instruction's source span (a `CompileError`'s call site).
    #[must_use]
    pub fn span(&self) -> Span {
        print::inst_span(self)
    }
}

impl LirTerm {
    /// The terminator's source span.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Return { span }
            | Self::Branch { span, .. }
            | Self::BranchIf { span, .. }
            | Self::Unreachable { span } => *span,
        }
    }
}

impl LirModule {
    /// Creates a module for `target` under the current IR version.
    #[must_use]
    pub fn new(target: Target) -> Self {
        Self {
            version: LILAC_IR_VERSION.to_owned(),
            target,
            data: Vec::new(),
            formats: Vec::new(),
            globals: Vec::new(),
            funcs: Vec::new(),
            layouts: HashMap::new(),
        }
    }
}

impl Default for LirModule {
    fn default() -> Self {
        Self::new(Target::default())
    }
}

/// Signature of a compiler builtin: input/output place types.
#[derive(Clone, Debug)]
pub struct BuiltinSig {
    /// Input place types.
    pub ins: Vec<LirTy>,
    /// Output place types.
    pub outs: Vec<LirTy>,
}

/// Looks up a builtin's signature by its `@`-prefixed name.
#[must_use]
pub fn builtin_sig(name: &str) -> Option<BuiltinSig> {
    let (ins, outs) = match name {
        "@memcpy" | "@memmove" => (
            vec![LirTy::byte_ref(), LirTy::byte_ref(), LirTy::BitNative],
            vec![],
        ),
        "@memset" => (
            vec![LirTy::byte_ref(), LirTy::Bit(8), LirTy::BitNative],
            vec![],
        ),
        // C's `memcmp`: zero exactly when the two ranges hold the same bytes.
        "@memcmp" => (
            vec![LirTy::byte_ref(), LirTy::byte_ref(), LirTy::BitNative],
            vec![LirTy::Bit(32)],
        ),
        "@ptr_add" => (
            vec![LirTy::byte_ref(), LirTy::BitNative],
            vec![LirTy::byte_ref()],
        ),
        "@lilac_abort" => (vec![], vec![]),
        "@linux_syscall" => (vec![LirTy::BitNative; 7], vec![LirTy::BitNative]),
        "@linux_thread_clone" => (
            vec![
                LirTy::BitNative,
                LirTy::opaque_ref(),
                LirTy::opaque_ref(),
                LirTy::opaque_ref(),
                LirTy::opaque_ref(),
                LirTy::BitNative,
                LirTy::opaque_ref(),
                LirTy::opaque_ref(),
            ],
            vec![LirTy::BitNative],
        ),
        "@trace_head" => (vec![], vec![LirTy::trace_frame().to_ref().to_ref()]),
        "@panic_depth" => (vec![], vec![LirTy::BitNative.to_ref()]),
        _ => return None,
    };
    Some(BuiltinSig { ins, outs })
}

/// Whether a LIR type is representable in the platform C ABI used by
/// `extern`: an integer or float by value, or any `ref T` (an opaque pointer).
///
/// This is the single source of truth for the `extern` accept-set, shared by
/// the `E028` frontend validation (`lilac-lower`) and the LLVM signature
/// lowering (`lilac-codegen-llvm`), so the two cannot drift.
#[must_use]
pub fn is_c_abi_type(ty: &LirTy) -> bool {
    !matches!(ty, LirTy::Named(_) | LirTy::Array(..))
}

/// Whether `ty` may be passed as a C variadic argument: C promotes narrower
/// integers and `float`, and Lilac integers carry no sign to promote with, so
/// only `#bit32`, `#bit64`, `#bit_native`, `#fbit64` and references pass.
/// Typeck's `E028` check and the verifier share this rule.
#[must_use]
pub fn is_variadic_arg_type(ty: &LirTy) -> bool {
    matches!(
        ty,
        LirTy::Bit(32 | 64) | LirTy::BitNative | LirTy::FBit(64) | LirTy::Ref(_)
    )
}

/// Fixed fields of the builtin aggregate types (`#byte_buffer`,
/// `#format_args`: an f-string, its template and its hole addresses,
/// `#.lilac.format_part`:
/// one part of a template, see [`format_template_layout`],
/// `#.lilac.format_line`: a one-piece template, see [`LirTy::format_line`],
/// and
/// `#.lilac.frame`: a shadow-stack record, see [`trace`]), laid out as C
/// structs like any other ([`builtin_layout`]).
/// These never appear in `module.layouts`, so the verifier and the backend
/// both consult this table.
#[must_use]
pub fn builtin_fields(ty: &LirTy) -> Option<Vec<(String, LirTy)>> {
    match ty.name()? {
        "byte_buffer" => Some(vec![
            ("len".to_owned(), LirTy::BitNative),
            ("data".to_owned(), LirTy::byte_ref()),
        ]),
        "format_args" => Some(vec![
            ("template".to_owned(), LirTy::opaque_ref()),
            ("holes".to_owned(), LirTy::opaque_ref()),
        ]),
        ".lilac.format_part" => Some(vec![
            ("text".to_owned(), LirTy::byte_buffer()),
            ("format".to_owned(), LirTy::opaque_ref()),
        ]),
        ".lilac.format_line" => Some(vec![
            ("holes".to_owned(), LirTy::BitNative),
            ("len".to_owned(), LirTy::BitNative),
            ("data".to_owned(), LirTy::byte_ref()),
            ("format".to_owned(), LirTy::opaque_ref()),
        ]),
        ".lilac.frame" => Some(vec![
            ("prev".to_owned(), LirTy::trace_frame().to_ref()),
            ("site".to_owned(), LirTy::byte_buffer()),
        ]),
        _ => None,
    }
}

/// The layout of a template with `holes` holes ([`LirFormat`]) on
/// `target`: the C struct `{ uintptr_t holes; struct format_part
/// parts[holes + 1]; }`, fields `holes` and `parts`. `None` when it is
/// larger than the target allows.
#[must_use]
pub fn format_template_layout(holes: usize, target: &Target) -> Option<LirLayout> {
    let parts = u64::try_from(holes).ok()?.checked_add(1)?;
    let no_layouts: HashMap<String, LirLayout> = HashMap::new();
    LirLayout::of_struct(
        vec![
            ("holes".to_owned(), LirTy::BitNative),
            (
                "parts".to_owned(),
                LirTy::Array(Box::new(LirTy::format_part()), parts),
            ),
        ],
        &no_layouts,
        target,
    )
    .ok()
}

/// The layout of a builtin aggregate on `target`: its [`builtin_fields`] as
/// a C struct.
#[must_use]
pub fn builtin_layout(ty: &LirTy, target: &Target) -> Option<LirLayout> {
    let no_layouts: HashMap<String, LirLayout> = HashMap::new();
    LirLayout::of_struct(builtin_fields(ty)?, &no_layouts, target).ok()
}

/// The fields of an aggregate: a module layout's, else a builtin one's.
#[must_use]
pub fn fields_of<S: std::hash::BuildHasher>(
    layouts: &HashMap<String, LirLayout, S>,
    ty: &LirTy,
) -> Option<Vec<(String, LirTy)>> {
    ty.name()
        .and_then(|name| layouts.get(name))
        .map(LirLayout::members)
        .or_else(|| builtin_fields(ty))
}

/// The layout of an aggregate on `target`: a module layout, else a builtin
/// one.
#[must_use]
pub fn layout_of<S: std::hash::BuildHasher>(
    layouts: &HashMap<String, LirLayout, S>,
    ty: &LirTy,
    target: &Target,
) -> Option<LirLayout> {
    ty.name()
        .and_then(|name| layouts.get(name))
        .cloned()
        .or_else(|| builtin_layout(ty, target))
}

/// The size and alignment in bytes of a LIR type on `target`, looked up,
/// never laid out anew: a scalar's from the target's C ABI table, a named
/// type's from its layout (a module layout, else a builtin one), an array's
/// as a C array of its element. `None` for a type with no size
/// (`#unknown`, a name with no layout) or an array larger than the target
/// allows.
#[must_use]
pub fn size_align<S: std::hash::BuildHasher>(
    layouts: &HashMap<String, LirLayout, S>,
    ty: &LirTy,
    target: &Target,
) -> Option<SizeAlign> {
    if let Some(scalar) = ty.scalar() {
        return Some(SizeAlign::scalar(target, scalar));
    }
    match ty {
        LirTy::Array(elem, len) => {
            let elem = size_align(layouts, elem, target)?;
            lilac_layout::array_layout(target, elem, *len).ok()
        }
        LirTy::Named(name) => match layouts.get(name) {
            Some(layout) => Some(SizeAlign {
                size: layout.size,
                align: layout.align,
            }),
            None => builtin_layout(ty, target).map(|layout| SizeAlign {
                size: layout.size,
                align: layout.align,
            }),
        },
        LirTy::Bit(_) | LirTy::BitNative | LirTy::FBit(_) | LirTy::Ref(_) => None,
    }
}

#[cfg(test)]
mod tests;
