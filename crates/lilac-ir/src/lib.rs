//! Lilac IR types, printer and verifier.
//!
//! This crate is the stability boundary: frontend crates build it, backend
//! crates consume it, and nothing else links LLVM.
//!
//! LIR is string-typed on purpose: types print exactly as they read
//! (`#bit32`, `ref #bit8`), and the verifier string-compares them. Layouts
//! (object field orders) ride in the module for the verifier only; they
//! never print.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_span::{SourceMap, Span};

mod codec;
mod dce;
pub mod runtime;
pub mod trace;
mod ty;

pub use codec::{CodecError, LIR_CODEC_VERSION};
pub use dce::{prune_unreachable, reach, Edge, EdgeKind, Frame, Reach};
pub use runtime::{build_runtime, entry_point, panic_runtime, Entry, Syscalls, System};
pub use ty::LirTy;

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
    /// Constant name without sigil (`c0` prints as `!c0`).
    pub name: String,
    /// Raw bytes (`!c0: str = "hi"`; any byte, not necessarily UTF-8).
    pub bytes: Vec<u8>,
    /// Definition span.
    pub span: Span,
}

/// One function: signature places plus labeled blocks.
#[derive(Clone, Debug)]
pub struct LirFunc {
    /// Function name without sigil (`main` prints as `@main`).
    pub name: String,
    /// `(place, type)` inputs (print as `in #t %p`).
    pub ins: Vec<(String, LirTy)>,
    /// `(place, type)` outputs (print as `out #t %p`).
    pub outs: Vec<(String, LirTy)>,
    /// Whether this is an `extern` (foreign) declaration with no body.
    pub is_extern: bool,
    /// Whether backends must always inline it (`=>` functions).
    pub inline: bool,
    /// Blocks in layout order (empty only for extern declarations).
    pub blocks: Vec<LirBlock>,
}

/// One labeled block: straight-line instructions plus one terminator.
#[derive(Clone, Debug)]
pub struct LirBlock {
    /// Block label without sigil (`entry` prints as `$entry:`).
    pub label: String,
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
    /// SSA value register (name without sigil).
    Reg(String),
    /// Mutable place (name without sigil).
    Place(String),
}

/// A call target: register (indirect), function (direct), or builtin.
#[derive(Clone, Debug)]
pub enum LirCallee {
    /// Indirect call through a register holding a function address.
    Reg(String),
    /// Direct call by function name (without sigil).
    Func(String),
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
        /// Destination register (without sigil).
        dst: String,
        /// Value type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `copy src -> %d` (place-to-place or value-to-place copy).
    CopyPlace {
        /// Source operand.
        src: LirOperand,
        /// Destination place (without sigil).
        dst: String,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = const lit` (constant materialization).
    Const {
        /// Destination register (without sigil).
        dst: String,
        /// Literal text.
        lit: String,
        /// Value type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = load %s` (deref-load through a place).
    Load {
        /// Source place (without sigil).
        src: String,
        /// Destination register (without sigil).
        dst: String,
        /// Value type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = addr %p` (place address).
    Addr {
        /// Place (without sigil).
        place: String,
        /// Destination register (without sigil).
        dst: String,
        /// Pointer type (`ref #t`).
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = addr.func @f` (function address).
    AddrFunc {
        /// Function name (without sigil).
        func: String,
        /// Destination register (without sigil).
        dst: String,
        /// Pointer type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `call &f (in %a) (out %b)` (arity carried at site).
    Call {
        /// Call target.
        func: LirCallee,
        /// Input places (without sigils).
        ins: Vec<String>,
        /// Output places (without sigils).
        outs: Vec<String>,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = field %o .name  // idx i` (name required).
    Field {
        /// Object place (without sigil).
        obj: String,
        /// Field name.
        name: String,
        /// Field index (also rendered as a comment).
        idx: u32,
        /// Destination register (without sigil).
        dst: String,
        /// Field type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `copy value -> %o.name  // idx i` (field store).
    StoreField {
        /// Value operand.
        value: LirOperand,
        /// Object place (without sigil).
        obj: String,
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
        /// Reference place holding the address (without sigil).
        dst: String,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = trunc|extend.zero &s` (width conversion).
    Convert {
        /// Conversion operator.
        op: ConvertOp,
        /// Source register (without sigil).
        src: String,
        /// Destination register (without sigil).
        dst: String,
        /// Result type.
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = field.addr %o .name  // idx i` (field address).
    FieldAddr {
        /// Object place (without sigil).
        obj: String,
        /// Field name.
        name: String,
        /// Field index (also rendered as a comment).
        idx: u32,
        /// Destination register (without sigil).
        dst: String,
        /// Pointer type (`ref #t`).
        ty: LirTy,
        /// Instruction span.
        span: Span,
    },
    /// `alloca #t %x : zeroed` (every place starts zeroed).
    Alloca {
        /// Place type.
        ty: LirTy,
        /// Place (without sigil).
        place: String,
        /// Instruction span.
        span: Span,
    },
    /// `drop %x` marks a place destroyed. Where the type declares an
    /// `@on_drop`, its destructor call precedes this marker (the hook runs on
    /// live memory; `drop` records that the place is dead).
    Drop {
        /// Place (without sigil).
        place: String,
        /// Instruction span.
        span: Span,
    },
    /// `&d: #t = op &l, &r` (typed arithmetic).
    Arith {
        /// Operator.
        op: ArithOp,
        /// Left register (without sigil).
        lhs: String,
        /// Right register (without sigil).
        rhs: String,
        /// Destination register (without sigil).
        dst: String,
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
        /// Place (without sigil).
        place: String,
        /// Instruction span.
        span: Span,
    },
}

/// Width-conversion operators.
#[derive(Clone, Debug)]
pub enum ConvertOp {
    /// `trunc` (narrowing).
    Trunc,
    /// `extend.zero` (zero-extension).
    ExtendZero,
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
        /// Target label (without sigil).
        target: String,
        /// Terminator span.
        span: Span,
    },
    /// `branch.if &c, $t [, $e]`.
    BranchIf {
        /// Condition register (without sigil).
        cond: String,
        /// Taken label (without sigil).
        then: String,
        /// Untaken label, if any (without sigil).
        els: Option<String>,
        /// Terminator span.
        span: Span,
    },
    /// `unreachable`.
    Unreachable {
        /// Terminator span.
        span: Span,
    },
}

impl LirModule {
    /// Creates a module for `target` under the current IR version.
    #[must_use]
    pub fn new(target: &str) -> Self {
        Self {
            version: LILAC_IR_VERSION.to_owned(),
            target: target.to_owned(),
            data: Vec::new(),
            funcs: Vec::new(),
            layouts: HashMap::new(),
        }
    }

    /// Canonical header text: the two header lines, neither source-spanned.
    #[must_use]
    pub fn header_text(&self) -> String {
        join_lines(&self.header_lines())
    }

    /// The two compiler-generated header lines (`lilac_ir`, `target`).
    fn header_lines(&self) -> Vec<(String, Option<Span>)> {
        vec![
            (format!("lilac_ir = \"{}\"", self.version), None),
            (format!("target = \"{}\"", self.target), None),
        ]
    }

    /// Every physical line of the module text, paired with its source span
    /// (`None` for compiler-generated lines). The single traversal shared by
    /// [`Self::to_text`] and [`Self::to_text_spanned`].
    fn module_lines(&self) -> Vec<(String, Option<Span>)> {
        let mut lines = self.header_lines();
        for data in &self.data {
            lines.extend(print_data(data));
        }
        let mut names: Vec<&str> = self.layouts.keys().map(String::as_str).collect();
        names.sort_unstable();
        for name in names {
            lines.extend(print_layout(name, &self.layouts[name]));
        }
        for func in &self.funcs {
            lines.extend(print_func(func));
        }
        physical_lines(lines)
    }

    /// Canonical module text: header, data consts, type sections (sorted
    /// by name for deterministic snapshots), then functions.
    #[must_use]
    pub fn to_text(&self) -> String {
        join_lines(&self.module_lines())
    }

    /// Canonical module text plus the source span of each physical line.
    #[must_use]
    pub fn to_text_spanned(&self) -> SpannedText {
        let lines = self.module_lines();
        SpannedText {
            text: join_lines(&lines),
            lines: lines.into_iter().map(|(_, span)| span).collect(),
        }
    }
}

/// Splits each logical line on `\n`, yielding one entry per physical line and
/// giving every part the logical line's span. Rejoining with `"\n"` reproduces
/// the logical line, so `join_lines` stays byte-identical even when a `data`
/// string literal contains a raw newline while `lines` stays one-per-physical
/// line (matching [`str::lines`]).
fn physical_lines(lines: Vec<(String, Option<Span>)>) -> Vec<(String, Option<Span>)> {
    let mut out = Vec::with_capacity(lines.len());
    for (text, span) in lines {
        for part in text.split('\n') {
            out.push((part.to_owned(), span));
        }
    }
    out
}

/// Joins `(line, span)` pairs with `"\n"` and appends a trailing newline.
fn join_lines(lines: &[(String, Option<Span>)]) -> String {
    let mut text = lines
        .iter()
        .map(|(line, _)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text
}

impl Default for LirModule {
    fn default() -> Self {
        Self::new("x86_64-unknown-none")
    }
}

/// Escapes data bytes for the text form: `"` and `\` are backslashed, other
/// printable ASCII and newlines print as-is, and every other byte is `\xHH`.
fn escape_data(bytes: &[u8]) -> String {
    let mut escaped = String::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b'"' | b'\\' => {
                escaped.push('\\');
                escaped.push(char::from(byte));
            }
            b'\n' | b' '..=b'~' => escaped.push(char::from(byte)),
            _ => {
                let _ = write!(escaped, "\\x{byte:02X}");
            }
        }
    }
    escaped
}

fn print_data(data: &LirData) -> Vec<(String, Option<Span>)> {
    vec![(
        format!("!{}: str = \"{}\"", data.name, escape_data(&data.bytes)),
        Some(data.span),
    )]
}

/// Prints one type section (`type #Name` with `name: #ty` members in
/// layout order; the section order itself is sorted in `to_text`). Type
/// sections are compiler-generated, so every line carries no span.
fn print_layout(name: &str, members: &[(String, LirTy)]) -> Vec<(String, Option<Span>)> {
    let mut lines = vec![(format!("type #{name} {{"), None)];
    for (member, ty) in members {
        lines.push((format!("    {member}: {ty},"), None));
    }
    lines.push(("}".to_owned(), None));
    lines
}

fn print_operand(operand: &LirOperand) -> String {
    match operand {
        LirOperand::Reg(reg) => format!("&{reg}"),
        LirOperand::Place(place) => format!("%{place}"),
    }
}

fn print_callee(callee: &LirCallee) -> String {
    match callee {
        LirCallee::Reg(reg) => format!("&{reg}"),
        LirCallee::Func(func) => format!("@{func}"),
        LirCallee::Builtin(name) => name.clone(),
    }
}

fn print_op(op: &ArithOp) -> &'static str {
    match op {
        ArithOp::IAdd => "i.add",
        ArithOp::ISub => "i.sub",
        ArithOp::IMul => "i.mul",
        ArithOp::IDiv => "i.div",
        ArithOp::IDivU => "i.div.u",
        ArithOp::IMod => "i.mod",
        ArithOp::IModU => "i.mod.u",
        ArithOp::FAdd => "f.add",
        ArithOp::FSub => "f.sub",
        ArithOp::FMul => "f.mul",
        ArithOp::FDiv => "f.div",
        ArithOp::FMod => "f.mod",
        ArithOp::Eq => "cmp.eql",
        ArithOp::ILt => "i.cmp.lt",
        ArithOp::ILtU => "i.cmp.lt.u",
        ArithOp::IGt => "i.cmp.gt",
        ArithOp::IGtU => "i.cmp.gt.u",
        ArithOp::FLt => "f.cmp.lt",
        ArithOp::FGt => "f.cmp.gt",
        ArithOp::ILe => "i.cmp.le",
        ArithOp::ILeU => "i.cmp.le.u",
        ArithOp::IGe => "i.cmp.ge",
        ArithOp::IGeU => "i.cmp.ge.u",
        ArithOp::FLe => "f.cmp.le",
        ArithOp::FGe => "f.cmp.ge",
    }
}

fn print_inst(inst: &LirInst) -> String {
    match inst {
        LirInst::CopyReg { src, dst, ty, .. } => {
            format!("&{dst}: {ty} = copy {}", print_operand(src))
        }
        LirInst::CopyPlace { src, dst, .. } => {
            format!("copy {} -> %{dst}", print_operand(src))
        }
        LirInst::Const { dst, lit, ty, .. } => format!("&{dst}: {ty} = const {lit}"),
        LirInst::Load { src, dst, ty, .. } => format!("&{dst}: {ty} = load %{src}"),
        LirInst::Addr { place, dst, ty, .. } => {
            format!("&{dst}: {ty} = addr %{place}")
        }
        LirInst::AddrFunc { func, dst, ty, .. } => {
            format!("&{dst}: {ty} = addr.func @{func}")
        }
        LirInst::Call {
            func, ins, outs, ..
        } => {
            let ins = if ins.is_empty() {
                "(in)".to_owned()
            } else {
                let places = ins
                    .iter()
                    .map(|place| format!("%{place}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("(in {places})")
            };
            let outs = if outs.is_empty() {
                "(out)".to_owned()
            } else {
                let places = outs
                    .iter()
                    .map(|place| format!("%{place}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("(out {places})")
            };
            format!("call {} {ins} {outs}", print_callee(func))
        }
        LirInst::Field {
            obj,
            name,
            idx,
            dst,
            ty,
            ..
        } => format!("&{dst}: {ty} = field %{obj} .{name}  // idx {idx}"),
        LirInst::StoreField {
            value,
            obj,
            name,
            idx,
            ..
        } => format!(
            "copy {} -> %{obj}.{name}  // idx {idx}",
            print_operand(value)
        ),
        LirInst::Store { src, dst, .. } => {
            format!("copy {} -> ref %{dst}", print_operand(src))
        }
        LirInst::Convert {
            op, src, dst, ty, ..
        } => {
            let op = match op {
                ConvertOp::Trunc => "trunc",
                ConvertOp::ExtendZero => "extend.zero",
                ConvertOp::Bitcast => "bitcast",
            };
            format!("&{dst}: {ty} = {op} &{src}")
        }
        LirInst::FieldAddr {
            obj,
            name,
            idx,
            dst,
            ty,
            ..
        } => format!("&{dst}: {ty} = field.addr %{obj} .{name}  // idx {idx}"),
        LirInst::Alloca { ty, place, .. } => format!("alloca {ty} %{place} : zeroed"),
        LirInst::Drop { place, .. } => format!("drop %{place}"),
        LirInst::Arith {
            op,
            lhs,
            rhs,
            dst,
            ty,
            ..
        } => format!("&{dst}: {ty} = {} &{lhs}, &{rhs}", print_op(op)),
        LirInst::CompileError { .. } => "@compile_error(live)".to_owned(),
        LirInst::Zero { place, .. } => format!("zero %{place}"),
    }
}

fn print_term(term: &LirTerm) -> String {
    match term {
        LirTerm::Return { .. } => "return".to_owned(),
        LirTerm::Branch { target, .. } => format!("branch ${target}"),
        LirTerm::BranchIf {
            cond, then, els, ..
        } => match els {
            Some(els) => format!("branch.if &{cond}, ${then}, ${els}"),
            None => format!("branch.if &{cond}, ${then}"),
        },
        LirTerm::Unreachable { .. } => "unreachable".to_owned(),
    }
}

/// The source span of an instruction: its `span` field, except
/// `CompileError`, which only records the `@compile_error` call site.
fn inst_span(inst: &LirInst) -> Span {
    match inst {
        LirInst::CopyReg { span, .. }
        | LirInst::CopyPlace { span, .. }
        | LirInst::Const { span, .. }
        | LirInst::Load { span, .. }
        | LirInst::Addr { span, .. }
        | LirInst::AddrFunc { span, .. }
        | LirInst::Call { span, .. }
        | LirInst::Field { span, .. }
        | LirInst::StoreField { span, .. }
        | LirInst::Store { span, .. }
        | LirInst::Convert { span, .. }
        | LirInst::FieldAddr { span, .. }
        | LirInst::Alloca { span, .. }
        | LirInst::Drop { span, .. }
        | LirInst::Arith { span, .. }
        | LirInst::Zero { span, .. } => *span,
        LirInst::CompileError { call_span, .. } => *call_span,
    }
}

/// The source span of a terminator (every variant carries one).
fn term_span(term: &LirTerm) -> Span {
    match term {
        LirTerm::Return { span, .. }
        | LirTerm::Branch { span, .. }
        | LirTerm::BranchIf { span, .. }
        | LirTerm::Unreachable { span, .. } => *span,
    }
}

fn print_func(func: &LirFunc) -> Vec<(String, Option<Span>)> {
    let header = if func.is_extern {
        format!("extern @{}", func.name)
    } else if func.inline {
        format!("inline function @{}", func.name)
    } else {
        format!("function @{}", func.name)
    };
    let mut lines = vec![(header, None)];
    for (place, ty) in &func.ins {
        lines.push((format!("  in {ty} %{place}"), None));
    }
    for (place, ty) in &func.outs {
        lines.push((format!("  out {ty} %{place}"), None));
    }
    if func.is_extern {
        return lines;
    }
    lines.push(("{".to_owned(), None));
    for block in &func.blocks {
        lines.push((format!("  ${}:", block.label), Some(block.span)));
        for inst in &block.insts {
            lines.push((format!("    {}", print_inst(inst)), Some(inst_span(inst))));
        }
        lines.push((
            format!("    {}", print_term(&block.term)),
            Some(term_span(&block.term)),
        ));
    }
    lines.push(("}".to_owned(), None));
    lines
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
        "@memcpy" => (
            vec![LirTy::byte_ref(), LirTy::byte_ref(), LirTy::BitNative],
            vec![],
        ),
        "@ptr_add" => (
            vec![LirTy::byte_ref(), LirTy::BitNative],
            vec![LirTy::byte_ref()],
        ),
        "@lilac_abort" => (vec![], vec![]),
        "@syscall" => (vec![LirTy::BitNative; 7], vec![LirTy::BitNative]),
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
    !matches!(ty, LirTy::Named(_))
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
    let mut size = 0u64;
    let mut align = 1u64;
    for (_, field) in &fields_of(layouts, ty).unwrap_or_default() {
        let (field_size, field_align) = size_align(layouts, field, ptr_bytes);
        size = size.next_multiple_of(field_align) + field_size;
        align = align.max(field_align);
    }
    (size.next_multiple_of(align), align)
}

/// Reports every `@compile_error` site in `module` as `E031`. Run after DCE:
/// a site is an error exactly when its function is reachable. The diagnostic
/// labels both the message literal and the call, and, given the pruning
/// [`Reach`], notes the shortest call chain from a root in stack-trace form
/// ([`trace::site_text`], `.lilac.*` frames hidden, names from `names`).
///
/// # Errors
///
/// Returns [`ErrorGuaranteed`] when at least one site was reported.
pub fn check_live_compile_errors<S: std::hash::BuildHasher>(
    module: &LirModule,
    reach: Option<&Reach>,
    sources: &SourceMap,
    names: &HashMap<String, String, S>,
    diags: &mut Diags,
) -> Result<(), ErrorGuaranteed> {
    let mut failed = false;
    for func in &module.funcs {
        for inst in func.blocks.iter().flat_map(|block| &block.insts) {
            if let LirInst::CompileError {
                msg,
                msg_span,
                call_span,
            } = inst
            {
                failed = true;
                let mut diagnostic = Diagnostic::error("E031", msg.clone())
                    .label(*msg_span, true, "message here".to_owned())
                    .label(*call_span, false, "evaluated here".to_owned());
                if let Some(reach) = reach {
                    let chain = reach_chain_text(reach, &func.name, *call_span, sources, names);
                    if !chain.is_empty() {
                        diagnostic = diagnostic.note(format!(
                            "reached from:
{chain}"
                        ));
                    }
                }
                diags.emit(diagnostic);
            }
        }
    }
    if failed {
        Err(ErrorGuaranteed::new())
    } else {
        Ok(())
    }
}

/// The `at` lines from `func` (located at `site`) up to its root, one per
/// line without a trailing newline. A frame reached by `addr.func` is marked
/// `[address taken]`.
fn reach_chain_text<S: std::hash::BuildHasher>(
    reach: &Reach,
    func: &str,
    site: Span,
    sources: &SourceMap,
    names: &HashMap<String, String, S>,
) -> String {
    let mut lines = Vec::new();
    for frame in reach.chain(func) {
        if trace::is_hidden(&frame.func) {
            continue;
        }
        let (span, kind) = frame.edge.unwrap_or((site, EdgeKind::Call));
        let name = names.get(&frame.func).unwrap_or(&frame.func);
        let location = sources.location(span);
        let mut line = trace::site_text(name, location.as_deref())
            .trim_end()
            .to_owned();
        if kind == EdgeKind::Address {
            line.push_str(" [address taken]");
        }
        lines.push(line);
    }
    lines.join(
        "
",
    )
}

/// Verifies a module: every rule failure is a spanned `E070` diagnostic.
/// A `CompileError` node reaching here means the live `@compile_error` check
/// never ran — an internal error (`bug!`), never a user diagnostic.
pub fn verify(module: &LirModule, diags: &mut Diags) -> Result<(), ErrorGuaranteed> {
    let mut verifier = Verifier {
        module,
        diags,
        failed: false,
    };
    verifier.run();
    if verifier.failed {
        Err(ErrorGuaranteed::new())
    } else {
        Ok(())
    }
}

struct Verifier<'a> {
    module: &'a LirModule,
    diags: &'a mut Diags,
    failed: bool,
}

/// One register definition: whether it holds a constant (constant branches
/// are an error), and
/// its declared type (so operand widths can be checked).
#[derive(Clone)]
struct RegDef<'a> {
    is_const: bool,
    ty: &'a LirTy,
}

/// Declared places of the function being verified, with their types.
type Places<'a> = HashMap<&'a str, &'a LirTy>;
/// Defined registers of the block being verified.
type Regs<'a> = HashMap<&'a str, RegDef<'a>>;

impl Verifier<'_> {
    fn error(&mut self, msg: String, span: Span) {
        self.failed = true;
        self.diags.emit(Diagnostic::error("E070", msg).label(
            span,
            true,
            "invalid IR here".to_owned(),
        ));
    }

    fn run(&mut self) {
        if self.module.version != LILAC_IR_VERSION {
            self.error(
                format!(
                    "unsupported IR version `{}` (want `{LILAC_IR_VERSION}`)",
                    self.module.version
                ),
                Span::dummy(),
            );
            return;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for func in &self.module.funcs {
            if !seen.insert(func.name.as_str()) {
                self.error(
                    format!("duplicate function `@{}`", func.name),
                    Span::dummy(),
                );
            }
            self.verify_func(func);
        }
    }

    /// Whether a type is known: a scalar, a pointer to a known type or to
    /// `#unknown` (the opaque `void*`; bare `#unknown` has no size), or a
    /// builtin or module layout.
    fn valid_type(&self, ty: &LirTy) -> bool {
        match ty {
            LirTy::Bit(_) | LirTy::BitNative | LirTy::FBit(_) => true,
            LirTy::Ref(inner) => inner.name() == Some("unknown") || self.valid_type(inner),
            LirTy::Named(name) => {
                builtin_layout(ty).is_some() || self.module.layouts.contains_key(name)
            }
        }
    }

    fn verify_func<'a>(&mut self, func: &'a LirFunc) {
        if func.is_extern {
            if !func.blocks.is_empty() {
                self.error(
                    format!("extern `@{}` must have no body", func.name),
                    Span::dummy(),
                );
            }
            let mut places: Places<'a> = HashMap::new();
            self.verify_signature(func, &mut places);
            return;
        }
        if func.blocks.is_empty() {
            self.error(
                format!("function `@{}` has no blocks", func.name),
                Span::dummy(),
            );
            return;
        }
        let mut places: Places<'a> = HashMap::new();
        self.verify_signature(func, &mut places);
        let mut assigned: HashSet<&str> = HashSet::new();
        let last = func.blocks.len().saturating_sub(1);
        for (index, block) in func.blocks.iter().enumerate() {
            let has_fallthrough = index < last;
            self.verify_block(func, block, &mut places, &mut assigned, has_fallthrough);
        }
        for (place, _) in &func.outs {
            if !assigned.contains(place.as_str()) {
                self.error(
                    format!("out place `%{place}` of `@{}` is never assigned", func.name),
                    Span::dummy(),
                );
            }
        }
    }

    /// Validates the signature places shared by functions and externs: no
    /// duplicate place names and every `in`/`out` type known.
    fn verify_signature<'a>(&mut self, func: &'a LirFunc, places: &mut Places<'a>) {
        for (place, ty) in func.ins.iter().chain(func.outs.iter()) {
            if places.insert(place.as_str(), ty).is_some() {
                self.error(
                    format!("duplicate place `%{place}` in `@{}`", func.name),
                    Span::dummy(),
                );
            }
            if !self.valid_type(ty) {
                self.error(format!("unknown type `{ty}`"), Span::dummy());
            }
        }
    }

    fn verify_block<'a>(
        &mut self,
        func: &'a LirFunc,
        block: &'a LirBlock,
        places: &mut Places<'a>,
        assigned: &mut HashSet<&'a str>,
        has_fallthrough: bool,
    ) {
        let mut regs: Regs = HashMap::new();
        for inst in &block.insts {
            self.verify_inst(func, inst, places, &mut regs, assigned);
        }
        self.verify_term(func, &block.term, &regs, has_fallthrough);
    }

    #[allow(clippy::too_many_lines)]
    fn verify_inst<'a>(
        &mut self,
        func: &LirFunc,
        inst: &'a LirInst,
        places: &mut Places<'a>,
        regs: &mut Regs<'a>,
        assigned: &mut HashSet<&'a str>,
    ) {
        match inst {
            LirInst::CopyReg { src, dst, ty, span } => {
                self.check_operand(src, regs, places, *span, func);
                self.check_type(ty, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::CopyPlace { src, dst, span } => {
                self.check_operand(src, regs, places, *span, func);
                self.check_copy_place(src, dst, places, regs, *span, func);
                assigned.insert(dst.as_str());
            }
            LirInst::Const { dst, lit, ty, span } => {
                self.check_const_lit(lit, ty, *span);
                self.check_type(ty, *span);
                self.define_reg(regs, dst, ty, true, *span, func);
            }
            LirInst::Load { .. } | LirInst::Addr { .. } => {
                self.verify_mem_op(func, inst, places, regs);
            }
            LirInst::AddrFunc {
                func: target,
                dst,
                ty,
                span,
            } => {
                self.check_addr_func(target, ty, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::Call {
                func: callee,
                ins,
                outs,
                span,
            } => {
                self.check_call_target(callee, ins, outs, places, *span, regs);
                self.track_call_places(places, assigned, ins, outs, *span);
            }
            LirInst::Field {
                obj,
                name,
                idx,
                dst,
                ty,
                span,
            } => {
                match places.get(obj.as_str()).copied() {
                    Some(owner_ty) => self.check_field_layout(owner_ty, name, *idx, ty, *span),
                    None => self.error(format!("field of undeclared place `%{obj}`"), *span),
                }
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::Alloca { ty, place, span } => {
                self.check_type(ty, *span);
                if places.insert(place.as_str(), ty).is_some() {
                    self.error(
                        format!("duplicate place `%{place}` in `@{}`", func.name),
                        *span,
                    );
                }
            }
            LirInst::Drop { place, span } => {
                if !places.contains_key(place.as_str()) {
                    self.error(
                        format!("drop of undeclared place `%{place}` in `@{}`", func.name),
                        *span,
                    );
                }
            }
            LirInst::Arith {
                op,
                lhs,
                rhs,
                dst,
                ty,
                span,
            } => {
                for reg in [lhs, rhs] {
                    if !regs.contains_key(reg.as_str()) {
                        self.error(format!("use of undefined register `&{reg}`"), *span);
                    }
                }
                self.check_arith_width(op, ty, lhs, rhs, regs, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::CompileError { call_span, .. } => {
                lilac_error::bug!(*call_span, "`@compile_error` reached verify");
            }
            LirInst::Zero { place, span } => {
                if !places.contains_key(place.as_str()) {
                    self.error(
                        format!("zero of undeclared place `%{place}` in `@{}`", func.name),
                        *span,
                    );
                }
                assigned.insert(place.as_str());
            }
            LirInst::StoreField { .. }
            | LirInst::Store { .. }
            | LirInst::Convert { .. }
            | LirInst::FieldAddr { .. } => {
                self.verify_store_ops(func, inst, places, regs);
            }
        }
    }

    /// Checks a place copy: the destination must be declared and the source
    /// operand's type must be compatible. Two reference places are compatible
    /// under opaque pointers (`ref X` -> `ref Y`), but a `ref`-to-value copy or
    /// two distinct value types are rejected, so an adapter such as a
    /// dereference cannot slip through as a raw pointer copy.
    fn check_copy_place(
        &mut self,
        src: &LirOperand,
        dst: &str,
        places: &Places,
        regs: &Regs,
        span: Span,
        func: &LirFunc,
    ) {
        let Some(dst_ty) = places.get(dst).copied() else {
            self.error(
                format!("copy to undeclared place `%{dst}` in `@{}`", func.name),
                span,
            );
            return;
        };
        let src_ty = match src {
            LirOperand::Reg(reg) => regs.get(reg.as_str()).map(|def| def.ty),
            LirOperand::Place(place) => places.get(place.as_str()).copied(),
        };
        // Under opaque pointers any two reference places are layout-compatible,
        // so `ref X` -> `ref Y` is a pointer copy. A reference/value mix or two
        // distinct value types is still ill-typed.
        if let Some(src_ty) = src_ty.filter(|src_ty| {
            src_ty.is_ref() != dst_ty.is_ref() || (!src_ty.is_ref() && *src_ty != dst_ty)
        }) {
            self.error(
                format!(
                    "copy type mismatch in `@{}`: `{src_ty}` -> `{dst_ty}`",
                    func.name
                ),
                span,
            );
        }
    }

    /// Checks a `const` literal: a `!`-prefixed literal is a data-symbol
    /// reference and must name declared data under an allowed type.
    fn check_const_lit(&mut self, lit: &str, ty: &LirTy, span: Span) {
        let Some(sym) = lit.strip_prefix('!') else {
            return;
        };
        if !self.module.data.iter().any(|datum| datum.name == sym) {
            self.error(format!("const of undeclared data `!{sym}`"), span);
        }
        if *ty != LirTy::byte_buffer() && *ty != LirTy::format_args() && *ty != LirTy::byte_ref() {
            self.error(
                format!(
                    "data-symbol const needs `#byte_buffer`, `#format_args`, or `ref #bit8`, found `{ty}`"
                ),
                span,
            );
        }
    }

    /// Verifies stores and conversions (field stores, deref stores,
    /// width conversions, field addresses).
    fn verify_store_ops<'a>(
        &mut self,
        func: &LirFunc,
        inst: &'a LirInst,
        places: &Places,
        regs: &mut Regs<'a>,
    ) {
        match inst {
            LirInst::StoreField {
                value,
                obj,
                name,
                idx,
                span,
            } => {
                self.check_operand(value, regs, places, *span, func);
                self.check_field_ref(obj, name, *idx, *span, places);
            }
            LirInst::Store { src, dst, span } => {
                self.check_operand(src, regs, places, *span, func);
                match places.get(dst.as_str()).copied() {
                    Some(ty) if ty.is_ref() => {}
                    _ => {
                        self.error(format!("store through non-reference place `%{dst}`"), *span);
                    }
                }
            }
            LirInst::Convert {
                src, dst, ty, span, ..
            } => {
                if !regs.contains_key(src.as_str()) {
                    self.error(format!("use of undefined register `&{src}`"), *span);
                }
                self.check_type(ty, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            LirInst::FieldAddr {
                obj,
                name,
                idx,
                dst,
                ty,
                span,
            } => {
                self.check_field_ref(obj, name, *idx, *span, places);
                self.check_type(ty, *span);
                self.define_reg(regs, dst, ty, false, *span, func);
            }
            _ => {
                lilac_error::bug!(Span::dummy(), "non-store instruction in store verifier");
            }
        }
    }

    /// Tracks call places (declared) and marks outputs assigned.
    fn track_call_places<'a>(
        &mut self,
        places: &Places,
        assigned: &mut HashSet<&'a str>,
        ins: &'a [String],
        outs: &'a [String],
        span: Span,
    ) {
        for place in ins.iter().chain(outs.iter()) {
            if !places.contains_key(place.as_str()) {
                self.error(format!("call through undeclared place `%{place}`"), span);
            }
        }
        for place in outs {
            assigned.insert(place.as_str());
        }
    }

    /// Checks a field reference against its owner's layout (name and index).
    fn check_field_ref(&mut self, obj: &str, name: &str, idx: u32, span: Span, places: &Places) {
        let Some(owner_ty) = places.get(obj).copied() else {
            self.error(format!("field of undeclared place `%{obj}`"), span);
            return;
        };
        let owner_ty = owner_ty.strip_ref();
        let Some(layout) = fields_of(&self.module.layouts, owner_ty) else {
            self.error(
                format!("field of `{obj}` with unknown layout `{owner_ty}`"),
                span,
            );
            return;
        };
        let Some((field_name, _)) = layout.get(idx as usize) else {
            self.error(
                format!("field index `{idx}` out of bounds for `{owner_ty}`"),
                span,
            );
            return;
        };
        if field_name != name {
            self.error(
                format!("field `.{name}` is not at index `{idx}` of `{owner_ty}`"),
                span,
            );
        }
    }

    /// Checks `addr.func` (target exists, type known).
    fn check_addr_func(&mut self, target: &str, ty: &LirTy, span: Span) {
        if !self.module.funcs.iter().any(|item| item.name == target) {
            self.error(format!("address of unknown function `@{target}`"), span);
        }
        self.check_type(ty, span);
    }

    /// Checks `load`/`addr` (place exists, type known) and defines the reg.
    fn verify_mem_op<'a>(
        &mut self,
        func: &LirFunc,
        inst: &'a LirInst,
        places: &Places,
        regs: &mut Regs<'a>,
    ) {
        let (place, dst, ty, span) = match inst {
            LirInst::Load { src, dst, ty, span } => (src, dst, ty, span),
            LirInst::Addr {
                place,
                dst,
                ty,
                span,
            } => (place, dst, ty, span),
            _ => lilac_error::bug!(Span::dummy(), "non-memory instruction in verify_mem_op"),
        };
        if !places.contains_key(place.as_str()) {
            self.error(
                format!("address of undeclared place `%{place}` in `@{}`", func.name),
                *span,
            );
        }
        self.check_type(ty, *span);
        self.define_reg(regs, dst, ty, false, *span, func);
    }

    /// Checks a call target: register definedness for indirect calls,
    /// name and arity for direct calls, signature for builtin calls.
    fn check_call_target(
        &mut self,
        callee: &LirCallee,
        ins: &[String],
        outs: &[String],
        places: &Places,
        span: Span,
        regs: &Regs,
    ) {
        match callee {
            LirCallee::Reg(reg) => {
                if !regs.contains_key(reg.as_str()) {
                    self.error(format!("call through undefined register `&{reg}`"), span);
                }
            }
            LirCallee::Func(target) => {
                let Some(found) = self.module.funcs.iter().find(|func| func.name == *target) else {
                    self.error(format!("call to unknown function `@{target}`"), span);
                    return;
                };
                if ins.len() != found.ins.len() || outs.len() != found.outs.len() {
                    self.error(
                        format!(
                            "call to `@{target}` takes {} in and {} out, found {} in and {} out",
                            found.ins.len(),
                            found.outs.len(),
                            ins.len(),
                            outs.len(),
                        ),
                        span,
                    );
                }
            }
            LirCallee::Builtin(name) => {
                let Some(sig) = builtin_sig(name) else {
                    self.error(format!("call to unknown builtin `{name}`"), span);
                    return;
                };
                if ins.len() != sig.ins.len() || outs.len() != sig.outs.len() {
                    self.error(
                        format!(
                            "call to `{name}` takes {} in and {} out, found {} in and {} out",
                            sig.ins.len(),
                            sig.outs.len(),
                            ins.len(),
                            outs.len(),
                        ),
                        span,
                    );
                    return;
                }
                let slots = ins
                    .iter()
                    .zip(&sig.ins)
                    .map(|slot| ("input", slot))
                    .chain(outs.iter().zip(&sig.outs).map(|slot| ("output", slot)));
                for (side, (place, expected)) in slots {
                    match places.get(place.as_str()) {
                        Some(actual) if *actual == expected => {}
                        actual => {
                            let actual =
                                actual.map_or("undeclared".to_owned(), ToString::to_string);
                            self.error(
                                format!(
                                    "builtin `{name}` {side} `%{place}` is `{actual}`, expected `{expected}`"
                                ),
                                span,
                            );
                        }
                    }
                }
            }
        }
    }

    /// Checks a `field` against its owner's layout (name, index, and type).
    fn check_field_layout(
        &mut self,
        owner_ty: &LirTy,
        name: &str,
        idx: u32,
        ty: &LirTy,
        span: Span,
    ) {
        let owner_ty = owner_ty.strip_ref();
        let Some(layout) = fields_of(&self.module.layouts, owner_ty) else {
            self.error(format!("field with unknown layout `{owner_ty}`"), span);
            return;
        };
        let Some((field_name, field_ty)) = layout.get(idx as usize) else {
            self.error(
                format!("field index `{idx}` out of bounds for `{owner_ty}`"),
                span,
            );
            return;
        };
        if field_name != name || field_ty != ty {
            self.error(
                format!("field `.{name}` is `{field_ty}` at index `{idx}`, not `{ty}`"),
                span,
            );
        }
    }

    /// Checks arithmetic widths. Integer ops need bit-container operands and
    /// result; float ops need `#fbit16/32/64/128` operands (real float
    /// containers), with a `#bit8` result for float comparisons.
    fn check_arith_width(
        &mut self,
        op: &ArithOp,
        ty: &LirTy,
        lhs: &str,
        rhs: &str,
        regs: &Regs,
        span: Span,
    ) {
        let is_float_arith = matches!(
            op,
            ArithOp::FAdd | ArithOp::FSub | ArithOp::FMul | ArithOp::FDiv | ArithOp::FMod
        );
        let is_float_cmp = matches!(
            op,
            ArithOp::FLt | ArithOp::FGt | ArithOp::FLe | ArithOp::FGe
        );
        if is_float_cmp {
            if *ty != LirTy::Bit(8) {
                self.error(
                    format!("`{}` needs a `#bit8` result, found `{ty}`", print_op(op)),
                    span,
                );
            }
        } else if is_float_arith {
            if !ty.is_float() {
                self.error(
                    format!("`{}` needs a float type, found `{ty}`", print_op(op)),
                    span,
                );
                return;
            }
        } else if !ty.is_int() {
            self.error(
                format!(
                    "`{}` needs a bit-container type, found `{ty}`",
                    print_op(op)
                ),
                span,
            );
            return;
        }
        if !(is_float_arith || is_float_cmp) {
            // Integer operands must match each other, and the result unless
            // the op is a comparison (whose result is `#bit8`).
            let operand_tys: Vec<&LirTy> = [lhs, rhs]
                .iter()
                .filter_map(|reg| regs.get(*reg).map(|def| def.ty))
                .collect();
            if let [left, right] = operand_tys.as_slice() {
                let is_eq = matches!(op, ArithOp::Eq);
                let mismatched = left != right
                    || (!is_eq && !left.is_int())
                    || (!op.is_comparison() && *left != ty)
                    || (op.is_comparison() && *ty != LirTy::Bit(8));
                if mismatched {
                    self.error(
                        format!(
                            "`{}` operands `{left}`/`{right}` do not match result `{ty}`",
                            print_op(op)
                        ),
                        span,
                    );
                }
            }
            return;
        }
        for reg in [lhs, rhs] {
            if let Some(def) = regs.get(reg) {
                if !def.ty.is_float() {
                    self.error(
                        format!(
                            "`{}` needs float operands, found `{}` in `&{reg}`",
                            print_op(op),
                            def.ty
                        ),
                        span,
                    );
                }
            }
        }
    }

    fn check_operand(
        &mut self,
        operand: &LirOperand,
        regs: &Regs,
        places: &Places,
        span: Span,
        func: &LirFunc,
    ) {
        match operand {
            LirOperand::Reg(reg) => {
                if !regs.contains_key(reg.as_str()) {
                    self.error(
                        format!("use of undefined register `&{reg}` in `@{}`", func.name),
                        span,
                    );
                }
            }
            LirOperand::Place(place) => {
                if !places.contains_key(place.as_str()) {
                    self.error(
                        format!("use of undeclared place `%{place}` in `@{}`", func.name),
                        span,
                    );
                }
            }
        }
    }

    fn check_type(&mut self, ty: &LirTy, span: Span) {
        if !self.valid_type(ty) {
            self.error(format!("unknown type `{ty}`"), span);
        }
    }

    fn define_reg<'a>(
        &mut self,
        regs: &mut Regs<'a>,
        dst: &'a str,
        ty: &'a LirTy,
        is_const: bool,
        span: Span,
        func: &LirFunc,
    ) {
        if regs.insert(dst, RegDef { is_const, ty }).is_some() {
            self.error(
                format!("register `&{dst}` defined twice in `@{}`", func.name),
                span,
            );
        }
    }

    fn verify_term(&mut self, func: &LirFunc, term: &LirTerm, regs: &Regs, has_fallthrough: bool) {
        let labels: HashSet<&str> = func
            .blocks
            .iter()
            .map(|block| block.label.as_str())
            .collect();
        match term {
            LirTerm::Branch { target, span } => {
                if !labels.contains(target.as_str()) {
                    self.error(
                        format!("branch to unknown block `${target}` in `@{}`", func.name),
                        *span,
                    );
                }
            }
            LirTerm::BranchIf {
                cond,
                then,
                els,
                span,
            } => {
                match regs.get(cond.as_str()) {
                    None => {
                        self.error(format!("branch on undefined register `&{cond}`"), *span);
                    }
                    Some(def) if def.is_const => {
                        self.error(
                            format!("branch on constant `&{cond}`; lowering decides constant conditions"),
                            *span,
                        );
                    }
                    Some(_) => {}
                }
                for target in [then].into_iter().chain(els.as_ref()) {
                    if !labels.contains(target.as_str()) {
                        self.error(
                            format!("branch to unknown block `${target}` in `@{}`", func.name),
                            *span,
                        );
                    }
                }
                if els.is_none() && !has_fallthrough {
                    self.error(
                        format!(
                            "`branch.if` without an `else` in `@{}` has no following block for its false edge",
                            func.name
                        ),
                        *span,
                    );
                }
            }
            LirTerm::Return { .. } | LirTerm::Unreachable { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    fn t(text: &str) -> crate::LirTy {
        crate::LirTy::parse(text).expect("type spelling")
    }

    use super::*;
    use lilac_span::{FileId, Span};

    fn test_span() -> Span {
        Span::dummy()
    }

    pub(crate) fn canonical_module() -> LirModule {
        LirModule {
            version: LILAC_IR_VERSION.to_owned(),
            target: "x86_64-unknown-none".to_owned(),
            data: Vec::new(),
            funcs: vec![LirFunc {
                name: "gen_function__bit32".to_owned(),
                ins: vec![("in".to_owned(), t("#bit32"))],
                outs: vec![("out".to_owned(), t("#bit32"))],
                is_extern: false,
                inline: false,
                blocks: vec![LirBlock {
                    label: "entry".to_owned(),
                    insts: vec![
                        LirInst::CopyReg {
                            src: LirOperand::Place("in".to_owned()),
                            dst: "1".to_owned(),
                            ty: t("#bit32"),
                            span: test_span(),
                        },
                        LirInst::CopyPlace {
                            src: LirOperand::Reg("1".to_owned()),
                            dst: "out".to_owned(),
                            span: test_span(),
                        },
                    ],
                    term: LirTerm::Return { span: test_span() },
                    span: test_span(),
                }],
            }],
            layouts: HashMap::new(),
        }
    }

    #[test]
    fn printer_emits_canonical() {
        let text = canonical_module().to_text();
        let expected = "lilac_ir = \"0.4\"\ntarget = \"x86_64-unknown-none\"\nfunction @gen_function__bit32\n  in #bit32 %in\n  out #bit32 %out\n{\n  $entry:\n    &1: #bit32 = copy %in\n    copy &1 -> %out\n    return\n}\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn printer_emits_type_sections() {
        let mut module = canonical_module();
        module.layouts.insert(
            "Number".to_owned(),
            vec![
                ("tag".to_owned(), t("#bit8")),
                ("val".to_owned(), t("#bit32")),
            ],
        );
        let text = module.to_text();
        assert!(
            text.contains("type #Number {\n    tag: #bit8,\n    val: #bit32,\n}\n"),
            "{text}"
        );
    }

    fn e070_count(diags: &lilac_error::Diags) -> usize {
        diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E070")
            .count()
    }

    #[test]
    fn printer_renders_unsigned_ops() {
        assert_eq!(print_op(&ArithOp::IGtU), "i.cmp.gt.u");
        assert_eq!(print_op(&ArithOp::IDivU), "i.div.u");
        assert_eq!(print_op(&ArithOp::IModU), "i.mod.u");
    }

    #[test]
    fn verify_accepts_clean() {
        let module = canonical_module();
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_ok());
        assert_eq!(e070_count(&diags), 0);
    }

    #[test]
    fn verify_accepts_fbit_types() {
        let module = LirModule::new("x86_64-unknown-none");
        let mut diags = lilac_error::Diags::new();
        let verifier = Verifier {
            module: &module,
            diags: &mut diags,
            failed: false,
        };
        for ty in ["#fbit16", "#fbit32", "#fbit64", "#fbit128"] {
            assert!(verifier.valid_type(&t(ty)), "{ty}");
        }
    }

    #[test]
    fn verify_rejects_old_copy() {
        // Old-style unchecked operand: a register that was never defined.
        let mut module = canonical_module();
        module.funcs[0].blocks[0].insts[1] = LirInst::CopyPlace {
            src: LirOperand::Reg("9".to_owned()),
            dst: "out".to_owned(),
            span: test_span(),
        };
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1);
    }

    /// `copy` permits `ref X` <-> `ref Y` under opaque pointers, but a
    /// reference/value mix (`IR_SPEC.md`; e.g. `ref #Object` into `#Object`)
    /// is ill-typed and must be rejected, not silently pointer-copied.
    #[test]
    fn verify_rejects_copy_place_type_mismatch() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module
            .layouts
            .insert("Object".to_owned(), vec![("value".to_owned(), t("#bit32"))]);
        module.funcs.push(LirFunc {
            name: "mismatch".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![
                    LirInst::Alloca {
                        ty: t("ref #Object"),
                        place: "r".to_owned(),
                        span: test_span(),
                    },
                    LirInst::Alloca {
                        ty: t("#Object"),
                        place: "v".to_owned(),
                        span: test_span(),
                    },
                    LirInst::CopyPlace {
                        src: LirOperand::Place("r".to_owned()),
                        dst: "v".to_owned(),
                        span: test_span(),
                    },
                ],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        });
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1, "{:?}", diags.diagnostics());
        let msgs: Vec<String> = diags.diagnostics().iter().map(|d| d.msg.clone()).collect();
        assert!(msgs.iter().any(|m| m.contains("copy")), "{msgs:?}");
    }

    fn trivial_func(name: &str) -> LirFunc {
        LirFunc {
            name: name.to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: Vec::new(),
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        }
    }

    #[test]
    fn verify_rejects_duplicate_function_names() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(trivial_func("dup"));
        module.funcs.push(trivial_func("dup"));
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1, "{:?}", diags.diagnostics());
        let msgs: Vec<String> = diags.diagnostics().iter().map(|d| d.msg.clone()).collect();
        assert!(
            msgs.iter().any(|m| m.contains("duplicate function")),
            "{msgs:?}"
        );
    }

    /// Two reference places of different pointee types are compatible under
    /// opaque pointers: `copy` between them is a pointer copy, not a value copy.
    #[test]
    fn verify_accepts_ref_to_ref_copy() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(LirFunc {
            name: "refcopy".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![
                    LirInst::Alloca {
                        ty: t("ref #bit8"),
                        place: "a".to_owned(),
                        span: test_span(),
                    },
                    LirInst::Alloca {
                        ty: t("ref #byte_buffer"),
                        place: "b".to_owned(),
                        span: test_span(),
                    },
                    LirInst::CopyPlace {
                        src: LirOperand::Place("a".to_owned()),
                        dst: "b".to_owned(),
                        span: test_span(),
                    },
                ],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        });
        let mut diags = lilac_error::Diags::new();
        assert!(
            verify(&module, &mut diags).is_ok(),
            "{:?}",
            diags.diagnostics()
        );
    }

    #[test]
    fn verify_rejects_const_branchif() {
        // Lowering decides constant conditions, so `branch.if` on a constant
        // is invalid IR.
        let mut module = canonical_module();
        module.funcs[0].blocks[0].insts[1] = LirInst::Const {
            dst: "2".to_owned(),
            lit: "0x1".to_owned(),
            ty: t("#bit8"),
            span: test_span(),
        };
        module.funcs[0].blocks[0].term = LirTerm::BranchIf {
            cond: "2".to_owned(),
            then: "entry".to_owned(),
            els: None,
            span: test_span(),
        };
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        // The dropped `copy &1 -> %out` also starves the out place; pin the
        // constant-branch error by message instead of by count.
        let msgs: Vec<String> = diags.diagnostics().iter().map(|d| d.msg.clone()).collect();
        assert!(
            diags.diagnostics().iter().any(|d| d.code == "E070")
                && msgs.iter().any(|m| m.contains("constant")),
            "{msgs:?}"
        );
    }

    #[test]
    fn verify_rejects_bad_branch_target() {
        let mut module = canonical_module();
        module.funcs[0].blocks[0].term = LirTerm::Branch {
            target: "nope".to_owned(),
            span: test_span(),
        };
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1);
    }

    /// An `else`-less `branch.if` uses the *next* block as its false edge, so a
    /// final one has nowhere to fall through to and must be rejected.
    #[test]
    fn verify_rejects_final_branchif_without_else() {
        let mut module = canonical_module();
        module.funcs[0].blocks[0].term = LirTerm::BranchIf {
            cond: "1".to_owned(),
            then: "entry".to_owned(),
            els: None,
            span: test_span(),
        };
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1, "{:?}", diags.diagnostics());
        let msgs: Vec<String> = diags.diagnostics().iter().map(|d| d.msg.clone()).collect();
        assert!(msgs.iter().any(|m| m.contains("else")), "{msgs:?}");
    }

    /// The same terminator is fine when a following block supplies the false
    /// edge.
    #[test]
    fn verify_accepts_branchif_fallthrough_with_next_block() {
        let mut module = canonical_module();
        module.funcs[0].blocks[0].term = LirTerm::BranchIf {
            cond: "1".to_owned(),
            then: "entry".to_owned(),
            els: None,
            span: test_span(),
        };
        module.funcs[0].blocks.push(LirBlock {
            label: "next".to_owned(),
            insts: Vec::new(),
            term: LirTerm::Return { span: test_span() },
            span: test_span(),
        });
        let mut diags = lilac_error::Diags::new();
        assert!(
            verify(&module, &mut diags).is_ok(),
            "{:?}",
            diags.diagnostics()
        );
    }

    /// Builds the canonical module with two extra operand registers of the
    /// given types feeding one `op`.
    fn module_with_arith(op: ArithOp, lhs_ty: &str, rhs_ty: &str, result_ty: &str) -> LirModule {
        let mut module = canonical_module();
        module.funcs[0].blocks[0].insts.push(LirInst::CopyReg {
            src: LirOperand::Place("in".to_owned()),
            dst: "2".to_owned(),
            ty: t(lhs_ty),
            span: test_span(),
        });
        module.funcs[0].blocks[0].insts.push(LirInst::CopyReg {
            src: LirOperand::Place("in".to_owned()),
            dst: "3".to_owned(),
            ty: t(rhs_ty),
            span: test_span(),
        });
        module.funcs[0].blocks[0].insts.push(LirInst::Arith {
            op,
            lhs: "2".to_owned(),
            rhs: "3".to_owned(),
            dst: "4".to_owned(),
            ty: t(result_ty),
            span: test_span(),
        });
        module
    }

    #[test]
    fn verify_rejects_float_arith_on_bit_operands() {
        for (op, result_ty) in [
            (ArithOp::FAdd, "#bit32"),
            (ArithOp::FLt, "#bit8"),
            (ArithOp::FGt, "#bit8"),
        ] {
            let label = format!("{op:?}");
            let module = module_with_arith(op, "#bit32", "#bit32", result_ty);
            let mut diags = lilac_error::Diags::new();
            assert!(verify(&module, &mut diags).is_err(), "{label}");
            assert!(
                e070_count(&diags) >= 1,
                "{label}: {:?}",
                diags.diagnostics()
            );
            let msgs: Vec<String> = diags.diagnostics().iter().map(|d| d.msg.clone()).collect();
            assert!(
                msgs.iter().any(|m| m.contains("float")),
                "{label}: {msgs:?}"
            );
        }
    }

    #[test]
    fn verify_accepts_float_arith_on_fbit_operands() {
        for (op, ty) in [
            (ArithOp::FAdd, "#fbit16"),
            (ArithOp::FAdd, "#fbit32"),
            (ArithOp::FDiv, "#fbit64"),
            (ArithOp::FMod, "#fbit128"),
            (ArithOp::FLt, "#fbit32"),
            (ArithOp::FGt, "#fbit64"),
        ] {
            let result_ty = if matches!(op, ArithOp::FLt | ArithOp::FGt) {
                "#bit8"
            } else {
                ty
            };
            let module = module_with_arith(op, ty, ty, result_ty);
            let mut diags = lilac_error::Diags::new();
            assert!(
                verify(&module, &mut diags).is_ok(),
                "{ty}: {:?}",
                diags.diagnostics()
            );
        }
    }

    #[test]
    fn verify_accepts_unsigned_ops_on_bit_containers() {
        for (op, result_ty) in [
            (ArithOp::IGtU, "#bit8"),
            (ArithOp::IDivU, "#bit32"),
            (ArithOp::IModU, "#bit32"),
        ] {
            let label = format!("{op:?}");
            let module = module_with_arith(op, "#bit32", "#bit32", result_ty);
            let mut diags = lilac_error::Diags::new();
            assert!(
                verify(&module, &mut diags).is_ok(),
                "{label}: {:?}",
                diags.diagnostics()
            );
        }
    }

    #[test]
    fn verify_rejects_empty_function() {
        let mut module = canonical_module();
        module.funcs[0].blocks.clear();
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1);
    }

    #[test]
    fn printer_emits_bodyless_extern() {
        let module = LirModule {
            version: LILAC_IR_VERSION.to_owned(),
            target: "x86_64-unknown-none".to_owned(),
            data: Vec::new(),
            funcs: vec![LirFunc {
                name: "puts".to_owned(),
                ins: vec![("s".to_owned(), t("#byte_buffer"))],
                outs: Vec::new(),
                is_extern: true,
                inline: false,
                blocks: Vec::new(),
            }],
            layouts: HashMap::new(),
        };
        let text = module.to_text();
        assert!(
            text.contains("extern @puts\n  in #byte_buffer %s\n"),
            "{text}"
        );
        assert!(!text.contains("{\n"), "extern must print no body: {text}");
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_ok(), "clean extern verifies");
    }

    #[test]
    fn verifier_rejects_extern_with_block() {
        let mut module = canonical_module();
        module.funcs[0].is_extern = true;
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1);
    }

    /// A bodyless extern with a valid signature verifies.
    fn extern_module() -> LirModule {
        LirModule {
            version: LILAC_IR_VERSION.to_owned(),
            target: "x86_64-unknown-none".to_owned(),
            data: Vec::new(),
            funcs: vec![LirFunc {
                name: "puts".to_owned(),
                ins: vec![("s".to_owned(), t("#byte_buffer"))],
                outs: Vec::new(),
                is_extern: true,
                inline: false,
                blocks: Vec::new(),
            }],
            layouts: HashMap::new(),
        }
    }

    #[test]
    fn verifier_accepts_clean_extern() {
        let module = extern_module();
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_ok(), "{diags:?}");
        assert_eq!(e070_count(&diags), 0);
    }

    #[test]
    fn verifier_rejects_extern_duplicate_places() {
        let mut module = extern_module();
        module.funcs[0].outs = vec![("s".to_owned(), t("#byte_buffer"))];
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1);
    }

    #[test]
    fn verifier_rejects_extern_unknown_type() {
        let mut module = extern_module();
        module.funcs[0].ins[0].1 = t("#nope");
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1);
    }

    #[test]
    fn verifier_accepts_ref_unknown() {
        let mut module = extern_module();
        module.funcs[0].outs = vec![("p".to_owned(), t("ref #unknown"))];
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_ok(), "{diags:?}");
        assert_eq!(e070_count(&diags), 0);
    }

    #[test]
    fn verifier_rejects_bare_unknown() {
        let mut module = extern_module();
        module.funcs[0].outs = vec![("p".to_owned(), t("#unknown"))];
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1);
    }

    #[test]
    fn verifier_rejects_non_extern_duplicate_places() {
        let mut module = canonical_module();
        module.funcs[0].ins = vec![
            ("in".to_owned(), t("#bit32")),
            ("in".to_owned(), t("#bit64")),
        ];
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1);
    }

    #[test]
    fn builtin_call_prints_and_verifies() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![LirInst::Call {
                    func: LirCallee::Builtin("@lilac_abort".into()),
                    ins: Vec::new(),
                    outs: Vec::new(),
                    span: test_span(),
                }],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        });
        let text = module.to_text();
        assert!(text.contains("call @lilac_abort"), "{text}");
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_ok(), "{diags:?}");
    }

    #[test]
    fn ptr_add_and_abort_builtin_signatures() {
        let ptr_add = builtin_sig("@ptr_add").expect("@ptr_add is a builtin");
        assert_eq!(ptr_add.ins, vec![t("ref #bit8"), t("#bit_native")]);
        assert_eq!(ptr_add.outs, vec![t("ref #bit8")]);

        let abort = builtin_sig("@lilac_abort").expect("@lilac_abort is a builtin");
        assert!(abort.ins.is_empty(), "{:?}", abort.ins);
        assert!(abort.outs.is_empty(), "{:?}", abort.outs);
    }

    /// After the runtime moved into LIR, only the three leaf intrinsics remain
    /// builtins; every moved name is now an ordinary function symbol.
    #[test]
    fn builtin_sig_shrinks_to_leaves() {
        assert!(builtin_sig("@memcpy").is_some());
        for removed in [
            "@byte_buffer_from",
            "@byte_buffer_ptr",
            "@byte_buffer_len",
            "@format_args_new",
            "@format_args_push",
            "@format_args_count",
            "@format_args_piece",
            "@load_u8",
            "@store_u8",
        ] {
            assert!(
                builtin_sig(removed).is_none(),
                "`{removed}` must no longer be a builtin"
            );
        }
    }

    #[test]
    fn data_symbol_const_verifies() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.data.push(LirData {
            name: "c0".to_owned(),
            bytes: "hi".as_bytes().to_vec(),
            span: test_span(),
        });
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![LirInst::Const {
                    dst: "1".to_owned(),
                    lit: "!c0".to_owned(),
                    ty: t("#byte_buffer"),
                    span: test_span(),
                }],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        });
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_ok(), "{diags:?}");
    }

    #[test]
    fn unknown_data_symbol_rejected() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![LirInst::Const {
                    dst: "1".to_owned(),
                    lit: "!c9".to_owned(),
                    ty: t("#byte_buffer"),
                    span: test_span(),
                }],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        });
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn builtin_layout_has_expected_fields() {
        let byte_buffer = builtin_layout(&t("#byte_buffer")).expect("#byte_buffer has a layout");
        assert_eq!(byte_buffer[0], ("len".to_owned(), t("#bit_native")));
        let format_args = builtin_layout(&t("#format_args")).expect("#format_args has a layout");
        assert_eq!(format_args.len(), 17);
        assert_eq!(format_args[1], ("p0".to_owned(), t("#byte_buffer")));
    }

    /// A `field` on a builtin aggregate resolves against `builtin_layout`
    /// (these types never appear in `module.layouts`).
    #[test]
    fn field_on_byte_buffer_uses_builtin_layout() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: vec![("buffer".to_owned(), t("#byte_buffer"))],
            outs: vec![("out".to_owned(), t("ref #bit8"))],
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![
                    LirInst::Field {
                        obj: "buffer".to_owned(),
                        name: "data".to_owned(),
                        idx: 1,
                        dst: "r".to_owned(),
                        ty: t("ref #bit8"),
                        span: test_span(),
                    },
                    LirInst::CopyPlace {
                        src: LirOperand::Reg("r".to_owned()),
                        dst: "out".to_owned(),
                        span: test_span(),
                    },
                ],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        });
        let mut diags = lilac_error::Diags::new();
        assert!(
            verify(&module, &mut diags).is_ok(),
            "{:?}",
            diags.diagnostics()
        );
    }

    #[test]
    fn spanned_text_matches_to_text_byte_for_byte() {
        let mut module = canonical_module();
        module
            .layouts
            .insert("Number".to_owned(), vec![("tag".to_owned(), t("#bit8"))]);
        module.data.push(LirData {
            name: "c0".to_owned(),
            bytes: "hi".as_bytes().to_vec(),
            span: test_span(),
        });
        let spanned = module.to_text_spanned();
        assert_eq!(spanned.text, module.to_text());
        assert_eq!(spanned.lines.len(), spanned.text.lines().count());
    }

    #[test]
    fn spanned_instruction_and_terminator_lines_carry_spans() {
        let module = canonical_module();
        let spanned = module.to_text_spanned();
        let lines: Vec<&str> = spanned.text.lines().collect();
        let inst_line = lines
            .iter()
            .position(|l| l.contains("= copy %in"))
            .expect("instruction line");
        let term_line = lines
            .iter()
            .position(|l| l.trim() == "return")
            .expect("terminator line");
        assert_eq!(spanned.lines[inst_line], Some(test_span()));
        assert_eq!(spanned.lines[term_line], Some(test_span()));
    }

    /// Every construct carries its *own* distinct, non-dummy span, so a
    /// swapped `inst_span`/`term_span` field or an off-by-one in
    /// `physical_lines` cannot pass: the instruction line must carry the
    /// instruction's bytes, the terminator line the terminator's, and the
    /// block-label line the block's.
    #[test]
    fn spanned_lines_carry_their_own_distinct_spans() {
        let block_span = Span::new(100, 400, FileId(0));
        let const1_span = Span::new(110, 120, FileId(0));
        let const2_span = Span::new(130, 140, FileId(0));
        let arith_span = Span::new(150, 170, FileId(0));
        let term_span = Span::new(380, 390, FileId(0));

        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![
                    LirInst::Const {
                        dst: "1".to_owned(),
                        lit: "0x1".to_owned(),
                        ty: t("#bit_native"),
                        span: const1_span,
                    },
                    LirInst::Const {
                        dst: "2".to_owned(),
                        lit: "0x2".to_owned(),
                        ty: t("#bit_native"),
                        span: const2_span,
                    },
                    LirInst::Arith {
                        op: ArithOp::IAdd,
                        lhs: "1".to_owned(),
                        rhs: "2".to_owned(),
                        dst: "3".to_owned(),
                        ty: t("#bit_native"),
                        span: arith_span,
                    },
                ],
                term: LirTerm::Return { span: term_span },
                span: block_span,
            }],
        });

        let spanned = module.to_text_spanned();
        let lines: Vec<&str> = spanned.text.lines().collect();
        let find = |needle: &str| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no line containing `{needle}` in:\n{}", spanned.text))
        };
        let label_line = find("$entry:");
        let const1_line = find("const 0x1");
        let const2_line = find("const 0x2");
        let arith_line = find("i.add");
        let term_line = find("return");

        assert_eq!(spanned.lines[label_line], Some(block_span));
        assert_eq!(spanned.lines[const1_line], Some(const1_span));
        assert_eq!(spanned.lines[const2_line], Some(const2_span));
        assert_eq!(spanned.lines[arith_line], Some(arith_span));
        assert_eq!(spanned.lines[term_line], Some(term_span));
    }

    #[test]
    fn spanned_header_and_layout_lines_have_no_span() {
        let mut module = canonical_module();
        module
            .layouts
            .insert("Number".to_owned(), vec![("tag".to_owned(), t("#bit8"))]);
        let spanned = module.to_text_spanned();
        let lines: Vec<&str> = spanned.text.lines().collect();
        for needle in [
            "lilac_ir =",
            "target =",
            "function @",
            "  in ",
            "type #Number",
            "{",
            "}",
        ] {
            if let Some(i) = lines.iter().position(|l| l.contains(needle)) {
                assert_eq!(
                    spanned.lines[i], None,
                    "line `{needle}` should have no span"
                );
            }
        }
    }

    #[test]
    fn spanned_extern_only_module_is_consistent() {
        let mut module = LirModule::default();
        module.funcs.push(LirFunc {
            name: "puts".to_owned(),
            ins: vec![("s".to_owned(), t("#bit8"))],
            outs: vec![],
            is_extern: true,
            inline: false,
            blocks: vec![],
        });
        let spanned = module.to_text_spanned();
        assert_eq!(spanned.text, module.to_text());
        assert_eq!(spanned.lines.len(), spanned.text.lines().count());
        assert!(spanned.lines.iter().all(Option::is_none));
    }

    #[test]
    fn spanned_data_literal_with_newlines_keeps_physical_lines_aligned() {
        let mut module = canonical_module();
        module.data.push(LirData {
            name: "c0".to_owned(),
            bytes: "a\nb".as_bytes().to_vec(),
            span: test_span(),
        });
        let spanned = module.to_text_spanned();
        // (a) Byte-identical to `to_text`, raw newline preserved.
        assert_eq!(spanned.text, module.to_text());
        assert!(
            spanned.text.contains("!c0: str = \"a\nb\""),
            "{}",
            spanned.text
        );
        // (b) One span per physical line.
        assert_eq!(spanned.lines.len(), spanned.text.lines().count());
        // (c) Both physical lines of the data carry the data's span.
        let lines: Vec<&str> = spanned.text.lines().collect();
        let data_line = lines
            .iter()
            .position(|l| l.starts_with("!c0: str"))
            .expect("data line");
        assert_eq!(lines[data_line], "!c0: str = \"a");
        assert_eq!(lines[data_line + 1], "b\"");
        assert_eq!(spanned.lines[data_line], Some(test_span()));
        assert_eq!(spanned.lines[data_line + 1], Some(test_span()));
    }
}
