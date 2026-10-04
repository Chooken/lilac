//! Constant evaluation + folding on LIR (peephole + `if`-folding in M2,
//! full call-eval with fuel in M3).
//!
//! Foldable v1 set, fuel caps, and the miss-is-ok rule are fixed by plan §10:
//! a missed fold costs performance only, never correctness.

use std::collections::{HashMap, HashSet};

use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_ir::{ArithOp, ConvertOp, LirFunc, LirInst, LirModule, LirOperand, LirTerm};

/// Fuel cap per call-eval (`MAX_FOLD_STEPS`).
pub const MAX_FOLD_STEPS: u64 = 100_000;

/// Recursion cap (`MAX_CALL_DEPTH`).
pub const MAX_CALL_DEPTH: u32 = 16;

/// Constant values (the full lattice lands in M3).
#[derive(Clone, Debug)]
pub enum ConstValue {
    /// Not constant (left as a runtime value).
    Unknown,
    /// Integer constant (widths live on the instructions using it).
    Int(u64),
    /// Boolean constant (comparison results, `@bit8` tests).
    Bool(bool),
}

/// Folds one function at a time: constant arithmetic, constant branch
/// conditions (with dead-block pruning), and dead `@compile_error`
/// removal. Still-live `@compile_error` sites report `E031`.
pub fn fold(module: &mut LirModule, diags: &mut Diags) -> Result<(), ErrorGuaranteed> {
    let mut folder = Folder {
        diags,
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
    failed: bool,
}

/// Parses an integer literal (`0x`, `0b`, decimal, `_` separators).
/// Floats and unparseable text stay `Unknown` (M3 owns them).
fn parse_lit(lit: &str) -> ConstValue {
    let digits = lit.replace('_', "");
    let value = if let Some(hex) = digits.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(hex) = digits.strip_prefix("0X") {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = digits.strip_prefix("0b") {
        u64::from_str_radix(bin, 2).ok()
    } else if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        digits.parse::<u64>().ok()
    } else {
        None
    };
    match value {
        Some(value) => ConstValue::Int(value),
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
        ConstValue::Int(value) => Some(*value != 0),
        ConstValue::Bool(value) => Some(*value),
        ConstValue::Unknown => None,
    }
}

/// Folds one instruction (arithmetic and conversions over constants
/// rewrite to `Const`) and tracks its destination's constness.
fn fold_inst(inst: &mut LirInst, consts: &mut HashMap<String, ConstValue>) {
    match inst {
        LirInst::Const { dst, lit, .. } => {
            consts.insert(dst.clone(), parse_lit(lit));
        }
        LirInst::CopyReg { src, dst, .. } => {
            let value = match src {
                LirOperand::Reg(reg) => consts.get(reg).cloned().unwrap_or(ConstValue::Unknown),
                LirOperand::Place(_) => ConstValue::Unknown,
            };
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
                ConstValue::Int(value) => Some(format!("0x{value:x}")),
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
                ConstValue::Int(value) => Some(format!("0x{value:x}")),
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
        _ => {}
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
        (ConstValue::Int(left), ConstValue::Int(right)) => Some((*left, *right)),
        _ => None,
    };
    match op {
        ArithOp::IAdd | ArithOp::FAdd => match ints {
            Some((l, r)) => ConstValue::Int(l.wrapping_add(r) & mask),
            None => ConstValue::Unknown,
        },
        ArithOp::ISub | ArithOp::FSub => match ints {
            Some((l, r)) => ConstValue::Int(l.wrapping_sub(r) & mask),
            None => ConstValue::Unknown,
        },
        ArithOp::IMul | ArithOp::FMul => match ints {
            Some((l, r)) => ConstValue::Int(l.wrapping_mul(r) & mask),
            None => ConstValue::Unknown,
        },
        ArithOp::IDiv | ArithOp::FDiv => match ints {
            Some((_, 0)) | None => ConstValue::Unknown,
            Some((l, r)) => ConstValue::Int(l.wrapping_div(r) & mask),
        },
        ArithOp::IMod | ArithOp::FMod => match ints {
            Some((_, 0)) | None => ConstValue::Unknown,
            Some((l, r)) => ConstValue::Int(l.wrapping_rem(r) & mask),
        },
        ArithOp::Eq => match (left, right) {
            (ConstValue::Int(l), ConstValue::Int(r)) => ConstValue::Bool(l == r),
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
        (ConvertOp::Trunc, ConstValue::Int(value)) => ConstValue::Int(value & mask),
        (ConvertOp::ExtendZero, ConstValue::Int(value)) => ConstValue::Int(*value),
        _ => ConstValue::Unknown,
    }
}

impl Folder<'_> {
    fn fold_func(&mut self, func: &mut LirFunc) {
        let mut consts: Vec<HashMap<String, ConstValue>> = Vec::new();
        for block in &mut func.blocks {
            let mut map = HashMap::new();
            for inst in &mut block.insts {
                fold_inst(inst, &mut map);
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
    use lilac_ir::{ArithOp, LirBlock, LirFunc, LirInst, LirModule, LirTerm};
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
}
