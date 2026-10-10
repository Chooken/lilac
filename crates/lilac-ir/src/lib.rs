//! Lilac IR types, printer and verifier.
//!
//! This crate is the stability boundary: frontend crates build it, backend
//! crates consume it, and nothing else links LLVM.
//!
//! LIR types print exactly as they read (`#bit32`, `ref #bit8`). Names are
//! typed by what they name ([`Reg`], [`PlaceName`], [`Label`], [`Symbol`])
//! and constants are values ([`LirConst`]). Layouts
//! (object field orders) ride in the module for the verifier only; they
//! never print.

use std::collections::HashMap;
use std::fmt;

use lilac_span::Span;

mod dce;
mod names;
mod print;
pub mod runtime;
pub mod trace;
mod ty;
mod verify;

pub use dce::{prune_unreachable, reach, CallGraph, Edge, EdgeKind, Reach};
pub use names::{Label, PlaceName, Reg, Symbol};
pub use runtime::{build_runtime, entry_point, hook_externs, panic_runtime, Entry};
pub use ty::LirTy;
pub use verify::{check_live_compile_errors, check_live_syscalls, verify};

/// IR version header; bump on any normative IR change.
pub const LILAC_IR_VERSION: &str = "0.4";

/// An IR module: versioned header plus data, functions, and layouts.
#[derive(Clone, Debug)]
pub struct LirModule {
    /// `lilac_ir = "..."` header value.
    pub version: String,
    /// `target = "..."` header value (fixes `@bit_native` width).
    pub target: String,
    /// Typed constants (`!c0: str`, `!c1: numlit`).
    pub data: Vec<LirData>,
    /// Global variables (`global !name: #t = …`).
    pub globals: Vec<LirGlobal>,
    /// Functions in source order.
    pub funcs: Vec<LirFunc>,
    /// Layout name (without `#`) → `(field, type)` in order.
    pub layouts: HashMap<String, Vec<(String, LirTy)>>,
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

/// One global variable: mutable static storage of one type.
#[derive(Clone, Debug)]
pub struct LirGlobal {
    /// Global name (prints as `!name`).
    pub name: Symbol,
    /// Stored type.
    pub ty: LirTy,
    /// The initial bytes in the target's layout (little-endian), exactly
    /// the type's size; `None` starts zeroed.
    pub init: Option<Vec<u8>>,
    /// Declaration span.
    pub span: Span,
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
    /// `&d: #t = load %s` (deref-load through a place).
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
}

/// The value of a `const`.
#[derive(Clone, Debug, PartialEq)]
pub enum LirConst {
    /// An integer of an integer type, or `0` as the null reference; prints
    /// in hex (`0x2a`).
    Int(u64),
    /// A float of a float type; prints so it reads back exactly (`5.25`).
    Float(f64),
    /// A data constant's contents: a `#byte_buffer` of them, or their
    /// address as a `ref #bit8` (`!c0`).
    Data(Symbol),
}

impl fmt::Display for LirConst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(value) => write!(f, "{value:#x}"),
            Self::Float(value) => write!(f, "{value:?}"),
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
    pub fn new(target: &str) -> Self {
        Self {
            version: LILAC_IR_VERSION.to_owned(),
            target: target.to_owned(),
            data: Vec::new(),
            globals: Vec::new(),
            funcs: Vec::new(),
            layouts: HashMap::new(),
        }
    }
}

impl Default for LirModule {
    fn default() -> Self {
        Self::new("x86_64-unknown-none")
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
        "@ptr_add" => (
            vec![LirTy::byte_ref(), LirTy::BitNative],
            vec![LirTy::byte_ref()],
        ),
        "@lilac_abort" => (vec![], vec![]),
        "@linux_syscall" => (vec![LirTy::BitNative; 7], vec![LirTy::BitNative]),
        "@thread_entry" => (vec![], vec![LirTy::opaque_ref()]),
        "@linux_thread_clone" => (
            vec![
                LirTy::BitNative,
                LirTy::opaque_ref(),
                LirTy::opaque_ref(),
                LirTy::opaque_ref(),
                LirTy::opaque_ref(),
                LirTy::BitNative,
                LirTy::opaque_ref(),
            ],
            vec![LirTy::BitNative],
        ),
        "@trace_head" => (vec![], vec![LirTy::trace_frame().to_ref().to_ref()]),
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

/// Fixed field layouts of the builtin aggregate types (`#byte_buffer`,
/// `#format_args`: a count plus sixteen pieces, since LIR has no arrays, and
/// `#.lilac.frame`: a shadow-stack record, see [`trace`]).
/// These never appear in `module.layouts`, so the verifier and the backend
/// both consult this table.
#[must_use]
pub fn builtin_layout(ty: &LirTy) -> Option<Vec<(String, LirTy)>> {
    match ty.name()? {
        "byte_buffer" => Some(vec![
            ("len".to_owned(), LirTy::BitNative),
            ("data".to_owned(), LirTy::byte_ref()),
        ]),
        "format_args" => Some(
            std::iter::once(("count".to_owned(), LirTy::BitNative))
                .chain((0..16).map(|index| (format!("p{index}"), LirTy::byte_buffer())))
                .collect(),
        ),
        ".lilac.frame" => Some(vec![
            ("prev".to_owned(), LirTy::trace_frame().to_ref()),
            ("site".to_owned(), LirTy::byte_buffer()),
        ]),
        _ => None,
    }
}

/// The fields of an aggregate: a module layout, else a builtin one.
#[must_use]
pub fn fields_of<S: std::hash::BuildHasher>(
    layouts: &HashMap<String, Vec<(String, LirTy)>, S>,
    ty: &LirTy,
) -> Option<Vec<(String, LirTy)>> {
    ty.name()
        .and_then(|name| layouts.get(name).cloned())
        .or_else(|| builtin_layout(ty))
}

/// Size and alignment in bytes of an IR type, by C layout rules (fields in
/// order, each aligned; the struct padded to its largest alignment), which is
/// what LLVM's data layouts use for our targets. `ptr_bytes` is the target's
/// pointer width.
#[must_use]
pub fn size_align<S: std::hash::BuildHasher>(
    layouts: &HashMap<String, Vec<(String, LirTy)>, S>,
    ty: &LirTy,
    ptr_bytes: u64,
) -> (u64, u64) {
    let ptr_bits = u32::try_from(ptr_bytes * 8).unwrap_or(u32::MAX);
    if let Some(bits) = ty.bits(ptr_bits) {
        let size = u64::from(bits / 8);
        return (size, size);
    }
    if let LirTy::Array(elem, len) = ty {
        let (size, align) = size_align(layouts, elem, ptr_bytes);
        return (size * len, align);
    }
    let mut size = 0u64;
    let mut align = 1u64;
    for (_, field) in &fields_of(layouts, ty).unwrap_or_default() {
        let (field_size, field_align) = size_align(layouts, field, ptr_bytes);
        size = size.next_multiple_of(field_align) + field_size;
        align = align.max(field_align);
    }
    (size.next_multiple_of(align), align)
}

#[cfg(test)]
mod tests;
