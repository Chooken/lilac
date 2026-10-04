//! Constant evaluation + folding on LIR (peephole + `if`-folding in M2,
//! full call-eval with fuel in M3).
//!
//! Foldable v1 set, fuel caps, and the miss-is-ok rule are fixed by plan §10:
//! a missed fold costs performance only, never correctness.

use std::collections::{HashMap, HashSet};

use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_ir::{ArithOp, ConvertOp, LirDataKind, LirFunc, LirInst, LirModule, LirOperand, LirTerm};

/// Fuel cap per call-eval (`MAX_FOLD_STEPS`).
pub const MAX_FOLD_STEPS: u64 = 100_000;

/// Recursion cap (`MAX_CALL_DEPTH`).
pub const MAX_CALL_DEPTH: u32 = 16;

/// Constant values (the full lattice lands in M3).
#[derive(Clone, Debug)]
pub enum ConstValue {
    /// Not constant (left as a runtime value).
    Unknown,
    /// Integer constant plus its width in bits.
    Int(u64, u32),
    /// Float constant plus its width in bits.
    Float(f64, u32),
    /// Opaque byte payload (`str`/`numlit` data lowered as `@byte_buffer`).
    Bytes(Vec<u8>),
    /// Boolean constant (comparison results, `@bit8` tests).
    Bool(bool),
}

/// A value error raised while evaluating a fold-required intrinsic,
/// rendered as a user-facing `E060`.
#[derive(Clone, Debug)]
pub struct FoldValueError {
    /// Human-readable reason.
    pub message: String,
}

/// Folds one function at a time: constant arithmetic, constant branch
/// conditions (with dead-block pruning), fold-required intrinsics
/// (`@as_int`/`@as_float`/`@size_of`), and dead `@compile_error` removal.
/// Still-live `@compile_error` sites report `E031`; unfoldable intrinsics
/// report `E060`.
pub fn fold(module: &mut LirModule, diags: &mut Diags) -> Result<(), ErrorGuaranteed> {
    let data: HashMap<String, Vec<u8>> = module
        .data
        .iter()
        .map(|data| {
            let payload = match &data.kind {
                LirDataKind::Str(text) | LirDataKind::NumLit(text) => text.as_bytes().to_vec(),
            };
            (data.name.clone(), payload)
        })
        .collect();
    let mut folder = Folder {
        diags,
        data: &data,
        failed: false,
    };
    for func in &mut module.funcs {
        folder.fold_func(func);
    }
    if folder.failed {
        Err(ErrorGuaranteed::new())
    } else {
        Ok(())
    }
}

struct Folder<'a> {
    diags: &'a mut Diags,
    data: &'a HashMap<String, Vec<u8>>,
    failed: bool,
}

/// Parses integer text (`0x`/`0X`, `0b`/`0B`, decimal, `_` separators and
/// surrounding whitespace).
fn parse_int(text: &str) -> Option<u64> {
    let digits = text.trim().replace('_', "");
    if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        u64::from_str_radix(bin, 2).ok()
    } else if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        digits.parse::<u64>().ok()
    } else {
        None
    }
}

/// Parses an integer literal at width `ty`. Floats and unparseable text
/// stay `Unknown` (the fold-required intrinsics own those errors).
fn parse_lit(lit: &str, ty: &str) -> ConstValue {
    let width = width_of(ty).unwrap_or(64);
    match parse_int(lit) {
        Some(value) => ConstValue::Int(value, width),
        None => ConstValue::Unknown,
    }
}

/// Width in bits for `#bitN` types (masking truncated results).
fn width_of(ty: &str) -> Option<u32> {
    match ty.strip_prefix("ref ").unwrap_or(ty) {
        "#bit8" => Some(8),
        "#bit16" => Some(16),
        "#bit32" => Some(32),
        "#bit64" | "#bitNative" => Some(64),
        _ => None,
    }
}

/// Truthiness: nonzero integers and true booleans.
fn is_truthy(value: &ConstValue) -> Option<bool> {
    match value {
        ConstValue::Int(value, _) => Some(*value != 0),
        ConstValue::Bool(value) => Some(*value),
        ConstValue::Float(_, _) | ConstValue::Bytes(_) | ConstValue::Unknown => None,
    }
}

/// Looks up an operand's tracked constant (`Unknown` for places or
/// untracked registers).
fn operand_const(consts: &HashMap<String, ConstValue>, operand: &LirOperand) -> ConstValue {
    match operand {
        LirOperand::Reg(reg) => consts.get(reg).cloned().unwrap_or(ConstValue::Unknown),
        LirOperand::Place(_) => ConstValue::Unknown,
    }
}

/// Evaluates a fold-required intrinsic over constant `args`. `width` is
/// the result bit width (from the instruction's type).
///
/// # Errors
///
/// Returns a [`FoldValueError`] (rendered as `E060`) when the operand is
/// not the expected kind, contains bad digits, or overflows `width`.
pub fn eval_intrinsic(
    name: &str,
    args: &[ConstValue],
    width: u32,
) -> Result<ConstValue, FoldValueError> {
    match name {
        "@as_int" => eval_as_int(args, width),
        "@as_float" => eval_as_float(args, width),
        "@size_of" => eval_size_of(args),
        _ => Err(FoldValueError {
            message: format!("`{name}` is not a fold-required intrinsic"),
        }),
    }
}

fn eval_as_int(args: &[ConstValue], width: u32) -> Result<ConstValue, FoldValueError> {
    let text = match args.first() {
        Some(ConstValue::Bytes(bytes)) => {
            std::str::from_utf8(bytes).map_err(|_| FoldValueError {
                message: "`@as_int` operand is not valid UTF-8".to_owned(),
            })?
        }
        _ => {
            return Err(FoldValueError {
                message: "`@as_int` requires a constant `@byte_buffer` operand".to_owned(),
            })
        }
    };
    let value = parse_int(text).ok_or_else(|| FoldValueError {
        message: format!("`{text}` is not a valid integer literal"),
    })?;
    let max = if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    if value > max {
        return Err(FoldValueError {
            message: format!("value does not fit in `#bit{width}` (max 0x{max:x})"),
        });
    }
    Ok(ConstValue::Int(value, width))
}

fn eval_as_float(args: &[ConstValue], width: u32) -> Result<ConstValue, FoldValueError> {
    let text = match args.first() {
        Some(ConstValue::Bytes(bytes)) => {
            std::str::from_utf8(bytes).map_err(|_| FoldValueError {
                message: "`@as_float` operand is not valid UTF-8".to_owned(),
            })?
        }
        _ => {
            return Err(FoldValueError {
                message: "`@as_float` requires a constant `@byte_buffer` operand".to_owned(),
            })
        }
    };
    let value = text.trim().parse::<f64>().map_err(|_| FoldValueError {
        message: format!("`{text}` is not a valid float literal"),
    })?;
    Ok(ConstValue::Float(value, width))
}

/// Best-effort `@size_of`: its operand-type semantics are not pinned in
/// M3 (ledger ruling R3), so a typed integer constant yields its byte
/// count and anything else is `E060`.
fn eval_size_of(args: &[ConstValue]) -> Result<ConstValue, FoldValueError> {
    match args.first() {
        Some(ConstValue::Int(_, bits)) => Ok(ConstValue::Int(u64::from(bits / 8), 64)),
        _ => Err(FoldValueError {
            message: "`@size_of` requires a constant typed operand".to_owned(),
        }),
    }
}

/// Evaluates arithmetic over constants (misses stay `Unknown`: a missed
/// fold is performance only, never a correctness error).
fn eval_arith(op: &ArithOp, left: &ConstValue, right: &ConstValue, ty: &str) -> ConstValue {
    let width = width_of(ty).unwrap_or(64);
    let mask = if width >= 64 {
        u64::MAX
    } else {
        (1 << width) - 1
    };
    let ints = match (left, right) {
        (ConstValue::Int(left, _), ConstValue::Int(right, _)) => Some((*left, *right)),
        _ => None,
    };
    match op {
        ArithOp::IAdd | ArithOp::FAdd => match ints {
            Some((l, r)) => ConstValue::Int(l.wrapping_add(r) & mask, width),
            None => ConstValue::Unknown,
        },
        ArithOp::ISub | ArithOp::FSub => match ints {
            Some((l, r)) => ConstValue::Int(l.wrapping_sub(r) & mask, width),
            None => ConstValue::Unknown,
        },
        ArithOp::IMul | ArithOp::FMul => match ints {
            Some((l, r)) => ConstValue::Int(l.wrapping_mul(r) & mask, width),
            None => ConstValue::Unknown,
        },
        ArithOp::IDiv | ArithOp::FDiv => match ints {
            Some((_, 0)) | None => ConstValue::Unknown,
            Some((l, r)) => ConstValue::Int(l.wrapping_div(r) & mask, width),
        },
        ArithOp::IMod | ArithOp::FMod => match ints {
            Some((_, 0)) | None => ConstValue::Unknown,
            Some((l, r)) => ConstValue::Int(l.wrapping_rem(r) & mask, width),
        },
        ArithOp::Eq => match (left, right) {
            (ConstValue::Int(l, _), ConstValue::Int(r, _)) => ConstValue::Bool(l == r),
            (ConstValue::Bool(l), ConstValue::Bool(r)) => ConstValue::Bool(l == r),
            _ => ConstValue::Unknown,
        },
        ArithOp::ILt | ArithOp::FLt => match ints {
            Some((l, r)) => ConstValue::Bool(l < r),
            None => ConstValue::Unknown,
        },
        ArithOp::IGt | ArithOp::FGt => match ints {
            Some((l, r)) => ConstValue::Bool(l > r),
            None => ConstValue::Unknown,
        },
    }
}

/// Evaluates width conversions over constants.
fn eval_convert(op: &ConvertOp, value: &ConstValue, ty: &str) -> ConstValue {
    let width = width_of(ty).unwrap_or(64);
    let mask = if width >= 64 {
        u64::MAX
    } else {
        (1 << width) - 1
    };
    match (op, value) {
        (ConvertOp::Trunc, ConstValue::Int(value, _)) => ConstValue::Int(value & mask, width),
        (ConvertOp::ExtendZero, ConstValue::Int(value, _)) => ConstValue::Int(*value, width),
        _ => ConstValue::Unknown,
    }
}

impl Folder<'_> {
    /// Folds one instruction (arithmetic, conversions, and fold-required
    /// intrinsics over constants rewrite to `Const`) and tracks its
    /// destination's constness.
    fn fold_inst(&mut self, inst: &mut LirInst, consts: &mut HashMap<String, ConstValue>) {
        match inst {
            LirInst::Const { dst, lit, ty, .. } => {
                let value = match lit.strip_prefix('!') {
                    Some(name) => match self.data.get(name) {
                        Some(bytes) => ConstValue::Bytes(bytes.clone()),
                        None => ConstValue::Unknown,
                    },
                    None => parse_lit(lit, ty),
                };
                consts.insert(dst.clone(), value);
            }
            LirInst::CopyReg { src, dst, .. } => {
                let value = operand_const(consts, src);
                consts.insert(dst.clone(), value);
            }
            LirInst::Arith {
                op,
                lhs,
                rhs,
                dst,
                ty,
                span,
            } => {
                let left = consts.get(lhs).cloned().unwrap_or(ConstValue::Unknown);
                let right = consts.get(rhs).cloned().unwrap_or(ConstValue::Unknown);
                let folded = eval_arith(op, &left, &right, ty);
                let replacement = match &folded {
                    ConstValue::Int(value, _) => Some(format!("0x{value:x}")),
                    _ => None,
                };
                let (dst_name, ty_name, span_val) = (dst.clone(), ty.clone(), *span);
                if let Some(lit) = replacement {
                    *inst = LirInst::Const {
                        dst: dst_name.clone(),
                        lit,
                        ty: ty_name,
                        span: span_val,
                    };
                }
                consts.insert(dst_name, folded);
            }
            LirInst::Convert {
                op,
                src,
                dst,
                ty,
                span,
            } => {
                let value = consts.get(src).cloned().unwrap_or(ConstValue::Unknown);
                let folded = eval_convert(op, &value, ty);
                let replacement = match &folded {
                    ConstValue::Int(value, _) => Some(format!("0x{value:x}")),
                    _ => None,
                };
                let (dst_name, ty_name, span_val) = (dst.clone(), ty.clone(), *span);
                if let Some(lit) = replacement {
                    *inst = LirInst::Const {
                        dst: dst_name.clone(),
                        lit,
                        ty: ty_name,
                        span: span_val,
                    };
                }
                consts.insert(dst_name, folded);
            }
            LirInst::FoldIntrinsic { .. } => {
                self.fold_intrinsic(inst, consts);
            }
            _ => {}
        }
    }

    /// Folds a fold-required intrinsic: a known operand is evaluated and
    /// the marker rewritten to `Const`; an unknown operand or an
    /// evaluation error reports `E060` and poisons the destination.
    fn fold_intrinsic(&mut self, inst: &mut LirInst, consts: &mut HashMap<String, ConstValue>) {
        let (name, arg, dst, ty, span) = match inst {
            LirInst::FoldIntrinsic {
                name,
                arg,
                dst,
                ty,
                span,
            } => (name.clone(), arg.clone(), dst.clone(), ty.clone(), *span),
            _ => return,
        };
        let width = width_of(&ty).unwrap_or(64);
        let value = operand_const(consts, &arg);
        let result = if matches!(value, ConstValue::Unknown) {
            Err(FoldValueError {
                message: format!("`{name}` requires a compile-time-constant operand"),
            })
        } else {
            eval_intrinsic(&name, &[value], width)
        };
        match result {
            Ok(ConstValue::Int(value, bits)) => {
                *inst = LirInst::Const {
                    dst: dst.clone(),
                    lit: format!("0x{value:x}"),
                    ty,
                    span,
                };
                consts.insert(dst, ConstValue::Int(value, bits));
            }
            Ok(ConstValue::Float(value, bits)) => {
                *inst = LirInst::Const {
                    dst: dst.clone(),
                    lit: format!("{value:?}"),
                    ty,
                    span,
                };
                consts.insert(dst, ConstValue::Float(value, bits));
            }
            Ok(value) => {
                consts.insert(dst, value);
            }
            Err(error) => {
                self.failed = true;
                self.diags
                    .emit(Diagnostic::error("E060", error.message).label(
                        span,
                        true,
                        "evaluated here".to_owned(),
                    ));
                consts.insert(dst, ConstValue::Unknown);
            }
        }
    }

    fn fold_func(&mut self, func: &mut LirFunc) {
        let mut consts: Vec<HashMap<String, ConstValue>> = Vec::new();
        for block in &mut func.blocks {
            let mut map = HashMap::new();
            for inst in &mut block.insts {
                self.fold_inst(inst, &mut map);
            }
            consts.push(map);
        }
        for (block, map) in func.blocks.iter_mut().zip(consts.iter()) {
            if let LirTerm::BranchIf {
                cond,
                then,
                els,
                span,
            } = &block.term
            {
                if let Some(live) = map.get(cond).and_then(|value| {
                    is_truthy(value).map(|truthy| {
                        if truthy {
                            then.clone()
                        } else {
                            els.clone().unwrap_or_else(|| then.clone())
                        }
                    })
                }) {
                    block.term = LirTerm::Branch {
                        target: live,
                        span: *span,
                    };
                }
            }
        }
        Self::prune_dead(func);
        self.check_live_errors(func);
    }

    /// Deletes unreachable non-entry blocks to a fixpoint (dead
    /// `@compile_error` sites vanish with them, silently).
    fn prune_dead(func: &mut LirFunc) {
        loop {
            let mut referenced: HashSet<String> = HashSet::new();
            for block in &func.blocks {
                match &block.term {
                    LirTerm::Branch { target, .. } => {
                        referenced.insert(target.clone());
                    }
                    LirTerm::BranchIf { then, els, .. } => {
                        referenced.insert(then.clone());
                        if let Some(els) = els {
                            referenced.insert(els.clone());
                        }
                    }
                    LirTerm::Return { .. }
                    | LirTerm::Panic { .. }
                    | LirTerm::Unreachable { .. } => {}
                }
            }
            let entry = func.blocks[0].label.clone();
            let before = func.blocks.len();
            func.blocks
                .retain(|block| block.label == entry || referenced.contains(&block.label));
            if func.blocks.len() == before {
                break;
            }
        }
    }

    /// Reports still-live `@compile_error` sites as `E031` (both spans:
    /// the message literal and the call).
    fn check_live_errors(&mut self, func: &LirFunc) {
        for block in &func.blocks {
            for inst in &block.insts {
                if let LirInst::CompileError {
                    msg,
                    msg_span,
                    call_span,
                } = inst
                {
                    self.failed = true;
                    self.diags.emit(
                        Diagnostic::error("E031", msg.clone())
                            .label(*msg_span, true, "message here".to_owned())
                            .label(*call_span, false, "evaluated here".to_owned()),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_error::Diags;
    use lilac_ir::{
        ArithOp, LirBlock, LirData, LirDataKind, LirFunc, LirInst, LirModule, LirOperand, LirTerm,
    };
    use lilac_span::Span;

    fn test_span() -> Span {
        Span::dummy()
    }

    fn arith_module() -> LirModule {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![
                    LirInst::Const {
                        dst: "1".to_owned(),
                        lit: "0x1".to_owned(),
                        ty: "#bit32".to_owned(),
                        span: test_span(),
                    },
                    LirInst::Const {
                        dst: "2".to_owned(),
                        lit: "0x2".to_owned(),
                        ty: "#bit32".to_owned(),
                        span: test_span(),
                    },
                    LirInst::Arith {
                        op: ArithOp::IAdd,
                        lhs: "1".to_owned(),
                        rhs: "2".to_owned(),
                        dst: "3".to_owned(),
                        ty: "#bit32".to_owned(),
                        span: test_span(),
                    },
                ],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        });
        module
    }

    #[test]
    fn folds_const_arith() {
        let mut module = arith_module();
        let mut diags = Diags::new();
        assert!(fold(&mut module, &mut diags).is_ok());
        assert!(!diags.has_errors(), "{diags:?}");
        let text = module.to_text();
        assert!(!text.contains("i.add"), "{text}");
        assert!(text.contains("0x3"), "{text}");
    }

    fn branch_module(live: bool) -> LirModule {
        // The `@compile_error` sits in the `then` branch: taken when
        // `live`, pruned away (silently) when not.
        let cond_lit = if live { "0x1" } else { "0x0" };
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            blocks: vec![
                LirBlock {
                    label: "entry".to_owned(),
                    insts: vec![LirInst::Const {
                        dst: "1".to_owned(),
                        lit: cond_lit.to_owned(),
                        ty: "#bit8".to_owned(),
                        span: test_span(),
                    }],
                    term: LirTerm::BranchIf {
                        cond: "1".to_owned(),
                        then: "then".to_owned(),
                        els: Some("els".to_owned()),
                        span: test_span(),
                    },
                    span: test_span(),
                },
                LirBlock {
                    label: "then".to_owned(),
                    insts: vec![LirInst::CompileError {
                        msg: "boom".to_owned(),
                        msg_span: test_span(),
                        call_span: test_span(),
                    }],
                    term: LirTerm::Return { span: test_span() },
                    span: test_span(),
                },
                LirBlock {
                    label: "els".to_owned(),
                    insts: Vec::new(),
                    term: LirTerm::Return { span: test_span() },
                    span: test_span(),
                },
            ],
        });
        module
    }

    #[test]
    fn if_fold_prunes_dead() {
        let mut module = branch_module(false);
        let mut diags = Diags::new();
        assert!(fold(&mut module, &mut diags).is_ok());
        assert!(!diags.has_errors(), "{diags:?}");
        let labels: Vec<&str> = module.funcs[0]
            .blocks
            .iter()
            .map(|block| block.label.as_str())
            .collect();
        assert!(!labels.contains(&"then"), "{labels:?}");
        assert!(module.to_text().contains("branch $els"), "{labels:?}");
    }

    #[test]
    fn dead_compile_error_silent() {
        // Mirrors `cfold_dead_compile_error.lilac`: the error dies with its
        // branch and never reports.
        let mut module = branch_module(false);
        let mut diags = Diags::new();
        assert!(fold(&mut module, &mut diags).is_ok());
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn live_compile_error_is_e031() {
        let mut module = branch_module(true);
        let mut diags = Diags::new();
        assert!(fold(&mut module, &mut diags).is_err());
        let errors: Vec<_> = diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E031")
            .collect();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].labels.len(), 2, "{diags:?}");
    }

    fn e060_count(diags: &Diags) -> usize {
        diags
            .diagnostics()
            .iter()
            .filter(|diag| diag.code == "E060")
            .count()
    }

    /// A function with `const !c0: numlit` followed by `name` folded
    /// from its buffer at result type `ty`.
    fn intrinsic_module(name: &str, payload: &str, ty: &str) -> LirModule {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.data.push(LirData {
            name: "c0".to_owned(),
            kind: LirDataKind::NumLit(payload.to_owned()),
            span: test_span(),
        });
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![
                    LirInst::Const {
                        dst: "1".to_owned(),
                        lit: "!c0".to_owned(),
                        ty: "#byte_buffer".to_owned(),
                        span: test_span(),
                    },
                    LirInst::FoldIntrinsic {
                        name: name.to_owned(),
                        arg: LirOperand::Reg("1".to_owned()),
                        dst: "2".to_owned(),
                        ty: ty.to_owned(),
                        span: test_span(),
                    },
                ],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        });
        module
    }

    /// A function with a fold intrinsic over an untracked (non-constant)
    /// register.
    fn nonconstant_intrinsic_module() -> LirModule {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![LirInst::FoldIntrinsic {
                    name: "@as_int".to_owned(),
                    arg: LirOperand::Reg("7".to_owned()),
                    dst: "2".to_owned(),
                    ty: "#bit8".to_owned(),
                    span: test_span(),
                }],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        });
        module
    }

    #[test]
    fn as_int_folds_to_const() {
        let mut module = intrinsic_module("@as_int", "5", "#bit8");
        let mut diags = Diags::new();
        assert!(fold(&mut module, &mut diags).is_ok(), "{diags:?}");
        assert!(!diags.has_errors(), "{diags:?}");
        let text = module.to_text();
        assert!(text.contains("const 0x5"), "{text}");
        assert!(text.contains("#bit8"), "{text}");
        assert!(!text.contains("@as_int"), "{text}");
    }

    #[test]
    fn as_int_bad_digits_is_e060() {
        let mut module = intrinsic_module("@as_int", "hello", "#bit8");
        let mut diags = Diags::new();
        assert!(fold(&mut module, &mut diags).is_err(), "{diags:?}");
        assert_eq!(e060_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn as_int_overflow_is_e060() {
        let mut module = intrinsic_module("@as_int", "0x1FF", "#bit8");
        let mut diags = Diags::new();
        assert!(fold(&mut module, &mut diags).is_err(), "{diags:?}");
        assert_eq!(e060_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn as_int_nonconstant_is_e060() {
        let mut module = nonconstant_intrinsic_module();
        let mut diags = Diags::new();
        assert!(fold(&mut module, &mut diags).is_err(), "{diags:?}");
        assert_eq!(e060_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn as_float_folds_to_const() {
        let mut module = intrinsic_module("@as_float", "5.25", "#bit64");
        let mut diags = Diags::new();
        assert!(fold(&mut module, &mut diags).is_ok(), "{diags:?}");
        let text = module.to_text();
        assert!(text.contains("const 5.25"), "{text}");
        assert!(!text.contains("@as_float"), "{text}");
    }

    #[test]
    fn size_of_folds_to_byte_count() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![
                    LirInst::Const {
                        dst: "1".to_owned(),
                        lit: "0x0".to_owned(),
                        ty: "#bit16".to_owned(),
                        span: test_span(),
                    },
                    LirInst::FoldIntrinsic {
                        name: "@size_of".to_owned(),
                        arg: LirOperand::Reg("1".to_owned()),
                        dst: "2".to_owned(),
                        ty: "#bitNative".to_owned(),
                        span: test_span(),
                    },
                ],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        });
        let mut diags = Diags::new();
        assert!(fold(&mut module, &mut diags).is_ok(), "{diags:?}");
        let text = module.to_text();
        assert!(text.contains("const 0x2"), "{text}");
        assert!(!text.contains("@size_of"), "{text}");
    }
}
