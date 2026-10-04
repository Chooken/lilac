//! Stable Lilac IR v2 types (printer + verifier land in M2).
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
use lilac_span::Span;

/// IR version header; bump on any normative IR change.
pub const LILAC_IR_VERSION: &str = "0.2";

/// An IR module: versioned header plus data, functions, and layouts.
#[derive(Clone, Debug)]
pub struct LirModule {
    /// `lilac_ir = "..."` header value.
    pub version: String,
    /// `target = "..."` header value (fixes `@bitNative` width).
    pub target: String,
    /// Typed constants (`!c0: str`, `!c1: numlit`).
    pub data: Vec<LirData>,
    /// Functions in source order.
    pub funcs: Vec<LirFunc>,
    /// Type name → `(field, type)` in order (verifier only, never printed).
    pub layouts: HashMap<String, Vec<(String, String)>>,
}

/// One typed constant.
#[derive(Clone, Debug)]
pub struct LirData {
    /// Constant name without sigil (`c0` prints as `!c0`).
    pub name: String,
    /// Constant payload.
    pub kind: LirDataKind,
    /// Definition span.
    pub span: Span,
}

/// Constant payloads.
#[derive(Clone, Debug)]
pub enum LirDataKind {
    /// UTF-8 bytes (`!c0: str = "hi"`).
    Str(String),
    /// Number source bytes (`!c1: numlit = "10.5"`).
    NumLit(String),
}

/// One function: signature places plus labeled blocks.
#[derive(Clone, Debug)]
pub struct LirFunc {
    /// Function name without sigil (`main` prints as `@main`).
    pub name: String,
    /// `(place, type)` inputs (print as `in #t %p`).
    pub ins: Vec<(String, String)>,
    /// `(place, type)` outputs (print as `out #t %p`).
    pub outs: Vec<(String, String)>,
    /// Blocks in layout order (non-empty: every function has a body).
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

/// A call target: register (indirect) or function (direct).
#[derive(Clone, Debug)]
pub enum LirCallee {
    /// Indirect call through a register holding a function address.
    Reg(String),
    /// Direct call by function name (without sigil).
    Func(String),
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
    /// `i.mod`.
    IMod,
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
    /// `i.cmp.gt`.
    IGt,
    /// `f.cmp.lt`.
    FLt,
    /// `f.cmp.gt`.
    FGt,
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
        ty: String,
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
        ty: String,
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
        ty: String,
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
        ty: String,
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
        ty: String,
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
        ty: String,
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
        ty: String,
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
        ty: String,
        /// Instruction span.
        span: Span,
    },
    /// `alloca #t %x : zeroed` (every place starts zeroed).
    Alloca {
        /// Place type.
        ty: String,
        /// Place (without sigil).
        place: String,
        /// Instruction span.
        span: Span,
    },
    /// `drop %x` (runs `@on_drop` where the type declares one).
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
        ty: String,
        /// Instruction span.
        span: Span,
    },
    /// `@compile_error` site (folding removes it or reports `E031`;
    /// reaching `verify` is an internal error).
    CompileError {
        /// Error message.
        msg: String,
        /// Message literal span.
        msg_span: Span,
        /// Call-site span.
        call_span: Span,
    },
}

/// Width-conversion operators.
#[derive(Clone, Debug)]
pub enum ConvertOp {
    /// `trunc` (narrowing).
    Trunc,
    /// `extend.zero` (zero-extension).
    ExtendZero,
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
    /// `panic !c`.
    Panic {
        /// Message const (without sigil).
        msg: String,
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

    /// Canonical header text.
    #[must_use]
    pub fn header_text(&self) -> String {
        format!(
            "lilac_ir = \"{}\"\ntarget = \"{}\"\n",
            self.version, self.target
        )
    }

    /// Canonical module text: header, data consts, then functions.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = self.header_text();
        for data in &self.data {
            out.push_str(&print_data(data));
            out.push('\n');
        }
        for func in &self.funcs {
            out.push_str(&print_func(func));
        }
        out
    }
}

impl Default for LirModule {
    fn default() -> Self {
        Self::new("x86_64-unknown-none")
    }
}

/// Escapes `"` and `\` inside `data` literal text.
fn escape_data(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch == '"' || ch == '\\' {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn print_data(data: &LirData) -> String {
    let (kind, value) = match &data.kind {
        LirDataKind::Str(text) => ("str", escape_data(text)),
        LirDataKind::NumLit(text) => ("numlit", escape_data(text)),
    };
    format!("!{}: {kind} = \"{value}\"", data.name)
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
    }
}

fn print_op(op: &ArithOp) -> &'static str {
    match op {
        ArithOp::IAdd => "i.add",
        ArithOp::ISub => "i.sub",
        ArithOp::IMul => "i.mul",
        ArithOp::IDiv => "i.div",
        ArithOp::IMod => "i.mod",
        ArithOp::FAdd => "f.add",
        ArithOp::FSub => "f.sub",
        ArithOp::FMul => "f.mul",
        ArithOp::FDiv => "f.div",
        ArithOp::FMod => "f.mod",
        ArithOp::Eq => "cmp.eql",
        ArithOp::ILt => "i.cmp.lt",
        ArithOp::IGt => "i.cmp.gt",
        ArithOp::FLt => "f.cmp.lt",
        ArithOp::FGt => "f.cmp.gt",
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
        LirTerm::Panic { msg, .. } => format!("panic !{msg}"),
        LirTerm::Unreachable { .. } => "unreachable".to_owned(),
    }
}

fn print_func(func: &LirFunc) -> String {
    let mut out = format!("function @{}\n", func.name);
    for (place, ty) in &func.ins {
        let _ = writeln!(out, "  in {ty} %{place}");
    }
    for (place, ty) in &func.outs {
        let _ = writeln!(out, "  out {ty} %{place}");
    }
    out.push_str("{\n");
    for block in &func.blocks {
        let _ = writeln!(out, "  ${}:", block.label);
        for inst in &block.insts {
            let _ = writeln!(out, "    {}", print_inst(inst));
        }
        let _ = writeln!(out, "    {}", print_term(&block.term));
    }
    out.push_str("}\n");
    out
}

/// Verifies a module: every rule failure is a spanned `E070` diagnostic.
/// A `CompileError` node reaching here means folding never ran — an
/// internal error (`bug!`), never a user diagnostic.
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

/// One register definition: whether it holds a constant (folding order).
#[derive(Clone, Copy)]
struct RegDef {
    is_const: bool,
}

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
        for func in &self.module.funcs {
            self.verify_func(func);
        }
    }

    fn valid_type(&self, ty: &str) -> bool {
        let base = ty.strip_prefix("ref ").unwrap_or(ty);
        matches!(
            base,
            "#bit8"
                | "#bit16"
                | "#bit32"
                | "#bit64"
                | "#bitNative"
                | "#byte_buffer"
                | "#format_args"
                | "#str"
                | "#numlit"
        ) || self.module.layouts.contains_key(base)
    }

    fn verify_func<'a>(&mut self, func: &'a LirFunc) {
        if func.blocks.is_empty() {
            self.error(
                format!("function `@{}` has no blocks", func.name),
                Span::dummy(),
            );
            return;
        }
        let mut places: HashMap<&'a str, &'a str> = HashMap::new();
        for (place, ty) in func.ins.iter().chain(func.outs.iter()) {
            if places.insert(place.as_str(), ty.as_str()).is_some() {
                self.error(
                    format!("duplicate place `%{place}` in `@{}`", func.name),
                    Span::dummy(),
                );
            }
            if !self.valid_type(ty) {
                self.error(format!("unknown type `{ty}`"), Span::dummy());
            }
        }
        let mut assigned: HashSet<&str> = HashSet::new();
        for block in &func.blocks {
            self.verify_block(func, block, &mut places, &mut assigned);
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

    fn verify_block<'a>(
        &mut self,
        func: &'a LirFunc,
        block: &'a LirBlock,
        places: &mut HashMap<&'a str, &'a str>,
        assigned: &mut HashSet<&'a str>,
    ) {
        let mut regs: HashMap<&str, RegDef> = HashMap::new();
        for inst in &block.insts {
            self.verify_inst(func, inst, places, &mut regs, assigned);
        }
        self.verify_term(func, &block.term, &regs);
    }

    fn verify_inst<'a>(
        &mut self,
        func: &LirFunc,
        inst: &'a LirInst,
        places: &mut HashMap<&'a str, &'a str>,
        regs: &mut HashMap<&'a str, RegDef>,
        assigned: &mut HashSet<&'a str>,
    ) {
        match inst {
            LirInst::CopyReg { src, dst, ty, span } => {
                self.check_operand(src, regs, places, *span, func);
                self.check_type(ty, *span);
                self.define_reg(regs, dst, false, *span, func);
            }
            LirInst::CopyPlace { src, dst, span } => {
                self.check_operand(src, regs, places, *span, func);
                if !places.contains_key(dst.as_str()) {
                    self.error(
                        format!("copy to undeclared place `%{dst}` in `@{}`", func.name),
                        *span,
                    );
                }
                assigned.insert(dst.as_str());
            }
            LirInst::Const { dst, ty, span, .. } => {
                self.check_type(ty, *span);
                self.define_reg(regs, dst, true, *span, func);
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
                self.define_reg(regs, dst, false, *span, func);
            }
            LirInst::Call {
                func: callee,
                ins,
                outs,
                span,
            } => {
                self.check_call_target(func, callee, ins.len(), outs.len(), *span, regs);
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
                let owner_ty = places.get(obj.as_str()).copied().unwrap_or("#unknown");
                self.check_field_layout(owner_ty, name, *idx, ty, *span);
                self.define_reg(regs, dst, false, *span, func);
            }
            LirInst::Alloca { ty, place, span } => {
                self.check_type(ty, *span);
                if places.insert(place.as_str(), ty.as_str()).is_some() {
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
                self.check_arith_width(op, ty, *span);
                self.define_reg(regs, dst, false, *span, func);
            }
            LirInst::CompileError { call_span, .. } => {
                lilac_error::bug!(*call_span, "unfolded `@compile_error` reached verify");
            }
            LirInst::StoreField { .. }
            | LirInst::Store { .. }
            | LirInst::Convert { .. }
            | LirInst::FieldAddr { .. } => {
                self.verify_store_ops(func, inst, places, regs);
            }
        }
    }

    /// Verifies stores and conversions (field stores, deref stores,
    /// width conversions, field addresses).
    fn verify_store_ops<'a>(
        &mut self,
        func: &LirFunc,
        inst: &'a LirInst,
        places: &HashMap<&str, &str>,
        regs: &mut HashMap<&'a str, RegDef>,
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
                    Some(ty) if ty.starts_with("ref ") => {}
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
                self.define_reg(regs, dst, false, *span, func);
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
                self.define_reg(regs, dst, false, *span, func);
            }
            _ => {
                lilac_error::bug!(Span::dummy(), "non-store instruction in store verifier");
            }
        }
    }

    /// Tracks call places (declared) and marks outputs assigned.
    fn track_call_places<'a>(
        &mut self,
        places: &HashMap<&str, &str>,
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
    fn check_field_ref(
        &mut self,
        obj: &str,
        name: &str,
        idx: u32,
        span: Span,
        places: &HashMap<&str, &str>,
    ) {
        let owner_ty = places.get(obj).copied().unwrap_or("#unknown");
        let owner_ty = owner_ty.strip_prefix("ref ").unwrap_or(owner_ty);
        let Some(layout) = self.module.layouts.get(owner_ty) else {
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
    fn check_addr_func(&mut self, target: &str, ty: &str, span: Span) {
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
        places: &HashMap<&str, &str>,
        regs: &mut HashMap<&'a str, RegDef>,
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
        self.define_reg(regs, dst, false, *span, func);
    }

    /// Checks a call target: register definedness for indirect calls,
    /// name and arity for direct calls.
    fn check_call_target(
        &mut self,
        func: &LirFunc,
        callee: &LirCallee,
        ins: usize,
        outs: usize,
        span: Span,
        regs: &HashMap<&str, RegDef>,
    ) {
        match callee {
            LirCallee::Reg(reg) => {
                if !regs.contains_key(reg.as_str()) {
                    self.error(
                        format!(
                            "call through undefined register `&{reg}` in `@{}`",
                            func.name
                        ),
                        span,
                    );
                }
            }
            LirCallee::Func(target) => {
                let Some(found) = self.module.funcs.iter().find(|func| func.name == *target) else {
                    self.error(format!("call to unknown function `@{target}`"), span);
                    return;
                };
                if ins != found.ins.len() || outs != found.outs.len() {
                    self.error(
                        format!(
                            "call to `@{target}` takes {} in and {} out, found {ins} in and {outs} out",
                            found.ins.len(),
                            found.outs.len(),
                        ),
                        span,
                    );
                }
            }
        }
    }

    /// Checks a `field` against its owner's layout (name, index, and type).
    fn check_field_layout(&mut self, owner_ty: &str, name: &str, idx: u32, ty: &str, span: Span) {
        let owner_ty = owner_ty.strip_prefix("ref ").unwrap_or(owner_ty);
        let Some(layout) = self.module.layouts.get(owner_ty) else {
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

    /// Checks arithmetic result widths (`i.*` and `f.*` need bit containers).
    fn check_arith_width(&mut self, op: &ArithOp, ty: &str, span: Span) {
        if !matches!(ty, "#bit8" | "#bit16" | "#bit32" | "#bit64" | "#bitNative") {
            self.error(
                format!(
                    "`{}` needs a bit-container type, found `{ty}`",
                    print_op(op)
                ),
                span,
            );
        }
    }

    fn check_operand(
        &mut self,
        operand: &LirOperand,
        regs: &HashMap<&str, RegDef>,
        places: &HashMap<&str, &str>,
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

    fn check_type(&mut self, ty: &str, span: Span) {
        if !self.valid_type(ty) {
            self.error(format!("unknown type `{ty}`"), span);
        }
    }

    fn define_reg<'a>(
        &mut self,
        regs: &mut HashMap<&'a str, RegDef>,
        dst: &'a str,
        is_const: bool,
        span: Span,
        func: &LirFunc,
    ) {
        if regs.insert(dst, RegDef { is_const }).is_some() {
            self.error(
                format!("register `&{dst}` defined twice in `@{}`", func.name),
                span,
            );
        }
    }

    fn verify_term(&mut self, func: &LirFunc, term: &LirTerm, regs: &HashMap<&str, RegDef>) {
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
                            format!("branch on constant `&{cond}`; fold constant branches first"),
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
            }
            LirTerm::Panic { msg, span } => {
                if !self.module.data.iter().any(|datum| datum.name == *msg) {
                    self.error(format!("panic of undeclared const `!{msg}`"), *span);
                }
            }
            LirTerm::Return { .. } | LirTerm::Unreachable { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_span::Span;

    fn test_span() -> Span {
        Span::dummy()
    }

    fn canonical_module() -> LirModule {
        LirModule {
            version: LILAC_IR_VERSION.to_owned(),
            target: "x86_64-unknown-none".to_owned(),
            data: Vec::new(),
            funcs: vec![LirFunc {
                name: "gen_function__bit32".to_owned(),
                ins: vec![("in".to_owned(), "#bit32".to_owned())],
                outs: vec![("out".to_owned(), "#bit32".to_owned())],
                blocks: vec![LirBlock {
                    label: "entry".to_owned(),
                    insts: vec![
                        LirInst::CopyReg {
                            src: LirOperand::Place("in".to_owned()),
                            dst: "1".to_owned(),
                            ty: "#bit32".to_owned(),
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
        let expected = "lilac_ir = \"0.2\"\ntarget = \"x86_64-unknown-none\"\nfunction @gen_function__bit32\n  in #bit32 %in\n  out #bit32 %out\n{\n  $entry:\n    &1: #bit32 = copy %in\n    copy &1 -> %out\n    return\n}\n";
        assert_eq!(text, expected);
    }

    fn e070_count(diags: &lilac_error::Diags) -> usize {
        diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E070")
            .count()
    }

    #[test]
    fn verify_accepts_clean() {
        let module = canonical_module();
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_ok());
        assert_eq!(e070_count(&diags), 0);
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

    #[test]
    fn verify_rejects_const_branchif() {
        // `branch.if` on a constant condition must fold first (Task 9).
        let mut module = canonical_module();
        module.funcs[0].blocks[0].insts[1] = LirInst::Const {
            dst: "2".to_owned(),
            lit: "0x1".to_owned(),
            ty: "#bit8".to_owned(),
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

    #[test]
    fn verify_rejects_empty_function() {
        let mut module = canonical_module();
        module.funcs[0].blocks.clear();
        let mut diags = lilac_error::Diags::new();
        assert!(verify(&module, &mut diags).is_err());
        assert_eq!(e070_count(&diags), 1);
    }
}
