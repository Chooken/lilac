//! The printer: a module as canonical LIR text, optionally with the
//! source span of each line.

use std::fmt::Write as _;

use lilac_span::Span;

use crate::{
    ArithOp, ConvertOp, LirCallee, LirData, LirFunc, LirGlobal, LirInst, LirModule, LirOperand,
    LirTerm, LirTy, SpannedText,
};

impl LirModule {
    /// Canonical header text: the two header lines, neither source-spanned.
    #[must_use]
    pub fn header_text(&self) -> String {
        join_lines(&self.header_lines())
    }

    /// The two compiler-generated header lines (`lilac_ir`, `target`).
    pub(crate) fn header_lines(&self) -> Vec<(String, Option<Span>)> {
        vec![
            (format!("lilac_ir = \"{}\"", self.version), None),
            (format!("target = \"{}\"", self.target), None),
        ]
    }

    /// Every physical line of the module text, paired with its source span
    /// (`None` for compiler-generated lines). The single traversal shared by
    /// [`Self::to_text`] and [`Self::to_text_spanned`].
    pub(crate) fn module_lines(&self) -> Vec<(String, Option<Span>)> {
        let mut lines = self.header_lines();
        for data in &self.data {
            lines.extend(print_data(data));
        }
        for global in &self.globals {
            lines.push(print_global(global));
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
pub(crate) fn physical_lines(lines: Vec<(String, Option<Span>)>) -> Vec<(String, Option<Span>)> {
    let mut out = Vec::with_capacity(lines.len());
    for (text, span) in lines {
        for part in text.split('\n') {
            out.push((part.to_owned(), span));
        }
    }
    out
}

/// Joins `(line, span)` pairs with `"\n"` and appends a trailing newline.
pub(crate) fn join_lines(lines: &[(String, Option<Span>)]) -> String {
    let mut text = lines
        .iter()
        .map(|(line, _)| line.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    text
}

/// Escapes data bytes for the text form: `"` and `\` are backslashed, other
/// printable ASCII and newlines print as-is, and every other byte is `\xHH`.
pub(crate) fn escape_data(bytes: &[u8]) -> String {
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

pub(crate) fn print_data(data: &LirData) -> Vec<(String, Option<Span>)> {
    vec![(
        format!(
            "!{}: str = \"{}\"",
            data.name.as_str(),
            escape_data(&data.bytes)
        ),
        Some(data.span),
    )]
}

/// `global !name: #t = zeroed`, or `= bytes "…"` with its initial bytes.
pub(crate) fn print_global(global: &LirGlobal) -> (String, Option<Span>) {
    let init = match &global.init {
        None => "zeroed".to_owned(),
        Some(bytes) => format!("bytes \"{}\"", escape_data(bytes)),
    };
    (
        format!("global !{}: {} = {init}", global.name.as_str(), global.ty),
        Some(global.span),
    )
}

/// Prints one type section (`type #Name` with `name: #ty` members in
/// layout order; the section order itself is sorted in `to_text`). Type
/// sections are compiler-generated, so every line carries no span.
pub(crate) fn print_layout(name: &str, members: &[(String, LirTy)]) -> Vec<(String, Option<Span>)> {
    let mut lines = vec![(format!("type #{name} {{"), None)];
    for (member, ty) in members {
        lines.push((format!("    {member}: {ty},"), None));
    }
    lines.push(("}".to_owned(), None));
    lines
}

pub(crate) fn print_operand(operand: &LirOperand) -> String {
    match operand {
        LirOperand::Reg(reg) => reg.to_string(),
        LirOperand::Place(place) => place.to_string(),
    }
}

pub(crate) fn print_callee(callee: &LirCallee) -> String {
    match callee {
        LirCallee::Reg(reg) => reg.to_string(),
        LirCallee::Func(func) => func.to_string(),
        LirCallee::Builtin(name) => name.clone(),
    }
}

pub(crate) fn print_op(op: &ArithOp) -> &'static str {
    match op {
        ArithOp::IAdd => "i.add",
        ArithOp::ISub => "i.sub",
        ArithOp::IMul => "i.mul",
        ArithOp::IDiv => "i.div",
        ArithOp::IDivU => "i.div.u",
        ArithOp::IMod => "i.mod",
        ArithOp::IModU => "i.mod.u",
        ArithOp::And => "and",
        ArithOp::Or => "or",
        ArithOp::Xor => "xor",
        ArithOp::Shl => "shift.l",
        ArithOp::Shr => "shift.r",
        ArithOp::ShrU => "shift.r.u",
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

pub(crate) fn print_inst(inst: &LirInst) -> String {
    match inst {
        LirInst::CopyReg { src, dst, ty, .. } => {
            format!("{dst}: {ty} = copy {}", print_operand(src))
        }
        LirInst::CopyPlace { src, dst, .. } => {
            format!("copy {} -> {dst}", print_operand(src))
        }
        LirInst::Const { dst, value, ty, .. } => format!("{dst}: {ty} = const {value}"),
        LirInst::Load { src, dst, ty, .. } => format!("{dst}: {ty} = load {src}"),
        LirInst::Addr { place, dst, ty, .. } => {
            format!("{dst}: {ty} = addr {place}")
        }
        LirInst::AddrFunc { func, dst, ty, .. } => {
            format!("{dst}: {ty} = addr.func {func}")
        }
        LirInst::AddrGlobal {
            global, dst, ty, ..
        } => {
            format!("{dst}: {ty} = addr.global !{}", global.as_str())
        }
        LirInst::Call {
            func, ins, outs, ..
        } => {
            let ins = if ins.is_empty() {
                "(in)".to_owned()
            } else {
                let places = ins
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("(in {places})")
            };
            let outs = if outs.is_empty() {
                "(out)".to_owned()
            } else {
                let places = outs
                    .iter()
                    .map(ToString::to_string)
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
        } => format!("{dst}: {ty} = field {obj} .{name}  // idx {idx}"),
        LirInst::StoreField {
            value,
            obj,
            name,
            idx,
            ..
        } => format!(
            "copy {} -> {obj}.{name}  // idx {idx}",
            print_operand(value)
        ),
        LirInst::Store { src, dst, .. } => {
            format!("copy {} -> ref {dst}", print_operand(src))
        }
        LirInst::Convert {
            op, src, dst, ty, ..
        } => {
            let op = match op {
                ConvertOp::Trunc => "trunc",
                ConvertOp::ExtendZero => "extend.zero",
                ConvertOp::ExtendSign => "extend.sign",
                ConvertOp::Bitcast => "bitcast",
            };
            format!("{dst}: {ty} = {op} {src}")
        }
        LirInst::FieldAddr { .. } | LirInst::ElemAddr { .. } => print_addr(inst),
        LirInst::Alloca { ty, place, .. } => format!("alloca {ty} {place} : zeroed"),
        LirInst::Drop { place, .. } => format!("drop {place}"),
        LirInst::Arith {
            op,
            lhs,
            rhs,
            dst,
            ty,
            ..
        } => format!("{dst}: {ty} = {} {lhs}, {rhs}", print_op(op)),
        LirInst::CompileError { .. } => "@compile_error(live)".to_owned(),
        LirInst::Zero { place, .. } => format!("zero {place}"),
        LirInst::AtomicLoad { .. }
        | LirInst::AtomicStore { .. }
        | LirInst::AtomicRmw { .. }
        | LirInst::CmpXchg { .. }
        | LirInst::Fence { .. } => print_atomic(inst),
    }
}

/// Prints `field.addr` or `elem.addr`.
pub(crate) fn print_addr(inst: &LirInst) -> String {
    match inst {
        LirInst::FieldAddr {
            obj,
            name,
            idx,
            dst,
            ty,
            ..
        } => format!("{dst}: {ty} = field.addr {obj} .{name}  // idx {idx}"),
        LirInst::ElemAddr {
            array,
            index,
            dst,
            ty,
            ..
        } => format!("{dst}: {ty} = elem.addr {array}, {index}"),
        _ => lilac_error::bug!(Span::dummy(), "non-address instruction in print_addr"),
    }
}

/// Prints an atomic instruction.
pub(crate) fn print_atomic(inst: &LirInst) -> String {
    match inst {
        LirInst::AtomicLoad {
            ptr,
            dst,
            ty,
            order,
            ..
        } => format!("{dst}: {ty} = atomic.load.{} {ptr}", order.name()),
        LirInst::AtomicStore {
            value,
            ptr,
            ty,
            order,
            ..
        } => format!("atomic.store.{} {ty} {value} -> {ptr}", order.name()),
        LirInst::AtomicRmw {
            op,
            ptr,
            value,
            dst,
            ty,
            order,
            ..
        } => format!(
            "{dst}: {ty} = atomic.{}.{} {ptr}, {value}",
            op.name(),
            order.name()
        ),
        LirInst::CmpXchg {
            ptr,
            expected,
            desired,
            dst,
            ty,
            order,
            ..
        } => format!(
            "{dst}: {ty} = atomic.cmpxchg.{} {ptr}, {expected}, {desired}",
            order.name()
        ),
        LirInst::Fence { order, .. } => format!("fence.{}", order.name()),
        _ => lilac_error::bug!(Span::dummy(), "non-atomic instruction in print_atomic"),
    }
}

pub(crate) fn print_term(term: &LirTerm) -> String {
    match term {
        LirTerm::Return { .. } => "return".to_owned(),
        LirTerm::Branch { target, .. } => format!("branch {target}"),
        LirTerm::BranchIf {
            cond, then, els, ..
        } => match els {
            Some(els) => format!("branch.if {cond}, {then}, {els}"),
            None => format!("branch.if {cond}, {then}"),
        },
        LirTerm::Unreachable { .. } => "unreachable".to_owned(),
    }
}

/// The source span of an instruction: its `span` field, except
/// `CompileError`, which only records the `@compile_error` call site.
pub(crate) fn inst_span(inst: &LirInst) -> Span {
    match inst {
        LirInst::CopyReg { span, .. }
        | LirInst::CopyPlace { span, .. }
        | LirInst::Const { span, .. }
        | LirInst::Load { span, .. }
        | LirInst::Addr { span, .. }
        | LirInst::AddrFunc { span, .. }
        | LirInst::AddrGlobal { span, .. }
        | LirInst::Call { span, .. }
        | LirInst::Field { span, .. }
        | LirInst::StoreField { span, .. }
        | LirInst::Store { span, .. }
        | LirInst::Convert { span, .. }
        | LirInst::FieldAddr { span, .. }
        | LirInst::ElemAddr { span, .. }
        | LirInst::Alloca { span, .. }
        | LirInst::Drop { span, .. }
        | LirInst::Arith { span, .. }
        | LirInst::Zero { span, .. }
        | LirInst::AtomicLoad { span, .. }
        | LirInst::AtomicStore { span, .. }
        | LirInst::AtomicRmw { span, .. }
        | LirInst::CmpXchg { span, .. }
        | LirInst::Fence { span, .. } => *span,
        LirInst::CompileError { call_span, .. } => *call_span,
    }
}

/// The source span of a terminator (every variant carries one).
pub(crate) fn term_span(term: &LirTerm) -> Span {
    match term {
        LirTerm::Return { span, .. }
        | LirTerm::Branch { span, .. }
        | LirTerm::BranchIf { span, .. }
        | LirTerm::Unreachable { span, .. } => *span,
    }
}

pub(crate) fn print_func(func: &LirFunc) -> Vec<(String, Option<Span>)> {
    let header = if func.is_extern {
        format!("extern {}", func.name)
    } else if func.inline {
        format!("inline function {}", func.name)
    } else {
        format!("function {}", func.name)
    };
    let mut lines = vec![(header, None)];
    for (place, ty) in &func.ins {
        lines.push((format!("  in {ty} {place}"), None));
    }
    if func.variadic {
        lines.push(("  in ...".to_owned(), None));
    }
    for (place, ty) in &func.outs {
        lines.push((format!("  out {ty} {place}"), None));
    }
    if func.is_extern {
        return lines;
    }
    lines.push(("{".to_owned(), None));
    for block in &func.blocks {
        lines.push((format!("  {}:", block.label), Some(block.span)));
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
