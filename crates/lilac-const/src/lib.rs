//! Constant evaluation and folding on LIR: constant arithmetic, constant
//! branches (with dead-block pruning, which silences dead `@compile_error`
//! sites), and best-effort evaluation of calls with constant arguments.
//!
//! A missed fold costs performance only, never correctness. A place whose
//! address is taken is never treated as constant (writes through the
//! reference are invisible to the dataflow).

use std::collections::{HashMap, HashSet};

use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_ir::{
    builtin_sig, ArithOp, ConvertOp, LirBlock, LirCallee, LirFunc, LirInst, LirModule, LirOperand,
    LirTerm, LirTy,
};
use lilac_span::Span;

/// Fuel cap per call-eval (`MAX_FOLD_STEPS`).
pub const MAX_FOLD_STEPS: u64 = 100_000;

/// Recursion cap (`MAX_CALL_DEPTH`).
pub const MAX_CALL_DEPTH: u32 = 16;

/// Constant values.
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

/// Why a builtin could not be evaluated (a fold miss, never a user error).
#[derive(Clone, Debug)]
pub struct FoldValueError {
    /// Human-readable reason.
    pub message: String,
}

/// Folds every function: constant arithmetic, constant branch conditions
/// (with dead-block pruning, which removes dead `@compile_error` sites), and
/// bounded call evaluation: a `call` whose `in` places are all constant is
/// interpreted under [`MAX_FOLD_STEPS`]/[`MAX_CALL_DEPTH`]; a miss leaves the
/// runtime call in place. Live `@compile_error` sites are reported later by
/// [`check_live_compile_errors`], which takes the returned [`FoldReport`].
pub fn fold(module: &mut LirModule) -> FoldReport {
    fold_with_fuel(module, MAX_FOLD_STEPS, MAX_CALL_DEPTH)
}

/// What folding could not decide.
#[derive(Clone, Debug, Default)]
pub struct FoldReport {
    /// Functions where a call evaluation ran out of steps or depth, so a
    /// branch it guards may have stayed live.
    pub exhausted: HashSet<String>,
}

/// Folding with an injectable fuel/depth budget (tests exercise exhaustion).
fn fold_with_fuel(module: &mut LirModule, fuel: u64, depth: u32) -> FoldReport {
    let data: HashMap<String, Vec<u8>> = module
        .data
        .iter()
        .map(|data| (data.name.clone(), data.bytes.clone()))
        .collect();
    let funcs = module.funcs.clone();
    let mut folder = Folder {
        data: &data,
        funcs: &funcs,
        fuel,
        max_depth: depth,
        fresh: 0,
        escaped: HashSet::new(),
        exhausted: false,
    };
    let mut report = FoldReport::default();
    for func in &mut module.funcs {
        folder.exhausted = false;
        folder.fold_func(func);
        if folder.exhausted {
            report.exhausted.insert(func.name.clone());
        }
    }
    report
}

/// Reports `E031` for every still-live `@compile_error` in `module`.
///
/// Call this **after** reachability pruning
/// ([`lilac_ir::prune_unreachable`]) so that errors in functions unreachable
/// from `main` — uncalled functions and unused hooks — stay silent. Each site
/// labels both the message literal and the call.
///
/// # Errors
///
/// Returns [`ErrorGuaranteed`] when at least one live site was reported.
pub fn check_live_compile_errors(
    module: &LirModule,
    report: &FoldReport,
    diags: &mut Diags,
) -> Result<(), ErrorGuaranteed> {
    let mut failed = false;
    for func in &module.funcs {
        for block in &func.blocks {
            for inst in &block.insts {
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
                    if report.exhausted.contains(&func.name) {
                        diagnostic = diagnostic.note(
                            "constant folding hit its step/depth limit in this function, \
                             so the condition guarding this call may be undecided",
                        );
                    }
                    diags.emit(diagnostic);
                }
            }
        }
    }
    if failed {
        Err(ErrorGuaranteed::new())
    } else {
        Ok(())
    }
}

struct Folder<'a> {
    data: &'a HashMap<String, Vec<u8>>,
    funcs: &'a [LirFunc],
    fuel: u64,
    max_depth: u32,
    fresh: u64,
    /// Places of the current function whose address is taken.
    escaped: HashSet<String>,
    /// Set when a call evaluation in the current function ran out of fuel or
    /// depth.
    exhausted: bool,
}

/// Places whose address is taken (`addr`, `field.addr`): writes through the
/// resulting reference are invisible to the dataflow.
fn escaped_places(func: &LirFunc) -> HashSet<String> {
    let mut escaped = HashSet::new();
    for block in &func.blocks {
        for inst in &block.insts {
            match inst {
                LirInst::Addr { place, .. } => {
                    escaped.insert(place.clone());
                }
                LirInst::FieldAddr { obj, .. } => {
                    escaped.insert(obj.clone());
                }
                _ => {}
            }
        }
    }
    escaped
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
fn parse_lit(lit: &str, ty: &LirTy) -> ConstValue {
    let width = width_of(ty).unwrap_or(64);
    match parse_int(lit) {
        Some(value) => ConstValue::Int(value, width),
        None => ConstValue::Unknown,
    }
}

/// Width in bits of a scalar (masking truncated results, sizing float
/// constants); a `ref` gives its pointee's width. `#bit_native` folds at 64.
fn width_of(ty: &LirTy) -> Option<u32> {
    match ty.strip_ref() {
        LirTy::Ref(_) => None,
        scalar => scalar.bits(64),
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

/// Whether a value is the uninformative top of the lattice.
fn is_unknown(value: &ConstValue) -> bool {
    matches!(value, ConstValue::Unknown)
}

/// Looks up an operand's tracked constant (`Unknown` for untracked
/// registers or places).
fn operand_const(consts: &HashMap<String, ConstValue>, operand: &LirOperand) -> ConstValue {
    let name = match operand {
        LirOperand::Reg(reg) | LirOperand::Place(reg) => reg,
    };
    consts.get(name).cloned().unwrap_or(ConstValue::Unknown)
}

/// Structural equality where `NaN` equals itself bit-for-bit (so the
/// dataflow fixpoint always converges).
fn values_equal(left: &ConstValue, right: &ConstValue) -> bool {
    match (left, right) {
        (ConstValue::Unknown, ConstValue::Unknown) => true,
        (ConstValue::Int(left, left_width), ConstValue::Int(right, right_width)) => {
            left == right && left_width == right_width
        }
        (ConstValue::Float(left, left_width), ConstValue::Float(right, right_width)) => {
            left.to_bits() == right.to_bits() && left_width == right_width
        }
        (ConstValue::Bytes(left), ConstValue::Bytes(right)) => left == right,
        (ConstValue::Bool(left), ConstValue::Bool(right)) => left == right,
        _ => false,
    }
}

/// Compares two tracked-constant maps with [`values_equal`].
fn maps_equal(left: &HashMap<String, ConstValue>, right: &HashMap<String, ConstValue>) -> bool {
    left.len() == right.len()
        && left.iter().all(|(key, value)| {
            right
                .get(key)
                .is_some_and(|other| values_equal(value, other))
        })
}

/// Joins predecessor exit maps: a key survives only when every predecessor
/// agrees on its value.
fn join_maps(maps: &[&HashMap<String, ConstValue>]) -> HashMap<String, ConstValue> {
    let Some(first) = maps.first() else {
        return HashMap::new();
    };
    let mut joined = HashMap::new();
    for (key, value) in *first {
        if maps
            .iter()
            .all(|map| map.get(key).is_some_and(|other| values_equal(value, other)))
        {
            joined.insert(key.clone(), value.clone());
        }
    }
    joined
}

/// Successor labels of a terminator.
fn successors(term: &LirTerm) -> Vec<String> {
    match term {
        LirTerm::Branch { target, .. } => vec![target.clone()],
        LirTerm::BranchIf { then, els, .. } => {
            let mut labels = vec![then.clone()];
            if let Some(els) = els {
                labels.push(els.clone());
            }
            labels
        }
        LirTerm::Return { .. } | LirTerm::Panic { .. } | LirTerm::Unreachable { .. } => Vec::new(),
    }
}

/// Hex literal for an integer constant (`None` for non-integer values).
fn int_lit(value: &ConstValue) -> Option<String> {
    match value {
        ConstValue::Int(value, _) => Some(format!("0x{value:x}")),
        _ => None,
    }
}

/// Evaluates a builtin over constant `ins` (`out_ty` supplies the result
/// width). Only the pure, value-producing builtins are foldable here;
/// pointer-producing (`@byte_buffer_ptr`) and mutating (`@store_u8`,
/// `@memcpy`) builtins are left to the interpreter (a miss otherwise).
///
/// # Errors
///
/// Returns a [`FoldValueError`] when the operands are not the expected
/// constants; callers treat that as a fold miss (not a user error).
fn eval_builtin(
    name: &str,
    ins: &[ConstValue],
    out_ty: &LirTy,
) -> Result<ConstValue, FoldValueError> {
    match name {
        "@byte_buffer_len" => match ins.first() {
            Some(ConstValue::Bytes(bytes)) => Ok(ConstValue::Int(
                u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                width_of(out_ty).unwrap_or(64),
            )),
            _ => Err(FoldValueError {
                message: "`@byte_buffer_len` requires a constant `@byte_buffer`".to_owned(),
            }),
        },
        "@byte_buffer_from" => match (ins.first(), ins.get(1)) {
            (Some(ConstValue::Bytes(bytes)), Some(ConstValue::Int(len, _))) => {
                let len = usize::try_from(*len).map_err(|_| FoldValueError {
                    message: "`@byte_buffer_from` length is out of range".to_owned(),
                })?;
                bytes
                    .get(..len)
                    .map(|slice| ConstValue::Bytes(slice.to_vec()))
                    .ok_or_else(|| FoldValueError {
                        message: "`@byte_buffer_from` length exceeds the source bytes".to_owned(),
                    })
            }
            _ => Err(FoldValueError {
                message: "`@byte_buffer_from` requires a constant pointer and length".to_owned(),
            }),
        },
        "@load_u8" => match (ins.first(), ins.get(1)) {
            (Some(ConstValue::Bytes(bytes)), Some(ConstValue::Int(offset, _))) => {
                let offset = usize::try_from(*offset).map_err(|_| FoldValueError {
                    message: "`@load_u8` offset is out of range".to_owned(),
                })?;
                bytes
                    .get(offset)
                    .map(|byte| ConstValue::Int(u64::from(*byte), 8))
                    .ok_or_else(|| FoldValueError {
                        message: "`@load_u8` offset is out of bounds".to_owned(),
                    })
            }
            _ => Err(FoldValueError {
                message: "`@load_u8` requires a constant pointee and offset".to_owned(),
            }),
        },
        _ => Err(FoldValueError {
            message: format!("`{name}` is not a foldable builtin"),
        }),
    }
}

/// Maps a `.lilac.<name>` runtime function to its `@<name>` builtin spelling,
/// or `None` for an ordinary user function. `fold` runs before the prelude is
/// injected, so a runtime call has no body yet and must be recognized by name.
fn lilac_func_builtin(name: &str) -> Option<String> {
    name.strip_prefix(".lilac.").map(|rest| format!("@{rest}"))
}

/// The result type of a runtime function the folder can evaluate, mirroring
/// its prelude signature. `None` for pointer-producing or mutating runtime
/// functions (the const value model has no pointers).
fn runtime_out_type(builtin: &str) -> Option<LirTy> {
    match builtin {
        "@byte_buffer_len" => Some(LirTy::BitNative),
        "@byte_buffer_from" => Some(LirTy::byte_buffer()),
        "@load_u8" => Some(LirTy::Bit(8)),
        _ => None,
    }
}

/// Evaluates arithmetic over constants (misses stay `Unknown`: a missed
/// fold is performance only, never a correctness error).
fn eval_arith(op: &ArithOp, left: &ConstValue, right: &ConstValue, ty: &LirTy) -> ConstValue {
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
    // Operand width (comparisons have a `#bit8` result but wider operands).
    let operand_width = match left {
        ConstValue::Int(_, width) => *width,
        _ => width,
    };
    let signed = |value: u64| sign_extend(value, operand_width);
    let unsigned = |value: u64| value & mask_of(operand_width);
    match op {
        ArithOp::IAdd => match ints {
            Some((l, r)) => ConstValue::Int(l.wrapping_add(r) & mask, width),
            None => ConstValue::Unknown,
        },
        ArithOp::ISub => match ints {
            Some((l, r)) => ConstValue::Int(l.wrapping_sub(r) & mask, width),
            None => ConstValue::Unknown,
        },
        ArithOp::IMul => match ints {
            Some((l, r)) => ConstValue::Int(l.wrapping_mul(r) & mask, width),
            None => ConstValue::Unknown,
        },
        ArithOp::IDiv => match ints {
            Some((_, 0)) | None => ConstValue::Unknown,
            Some((l, r)) => ConstValue::Int(
                signed(l).wrapping_div(signed(r)).cast_unsigned() & mask,
                width,
            ),
        },
        ArithOp::IDivU => match ints {
            Some((l, r)) => match (l & mask).checked_div(r & mask) {
                Some(value) => ConstValue::Int(value & mask, width),
                None => ConstValue::Unknown,
            },
            None => ConstValue::Unknown,
        },
        ArithOp::IMod => match ints {
            Some((_, 0)) | None => ConstValue::Unknown,
            Some((l, r)) => ConstValue::Int(
                signed(l).wrapping_rem(signed(r)).cast_unsigned() & mask,
                width,
            ),
        },
        ArithOp::IModU => match ints {
            Some((l, r)) => match (l & mask).checked_rem(r & mask) {
                Some(value) => ConstValue::Int(value & mask, width),
                None => ConstValue::Unknown,
            },
            None => ConstValue::Unknown,
        },
        ArithOp::Eq => match (left, right) {
            (ConstValue::Int(l, _), ConstValue::Int(r, _)) => ConstValue::Bool(l == r),
            (ConstValue::Bool(l), ConstValue::Bool(r)) => ConstValue::Bool(l == r),
            _ => ConstValue::Unknown,
        },
        ArithOp::ILt | ArithOp::IGt | ArithOp::ILe | ArithOp::IGe => match ints {
            Some((l, r)) => {
                let (l, r) = (signed(l), signed(r));
                ConstValue::Bool(match op {
                    ArithOp::ILt => l < r,
                    ArithOp::IGt => l > r,
                    ArithOp::ILe => l <= r,
                    _ => l >= r,
                })
            }
            None => ConstValue::Unknown,
        },
        ArithOp::ILtU | ArithOp::IGtU | ArithOp::ILeU | ArithOp::IGeU => match ints {
            Some((l, r)) => {
                let (l, r) = (unsigned(l), unsigned(r));
                ConstValue::Bool(match op {
                    ArithOp::ILtU => l < r,
                    ArithOp::IGtU => l > r,
                    ArithOp::ILeU => l <= r,
                    _ => l >= r,
                })
            }
            None => ConstValue::Unknown,
        },
        // Floats are never folded.
        ArithOp::FAdd
        | ArithOp::FSub
        | ArithOp::FMul
        | ArithOp::FDiv
        | ArithOp::FMod
        | ArithOp::FLt
        | ArithOp::FGt
        | ArithOp::FLe
        | ArithOp::FGe => ConstValue::Unknown,
    }
}

/// All-ones mask of `width` bits.
fn mask_of(width: u32) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1 << width) - 1
    }
}

/// Two's-complement value of the low `width` bits.
fn sign_extend(value: u64, width: u32) -> i64 {
    if width >= 64 {
        return value.cast_signed();
    }
    let shift = 64 - width;
    (value << shift).cast_signed() >> shift
}

/// Evaluates width conversions over constants. A `Bitcast` reinterprets the
/// bits of a float at 32/64 bits (16/128-bit folding is a best-effort miss);
/// an integer bitcast to a reference (an address) is not folded.
fn eval_convert(op: &ConvertOp, value: &ConstValue, ty: &LirTy) -> ConstValue {
    let width = width_of(ty).unwrap_or(64);
    let mask = if width >= 64 {
        u64::MAX
    } else {
        (1 << width) - 1
    };
    match (op, value) {
        (ConvertOp::Trunc, ConstValue::Int(value, _)) => ConstValue::Int(value & mask, width),
        (ConvertOp::ExtendZero, ConstValue::Int(value, _)) => ConstValue::Int(*value, width),
        (ConvertOp::Bitcast, ConstValue::Float(value, _)) => float_bits(*value, width),
        (ConvertOp::Bitcast, ConstValue::Int(bits, _)) if !ty.is_ref() => bits_float(*bits, width),
        _ => ConstValue::Unknown,
    }
}

/// The IEEE-754 bit pattern of a float constant at `width` bits (32/64 only;
/// half/quad folding is deferred). The narrowing to `f32` is the bitcast's
/// whole point, so a lossy-looking cast is intended.
#[allow(clippy::cast_possible_truncation)]
fn float_bits(value: f64, width: u32) -> ConstValue {
    match width {
        32 => ConstValue::Int(u64::from((value as f32).to_bits()), 32),
        64 => ConstValue::Int(value.to_bits(), 64),
        _ => ConstValue::Unknown,
    }
}

/// Reinterprets an integer bit pattern as a float at `width` bits (32/64 only).
fn bits_float(bits: u64, width: u32) -> ConstValue {
    if width == 64 {
        return ConstValue::Float(f64::from_bits(bits), 64);
    }
    if width == 32 {
        return u32::try_from(bits).map_or(ConstValue::Unknown, |bits| {
            ConstValue::Float(f64::from(f32::from_bits(bits)), 32)
        });
    }
    ConstValue::Unknown
}

/// The constants a `call` evaluated to, paired with the static out types
/// needed to materialize them.
struct FoldedCall {
    values: Vec<ConstValue>,
    out_types: Vec<LirTy>,
}

/// Why a call evaluation stopped: not enough information, unsupported, or
/// out of budget. The runtime call stays.
enum Eval {
    /// Leave the call in place.
    Miss,
    /// Out of steps or depth (also leaves the call; recorded for `E031`).
    Exhausted,
}

impl Folder<'_> {
    /// Folds one instruction (arithmetic, conversions, and fold-required
    /// intrinsics over constants rewrite to `Const`) and tracks its
    /// destination's constness.
    fn fold_inst(&mut self, inst: &mut LirInst, consts: &mut HashMap<String, ConstValue>) {
        match inst {
            LirInst::Const { dst, lit, ty, .. } => {
                consts.insert(dst.clone(), self.const_lit_value(lit, ty));
            }
            LirInst::CopyReg { src, dst, .. } | LirInst::CopyPlace { src, dst, .. } => {
                consts.insert(dst.clone(), operand_const(consts, src));
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
                let (dst_name, ty_name, span_val) = (dst.clone(), ty.clone(), *span);
                if let Some(lit) = int_lit(&folded) {
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
                let (dst_name, ty_name, span_val) = (dst.clone(), ty.clone(), *span);
                if let Some(lit) = int_lit(&folded) {
                    *inst = LirInst::Const {
                        dst: dst_name.clone(),
                        lit,
                        ty: ty_name,
                        span: span_val,
                    };
                }
                consts.insert(dst_name, folded);
            }
            LirInst::Zero { place, .. } => {
                consts.insert(place.clone(), ConstValue::Unknown);
            }
            _ => {}
        }
    }

    /// Constant value of a `const` literal: a data symbol under a value type
    /// is its bytes; a `ref`-typed data symbol is a pointer (not a value,
    /// so statically `Unknown`); anything else is parsed text.
    fn const_lit_value(&self, lit: &str, ty: &LirTy) -> ConstValue {
        match lit.strip_prefix('!') {
            Some(name) => match self.data.get(name) {
                Some(bytes) if !ty.is_ref() => ConstValue::Bytes(bytes.clone()),
                _ => ConstValue::Unknown,
            },
            None => parse_lit(lit, ty),
        }
    }

    /// Tracks an instruction's constants without rewriting it (used by the
    /// per-block entry-map fixpoint).
    fn track_inst(&self, inst: &LirInst, consts: &mut HashMap<String, ConstValue>) {
        match inst {
            LirInst::Const { dst, lit, ty, .. } => {
                consts.insert(dst.clone(), self.const_lit_value(lit, ty));
            }
            LirInst::CopyReg { src, dst, .. } | LirInst::CopyPlace { src, dst, .. } => {
                consts.insert(dst.clone(), operand_const(consts, src));
            }
            LirInst::Arith {
                op,
                lhs,
                rhs,
                dst,
                ty,
                ..
            } => {
                let left = consts.get(lhs).cloned().unwrap_or(ConstValue::Unknown);
                let right = consts.get(rhs).cloned().unwrap_or(ConstValue::Unknown);
                consts.insert(dst.clone(), eval_arith(op, &left, &right, ty));
            }
            LirInst::Convert {
                op, src, dst, ty, ..
            } => {
                let value = consts.get(src).cloned().unwrap_or(ConstValue::Unknown);
                consts.insert(dst.clone(), eval_convert(op, &value, ty));
            }
            LirInst::Alloca { ty, place, .. } => {
                if !ty.is_ref() {
                    if let Some(width) = width_of(ty) {
                        consts.insert(place.clone(), ConstValue::Int(0, width));
                    }
                }
            }
            LirInst::Zero { place, .. } => {
                consts.insert(place.clone(), ConstValue::Unknown);
            }
            LirInst::Call { ins, outs, .. } => {
                for place in ins.iter().chain(outs) {
                    consts.insert(place.clone(), ConstValue::Unknown);
                }
            }
            _ => {}
        }
    }

    /// Computes each block's entry `Reg/Place -> ConstValue` map to a
    /// fixpoint: the entry block starts empty and every other block joins
    /// its predecessors' exits (disagreement drops the key to unknown).
    fn compute_entry_consts(&self, func: &LirFunc) -> Vec<HashMap<String, ConstValue>> {
        let count = func.blocks.len();
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); count];
        for (index, block) in func.blocks.iter().enumerate() {
            for target in successors(&block.term) {
                if let Some(succ) = func.blocks.iter().position(|block| block.label == target) {
                    preds[succ].push(index);
                }
            }
        }
        let mut entries: Vec<HashMap<String, ConstValue>> = vec![HashMap::new(); count];
        let mut exits: Vec<HashMap<String, ConstValue>> = vec![HashMap::new(); count];
        let limit = count.saturating_mul(8).saturating_add(8);
        for _ in 0..limit {
            let mut changed = false;
            for index in 0..count {
                let entry = if index == 0 {
                    HashMap::new()
                } else {
                    let incoming: Vec<&HashMap<String, ConstValue>> =
                        preds[index].iter().map(|&pred| &exits[pred]).collect();
                    join_maps(&incoming)
                };
                let mut exit = entry.clone();
                for inst in &func.blocks[index].insts {
                    self.track_inst(inst, &mut exit);
                    self.forget_escaped(&mut exit);
                }
                if !maps_equal(&entry, &entries[index]) || !maps_equal(&exit, &exits[index]) {
                    changed = true;
                    entries[index] = entry;
                    exits[index] = exit;
                }
            }
            if !changed {
                break;
            }
        }
        entries
    }

    /// Folds one block: rewrites constant arithmetic/conversions, expands
    /// evaluated calls, and folds a constant `branch.if`.
    fn fold_block(&mut self, block: &mut LirBlock, consts: &mut HashMap<String, ConstValue>) {
        let mut insts = Vec::with_capacity(block.insts.len());
        for inst in std::mem::take(&mut block.insts) {
            match inst {
                LirInst::Call {
                    func,
                    ins,
                    outs,
                    span,
                } => self.fold_call(func, &ins, &outs, span, consts, &mut insts),
                mut other => {
                    self.fold_inst(&mut other, consts);
                    insts.push(other);
                }
            }
            self.forget_escaped(consts);
        }
        block.insts = insts;
        Self::fold_term(&mut block.term, consts);
    }

    /// Replaces a constant-condition `branch.if` with a `branch`.
    fn fold_term(term: &mut LirTerm, consts: &HashMap<String, ConstValue>) {
        if let LirTerm::BranchIf {
            cond,
            then,
            els,
            span,
        } = term
        {
            let live = consts.get(cond).and_then(is_truthy).map(|truthy| {
                if truthy {
                    then.clone()
                } else {
                    els.clone().unwrap_or_else(|| then.clone())
                }
            });
            if let Some(target) = live {
                *term = LirTerm::Branch {
                    target,
                    span: *span,
                };
            }
        }
    }

    /// Attempts to evaluate a `call` with all-constant `in` places. Success
    /// materializes the out places as constants (removing the call); a miss
    /// leaves the runtime call; a value error reports `E060` and also leaves
    /// the call.
    fn fold_call(
        &mut self,
        callee: LirCallee,
        ins: &[String],
        outs: &[String],
        span: Span,
        consts: &mut HashMap<String, ConstValue>,
        insts: &mut Vec<LirInst>,
    ) {
        let args: Vec<ConstValue> = ins
            .iter()
            .map(|place| consts.get(place).cloned().unwrap_or(ConstValue::Unknown))
            .collect();
        let outcome = if args.iter().any(is_unknown) {
            Ok(None)
        } else {
            self.try_eval_call(&callee, &args)
        };
        match outcome {
            Ok(Some(folded)) => {
                if let Some(materialized) =
                    self.materialize_call(outs, &folded.values, &folded.out_types, span)
                {
                    for (place, value) in outs.iter().zip(&folded.values) {
                        consts.insert(place.clone(), value.clone());
                    }
                    insts.extend(materialized);
                } else {
                    for (place, value) in outs.iter().zip(&folded.values) {
                        consts.insert(place.clone(), value.clone());
                    }
                    insts.push(LirInst::Call {
                        func: callee,
                        ins: ins.to_vec(),
                        outs: outs.to_vec(),
                        span,
                    });
                }
            }
            Ok(None) | Err(Eval::Miss) => {
                Self::miss_call(callee, ins, outs, span, consts, insts);
            }
            Err(Eval::Exhausted) => {
                self.exhausted = true;
                Self::miss_call(callee, ins, outs, span, consts, insts);
            }
        }
    }

    /// Leaves a runtime call and poisons its out places.
    fn miss_call(
        callee: LirCallee,
        ins: &[String],
        outs: &[String],
        span: Span,
        consts: &mut HashMap<String, ConstValue>,
        insts: &mut Vec<LirInst>,
    ) {
        for place in outs {
            consts.insert(place.clone(), ConstValue::Unknown);
        }
        insts.push(LirInst::Call {
            func: callee,
            ins: ins.to_vec(),
            outs: outs.to_vec(),
            span,
        });
    }

    /// Attempts to evaluate a call to a pure function or foldable builtin.
    /// `Ok(None)` is a miss (unknown/unsupported); `Err` is a fatal
    /// fold-required value error.
    fn try_eval_call(
        &self,
        callee: &LirCallee,
        args: &[ConstValue],
    ) -> Result<Option<FoldedCall>, Eval> {
        match callee {
            LirCallee::Func(name) => {
                if let Some(builtin) = lilac_func_builtin(name) {
                    let Some(out_ty) = runtime_out_type(&builtin) else {
                        return Ok(None);
                    };
                    return match eval_builtin(&builtin, args, &out_ty) {
                        Ok(value) => Ok(Some(FoldedCall {
                            values: vec![value],
                            out_types: vec![out_ty],
                        })),
                        Err(_) => Ok(None),
                    };
                }
                let out_types = self.func_out_types(name);
                let values = self.eval_user_call(name, args)?;
                Ok(Some(FoldedCall { values, out_types }))
            }
            LirCallee::Builtin(name) => {
                let Some(sig) = builtin_sig(name) else {
                    return Ok(None);
                };
                let out_ty = sig.outs.first().cloned().unwrap_or(LirTy::BitNative);
                match eval_builtin(name, args, &out_ty) {
                    Ok(value) => Ok(Some(FoldedCall {
                        values: vec![value],
                        out_types: sig.outs,
                    })),
                    Err(_) => Ok(None),
                }
            }
            LirCallee::Reg(_) => Ok(None),
        }
    }

    /// The out-place types of a named function (`None` when unknown).
    fn func_out_types(&self, name: &str) -> Vec<LirTy> {
        self.funcs
            .iter()
            .find(|func| func.name == name)
            .map(|func| func.outs.iter().map(|(_, ty)| ty.clone()).collect())
            .unwrap_or_default()
    }

    /// Interprets a user function under a fresh fuel budget. A body (or any
    /// nested callee) that executes a buffer-mutating builtin makes the whole
    /// evaluation a miss: frame-local stores are not reflected in the `Bytes`
    /// values a callee can return, so folding across such a call would be
    /// unsound.
    fn eval_user_call(&self, name: &str, args: &[ConstValue]) -> Result<Vec<ConstValue>, Eval> {
        let mut evaluator = Evaluator {
            funcs: self.funcs,
            data: self.data,
            steps: self.fuel,
            depth: 0,
            max_depth: self.max_depth,
            mutated: false,
        };
        let result = evaluator.run_func(name, args);
        if evaluator.mutated {
            Err(Eval::Miss)
        } else {
            result
        }
    }

    /// Emits `Const` + `CopyPlace` for every out value, or `None` when a
    /// value/type cannot be materialized as a literal (leave the call).
    fn materialize_call(
        &mut self,
        outs: &[String],
        values: &[ConstValue],
        out_types: &[LirTy],
        span: Span,
    ) -> Option<Vec<LirInst>> {
        if outs.len() != values.len() || values.len() != out_types.len() {
            return None;
        }
        let mut materialized = Vec::with_capacity(outs.len() * 2);
        for ((place, value), ty) in outs.iter().zip(values).zip(out_types) {
            let lit = match value {
                ConstValue::Int(value, _) => format!("0x{value:x}"),
                ConstValue::Float(value, _) => format!("{value:?}"),
                ConstValue::Unknown | ConstValue::Bytes(_) | ConstValue::Bool(_) => return None,
            };
            let reg = self.fresh_reg();
            materialized.push(LirInst::Const {
                dst: reg.clone(),
                lit,
                ty: ty.clone(),
                span,
            });
            materialized.push(LirInst::CopyPlace {
                src: LirOperand::Reg(reg),
                dst: place.clone(),
                span,
            });
        }
        Some(materialized)
    }

    /// A fresh register name that cannot collide with lowered names.
    fn fresh_reg(&mut self) -> String {
        self.fresh += 1;
        format!("cf.{}", self.fresh)
    }

    fn fold_func(&mut self, func: &mut LirFunc) {
        if func.is_extern {
            return;
        }
        self.escaped = escaped_places(func);
        let entries = self.compute_entry_consts(func);
        for (index, block) in func.blocks.iter_mut().enumerate() {
            let mut consts = entries.get(index).cloned().unwrap_or_default();
            self.fold_block(block, &mut consts);
        }
        Self::prune_dead(func);
    }

    /// Forgets every place whose address escaped.
    fn forget_escaped(&self, consts: &mut HashMap<String, ConstValue>) {
        for place in &self.escaped {
            consts.remove(place);
        }
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
}

/// A pointer into a frame-local byte buffer.
#[derive(Clone, Copy)]
struct Ptr {
    buffer: usize,
    offset: usize,
}

/// One interpreted call frame: register/place constants, pointers, and the
/// mutable byte buffers backing those pointers. `buffer_of` keys a source
/// name (place/register or `!data`) to its buffer so repeated pointer
/// snapshots alias the same storage (and do not grow without bound).
#[derive(Default)]
struct Frame {
    values: HashMap<String, ConstValue>,
    pointers: HashMap<String, Ptr>,
    buffer_of: HashMap<String, usize>,
    buffers: Vec<Vec<u8>>,
}

/// Interprets pure user-function bodies over a `ConstValue` environment plus
/// a const-bytes memory model (fuel- and depth-bounded).
struct Evaluator<'a> {
    funcs: &'a [LirFunc],
    data: &'a HashMap<String, Vec<u8>>,
    steps: u64,
    depth: u32,
    max_depth: u32,
    /// Set when any interpreted frame executes `@store_u8`/`@memcpy`.
    mutated: bool,
}

impl Evaluator<'_> {
    /// Interprets `name` with `args` bound to its `in` places, returning the
    /// values of its `out` places under `Eval::Miss` on any unsupported op.
    fn run_func(&mut self, name: &str, args: &[ConstValue]) -> Result<Vec<ConstValue>, Eval> {
        if self.depth >= self.max_depth {
            return Err(Eval::Exhausted);
        }
        let Some(func) = self.funcs.iter().find(|func| func.name == name) else {
            return Err(Eval::Miss);
        };
        if func.ins.len() != args.len() {
            return Err(Eval::Miss);
        }
        let mut frame = Frame::default();
        for ((place, _), value) in func.ins.iter().zip(args) {
            frame.values.insert(place.clone(), value.clone());
        }
        self.depth += 1;
        let body = self.run_blocks(func, &mut frame);
        self.depth -= 1;
        body?;
        let mut outs = Vec::with_capacity(func.outs.len());
        for (place, _) in &func.outs {
            outs.push(
                frame
                    .values
                    .get(place)
                    .cloned()
                    .unwrap_or(ConstValue::Unknown),
            );
        }
        Ok(outs)
    }

    /// Runs blocks from entry, following constant branches until `return`.
    fn run_blocks(&mut self, func: &LirFunc, frame: &mut Frame) -> Result<(), Eval> {
        let mut label = func.blocks.first().ok_or(Eval::Miss)?.label.clone();
        loop {
            let Some(block) = func.blocks.iter().find(|block| block.label == label) else {
                return Err(Eval::Miss);
            };
            for inst in &block.insts {
                self.step()?;
                self.exec_inst(inst, frame)?;
            }
            match &block.term {
                LirTerm::Return { .. } => return Ok(()),
                LirTerm::Branch { target, .. } => label = target.clone(),
                LirTerm::BranchIf {
                    cond, then, els, ..
                } => {
                    let value = frame
                        .values
                        .get(cond)
                        .cloned()
                        .unwrap_or(ConstValue::Unknown);
                    match is_truthy(&value) {
                        Some(true) => label = then.clone(),
                        Some(false) => label = els.clone().ok_or(Eval::Miss)?,
                        None => return Err(Eval::Miss),
                    }
                }
                LirTerm::Panic { .. } | LirTerm::Unreachable { .. } => return Err(Eval::Miss),
            }
        }
    }

    /// Decrements the fuel budget.
    fn step(&mut self) -> Result<(), Eval> {
        if self.steps == 0 {
            return Err(Eval::Exhausted);
        }
        self.steps -= 1;
        Ok(())
    }

    fn exec_inst(&mut self, inst: &LirInst, frame: &mut Frame) -> Result<(), Eval> {
        match inst {
            LirInst::Const { dst, lit, ty, .. } => {
                bind_const(self.data, frame, dst, lit, ty);
                Ok(())
            }
            LirInst::CopyReg { src, dst, .. } | LirInst::CopyPlace { src, dst, .. } => {
                let value = operand_value(frame, src);
                frame.values.insert(dst.clone(), value);
                if let Some(pointer) = operand_ptr(frame, src) {
                    frame.pointers.insert(dst.clone(), pointer);
                }
                Ok(())
            }
            LirInst::Arith {
                op,
                lhs,
                rhs,
                dst,
                ty,
                ..
            } => {
                let left = frame
                    .values
                    .get(lhs)
                    .cloned()
                    .unwrap_or(ConstValue::Unknown);
                let right = frame
                    .values
                    .get(rhs)
                    .cloned()
                    .unwrap_or(ConstValue::Unknown);
                frame
                    .values
                    .insert(dst.clone(), eval_arith(op, &left, &right, ty));
                Ok(())
            }
            LirInst::Convert {
                op, src, dst, ty, ..
            } => {
                let value = frame
                    .values
                    .get(src)
                    .cloned()
                    .unwrap_or(ConstValue::Unknown);
                frame
                    .values
                    .insert(dst.clone(), eval_convert(op, &value, ty));
                Ok(())
            }
            LirInst::Alloca { ty, place, .. } => {
                if !ty.is_ref() {
                    if let Some(width) = width_of(ty) {
                        frame
                            .values
                            .insert(place.clone(), ConstValue::Int(0, width));
                    }
                }
                Ok(())
            }
            LirInst::Zero { place, .. } => {
                let zero = match frame.values.get(place) {
                    Some(ConstValue::Int(_, width)) => ConstValue::Int(0, *width),
                    _ => ConstValue::Unknown,
                };
                frame.values.insert(place.clone(), zero);
                Ok(())
            }
            LirInst::Call {
                func,
                ins,
                outs,
                span,
            } => self.exec_call(func, ins, outs, *span, frame),
            _ => Err(Eval::Miss),
        }
    }

    fn exec_call(
        &mut self,
        callee: &LirCallee,
        ins: &[String],
        outs: &[String],
        span: Span,
        frame: &mut Frame,
    ) -> Result<(), Eval> {
        match callee {
            LirCallee::Func(name) => {
                if let Some(builtin) = lilac_func_builtin(name) {
                    return self.exec_builtin(&builtin, ins, outs, span, frame);
                }
                let args: Vec<ConstValue> = ins
                    .iter()
                    .map(|place| {
                        frame
                            .values
                            .get(place)
                            .cloned()
                            .unwrap_or(ConstValue::Unknown)
                    })
                    .collect();
                if args.iter().any(is_unknown) {
                    return Err(Eval::Miss);
                }
                let values = self.run_func(name, &args)?;
                for (place, value) in outs.iter().zip(values) {
                    frame.values.insert(place.clone(), value);
                }
                Ok(())
            }
            LirCallee::Builtin(name) => self.exec_builtin(name, ins, outs, span, frame),
            LirCallee::Reg(_) => Err(Eval::Miss),
        }
    }

    /// Interprets a builtin over the frame's constants and const-bytes
    /// memory; unknown/impure builtins are a miss. Store builtins set
    /// `mutated` so the enclosing call-eval is discarded (see
    /// [`Folder::eval_user_call`]).
    fn exec_builtin(
        &mut self,
        name: &str,
        ins: &[String],
        outs: &[String],
        span: Span,
        frame: &mut Frame,
    ) -> Result<(), Eval> {
        match name {
            "@byte_buffer_len" => {
                let arg = call_operand(ins, 0, name, span);
                let value = frame
                    .values
                    .get(arg)
                    .cloned()
                    .unwrap_or(ConstValue::Unknown);
                let result =
                    eval_builtin(name, &[value], &LirTy::BitNative).map_err(|_| Eval::Miss)?;
                bind_out(frame, outs, &[result]);
                Ok(())
            }
            "@byte_buffer_ptr" => {
                let arg = call_operand(ins, 0, name, span);
                let result = call_result(outs, name, span);
                let buffer = if let Some(&buffer) = frame.buffer_of.get(arg) {
                    buffer
                } else {
                    let Some(ConstValue::Bytes(bytes)) = frame.values.get(arg).cloned() else {
                        return Err(Eval::Miss);
                    };
                    let buffer = frame.buffers.len();
                    frame.buffers.push(bytes);
                    frame.buffer_of.insert(arg.clone(), buffer);
                    buffer
                };
                frame
                    .pointers
                    .insert(result.clone(), Ptr { buffer, offset: 0 });
                Ok(())
            }
            "@byte_buffer_from" => {
                let pointer_arg = call_operand(ins, 0, name, span);
                let len_arg = call_operand(ins, 1, name, span);
                let pointer = *frame.pointers.get(pointer_arg).ok_or(Eval::Miss)?;
                let len = expect_int(frame, len_arg)?;
                let bytes = slice_from(frame, pointer)?;
                let result = eval_builtin(
                    name,
                    &[ConstValue::Bytes(bytes), ConstValue::Int(len, 64)],
                    &LirTy::byte_buffer(),
                )
                .map_err(|_| Eval::Miss)?;
                bind_out(frame, outs, &[result]);
                Ok(())
            }
            "@load_u8" => {
                let pointer_arg = call_operand(ins, 0, name, span);
                let offset_arg = call_operand(ins, 1, name, span);
                let pointer = *frame.pointers.get(pointer_arg).ok_or(Eval::Miss)?;
                let offset =
                    usize::try_from(expect_int(frame, offset_arg)?).map_err(|_| Eval::Miss)?;
                let at = pointer.offset.checked_add(offset).ok_or(Eval::Miss)?;
                let byte = *frame
                    .buffers
                    .get(pointer.buffer)
                    .and_then(|buffer| buffer.get(at))
                    .ok_or(Eval::Miss)?;
                bind_out(frame, outs, &[ConstValue::Int(u64::from(byte), 8)]);
                Ok(())
            }
            "@store_u8" => {
                let pointer_arg = call_operand(ins, 0, name, span);
                let offset_arg = call_operand(ins, 1, name, span);
                let value_arg = call_operand(ins, 2, name, span);
                let pointer = *frame.pointers.get(pointer_arg).ok_or(Eval::Miss)?;
                let offset =
                    usize::try_from(expect_int(frame, offset_arg)?).map_err(|_| Eval::Miss)?;
                let value = u8::try_from(expect_int(frame, value_arg)?).map_err(|_| Eval::Miss)?;
                let at = pointer.offset.checked_add(offset).ok_or(Eval::Miss)?;
                let buffer = frame.buffers.get_mut(pointer.buffer).ok_or(Eval::Miss)?;
                *buffer.get_mut(at).ok_or(Eval::Miss)? = value;
                self.mutated = true;
                Ok(())
            }
            "@memcpy" => {
                let dst_arg = call_operand(ins, 0, name, span);
                let src_arg = call_operand(ins, 1, name, span);
                let len_arg = call_operand(ins, 2, name, span);
                let dst = *frame.pointers.get(dst_arg).ok_or(Eval::Miss)?;
                let src = *frame.pointers.get(src_arg).ok_or(Eval::Miss)?;
                let len = usize::try_from(expect_int(frame, len_arg)?).map_err(|_| Eval::Miss)?;
                memcpy(frame, dst, src, len)?;
                self.mutated = true;
                Ok(())
            }
            _ => Err(Eval::Miss),
        }
    }
}

/// Fetches a call's `index`-th in-place (a broken arity is an internal
/// invariant, so a missing slot is an ICE via `bug!`, never an index panic).
fn call_operand<'a>(ins: &'a [String], index: usize, name: &str, span: Span) -> &'a String {
    ins.get(index)
        .unwrap_or_else(|| lilac_error::bug!(span, "`{name}` call is missing operand {index}"))
}

/// Fetches a single-result call's out place (see [`call_operand`]).
fn call_result<'a>(outs: &'a [String], name: &str, span: Span) -> &'a String {
    outs.first()
        .unwrap_or_else(|| lilac_error::bug!(span, "`{name}` call is missing its result place"))
}

/// Binds a `const` literal into a frame (data symbols become `Bytes`; a
/// `ref`-typed data symbol becomes a pointer to a fresh buffer).
fn bind_const(
    data: &HashMap<String, Vec<u8>>,
    frame: &mut Frame,
    dst: &str,
    lit: &str,
    ty: &LirTy,
) {
    if let Some(name) = lit.strip_prefix('!') {
        if let Some(bytes) = data.get(name) {
            if ty.is_ref() {
                let key = format!("!{name}");
                let buffer = if let Some(&buffer) = frame.buffer_of.get(&key) {
                    buffer
                } else {
                    let buffer = frame.buffers.len();
                    frame.buffers.push(bytes.clone());
                    frame.buffer_of.insert(key, buffer);
                    buffer
                };
                frame
                    .pointers
                    .insert(dst.to_owned(), Ptr { buffer, offset: 0 });
            } else {
                frame
                    .values
                    .insert(dst.to_owned(), ConstValue::Bytes(bytes.clone()));
            }
        } else {
            frame.values.insert(dst.to_owned(), ConstValue::Unknown);
        }
    } else {
        frame.values.insert(dst.to_owned(), parse_lit(lit, ty));
    }
}

/// Binds a builtin's result to its out places.
fn bind_out(frame: &mut Frame, outs: &[String], values: &[ConstValue]) {
    for (place, value) in outs.iter().zip(values) {
        frame.values.insert(place.clone(), value.clone());
    }
}

/// An operand's tracked constant (`Unknown` when absent).
fn operand_value(frame: &Frame, operand: &LirOperand) -> ConstValue {
    let name = match operand {
        LirOperand::Reg(name) | LirOperand::Place(name) => name,
    };
    frame
        .values
        .get(name)
        .cloned()
        .unwrap_or(ConstValue::Unknown)
}

/// An operand's tracked pointer (`None` when not a pointer).
fn operand_ptr(frame: &Frame, operand: &LirOperand) -> Option<Ptr> {
    let name = match operand {
        LirOperand::Reg(name) | LirOperand::Place(name) => name,
    };
    frame.pointers.get(name).copied()
}

/// Reads an integer constant by place/register name, missing otherwise.
fn expect_int(frame: &Frame, name: &str) -> Result<u64, Eval> {
    match frame.values.get(name) {
        Some(ConstValue::Int(value, _)) => Ok(*value),
        _ => Err(Eval::Miss),
    }
}

/// Copies a pointer's remaining (offset-adjusted) bytes out of the frame.
fn slice_from(frame: &Frame, pointer: Ptr) -> Result<Vec<u8>, Eval> {
    frame
        .buffers
        .get(pointer.buffer)
        .and_then(|buffer| buffer.get(pointer.offset..))
        .map(<[u8]>::to_vec)
        .ok_or(Eval::Miss)
}

/// Copies `len` bytes between frame-local buffers (aliasing-safe).
fn memcpy(frame: &mut Frame, dst: Ptr, src: Ptr, len: usize) -> Result<(), Eval> {
    if dst.buffer == src.buffer {
        let buffer = frame.buffers.get_mut(dst.buffer).ok_or(Eval::Miss)?;
        let src_end = src.offset.checked_add(len).ok_or(Eval::Miss)?;
        let dst_end = dst.offset.checked_add(len).ok_or(Eval::Miss)?;
        if src_end > buffer.len() || dst_end > buffer.len() {
            return Err(Eval::Miss);
        }
        buffer.copy_within(src.offset..src_end, dst.offset);
    } else {
        let chunk = {
            let source = frame.buffers.get(src.buffer).ok_or(Eval::Miss)?;
            let end = src.offset.checked_add(len).ok_or(Eval::Miss)?;
            source.get(src.offset..end).ok_or(Eval::Miss)?.to_vec()
        };
        let destination = frame.buffers.get_mut(dst.buffer).ok_or(Eval::Miss)?;
        let end = dst.offset.checked_add(len).ok_or(Eval::Miss)?;
        destination
            .get_mut(dst.offset..end)
            .ok_or(Eval::Miss)?
            .copy_from_slice(&chunk);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    fn t(text: &str) -> lilac_ir::LirTy {
        lilac_ir::LirTy::parse(text).expect("type spelling")
    }

    use super::*;
    use lilac_error::Diags;
    use lilac_ir::{ArithOp, LirBlock, LirData, LirFunc, LirInst, LirModule, LirOperand, LirTerm};
    use lilac_span::Span;

    fn test_span() -> Span {
        Span::dummy()
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn reg(name: &str) -> LirOperand {
        LirOperand::Reg(name.to_owned())
    }

    fn place(name: &str) -> LirOperand {
        LirOperand::Place(name.to_owned())
    }

    fn alloca(ty: &str, name: &str) -> LirInst {
        LirInst::Alloca {
            ty: t(ty),
            place: name.to_owned(),
            span: test_span(),
        }
    }

    fn constant(dst: &str, lit: &str, ty: &str) -> LirInst {
        LirInst::Const {
            dst: dst.to_owned(),
            lit: lit.to_owned(),
            ty: t(ty),
            span: test_span(),
        }
    }

    fn copy_place(src: LirOperand, dst: &str) -> LirInst {
        LirInst::CopyPlace {
            src,
            dst: dst.to_owned(),
            span: test_span(),
        }
    }

    fn copy_reg(src: LirOperand, dst: &str, ty: &str) -> LirInst {
        LirInst::CopyReg {
            src,
            dst: dst.to_owned(),
            ty: t(ty),
            span: test_span(),
        }
    }

    fn arith(op: ArithOp, lhs: &str, rhs: &str, dst: &str, ty: &str) -> LirInst {
        LirInst::Arith {
            op,
            lhs: lhs.to_owned(),
            rhs: rhs.to_owned(),
            dst: dst.to_owned(),
            ty: t(ty),
            span: test_span(),
        }
    }

    fn extend(src: &str, dst: &str, ty: &str) -> LirInst {
        LirInst::Convert {
            op: ConvertOp::ExtendZero,
            src: src.to_owned(),
            dst: dst.to_owned(),
            ty: t(ty),
            span: test_span(),
        }
    }

    fn call_func(name: &str, ins: &[&str], outs: &[&str]) -> LirInst {
        LirInst::Call {
            func: LirCallee::Func(name.to_owned()),
            ins: strings(ins),
            outs: strings(outs),
            span: test_span(),
        }
    }

    fn branch(target: &str) -> LirTerm {
        LirTerm::Branch {
            target: target.to_owned(),
            span: test_span(),
        }
    }

    fn branch_if(cond: &str, then: &str, els: Option<&str>) -> LirTerm {
        LirTerm::BranchIf {
            cond: cond.to_owned(),
            then: then.to_owned(),
            els: els.map(str::to_owned),
            span: test_span(),
        }
    }

    fn ret() -> LirTerm {
        LirTerm::Return { span: test_span() }
    }

    fn arith_module() -> LirModule {
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
                    constant("1", "0x1", "#bit32"),
                    constant("2", "0x2", "#bit32"),
                    arith(ArithOp::IAdd, "1", "2", "3", "#bit32"),
                ],
                term: ret(),
                span: test_span(),
            }],
        });
        module
    }

    #[test]
    fn eval_arith_unsigned_semantics() {
        let max = ConstValue::Int(u64::MAX, 64);
        let one = ConstValue::Int(1, 64);
        assert!(matches!(
            eval_arith(&ArithOp::IGtU, &max, &one, &t("#bit64")),
            ConstValue::Bool(true)
        ));
        assert!(matches!(
            eval_arith(&ArithOp::IDivU, &max, &ConstValue::Int(2, 64), &t("#bit64")),
            ConstValue::Int(value, 64) if value == 0x7fff_ffff_ffff_ffff
        ));
        assert!(matches!(
            eval_arith(
                &ArithOp::IModU,
                &ConstValue::Int(7, 64),
                &ConstValue::Int(3, 64),
                &t("#bit64")
            ),
            ConstValue::Int(1, 64)
        ));
        assert!(matches!(
            eval_arith(&ArithOp::IDivU, &one, &ConstValue::Int(0, 64), &t("#bit64")),
            ConstValue::Unknown
        ));
    }

    #[test]
    fn folds_const_arith() {
        let mut module = arith_module();
        let diags = Diags::new();
        fold(&mut module);
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
            is_extern: false,
            inline: false,
            blocks: vec![
                LirBlock {
                    label: "entry".to_owned(),
                    insts: vec![constant("1", cond_lit, "#bit8")],
                    term: branch_if("1", "then", Some("els")),
                    span: test_span(),
                },
                LirBlock {
                    label: "then".to_owned(),
                    insts: vec![LirInst::CompileError {
                        msg: "boom".to_owned(),
                        msg_span: test_span(),
                        call_span: test_span(),
                    }],
                    term: ret(),
                    span: test_span(),
                },
                LirBlock {
                    label: "els".to_owned(),
                    insts: Vec::new(),
                    term: ret(),
                    span: test_span(),
                },
            ],
        });
        module
    }

    #[test]
    fn if_fold_prunes_dead() {
        let mut module = branch_module(false);
        let diags = Diags::new();
        fold(&mut module);
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
        let diags = Diags::new();
        fold(&mut module);
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn live_compile_error_is_e031() {
        let mut module = branch_module(true);
        let mut diags = Diags::new();
        // Folding no longer decides E031; the module-level check does, so a
        // caller that already pruned unreachable functions can run it.
        let report = fold(&mut module);
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(check_live_compile_errors(&module, &report, &mut diags).is_err());
        let errors: Vec<_> = diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E031")
            .collect();
        assert_eq!(errors.len(), 1, "{diags:?}");
        assert_eq!(errors[0].labels.len(), 2, "{diags:?}");
    }

    /// `main` plus an uncalled `dead` function carrying a `@compile_error`.
    fn dead_error_module() -> LirModule {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: Vec::new(),
                term: ret(),
                span: test_span(),
            }],
        });
        module.funcs.push(LirFunc {
            name: "dead".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![LirInst::CompileError {
                    msg: "boom".to_owned(),
                    msg_span: test_span(),
                    call_span: test_span(),
                }],
                term: ret(),
                span: test_span(),
            }],
        });
        module
    }

    /// Once DCE drops an unreachable function, its `@compile_error` is silent.
    #[test]
    fn pruned_compile_error_is_silent() {
        let mut module = dead_error_module();
        let mut diags = Diags::new();
        let report = fold(&mut module);
        assert!(!diags.has_errors(), "fold must not report E031: {diags:?}");
        lilac_ir::prune_unreachable(&mut module, &std::collections::HashSet::new());
        assert!(
            module.funcs.iter().all(|f| f.name != "dead"),
            "dead function must be pruned"
        );
        assert!(check_live_compile_errors(&module, &report, &mut diags).is_ok());
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn bitcast_reinterprets_bits_directly() {
        // `1.0f32` is `0x3f80_0000`; the reverse round-trips.
        let bits = eval_convert(
            &ConvertOp::Bitcast,
            &ConstValue::Float(1.0, 32),
            &t("#bit32"),
        );
        assert!(
            matches!(bits, ConstValue::Int(value, 32) if value == 0x3f80_0000),
            "{bits:?}"
        );
        let back = eval_convert(
            &ConvertOp::Bitcast,
            &ConstValue::Int(0x3f80_0000, 32),
            &t("#fbit32"),
        );
        assert!(
            matches!(back, ConstValue::Float(value, 32) if value.to_bits() == 1.0f64.to_bits()),
            "{back:?}"
        );
        // 64-bit doubles round-trip too.
        let ints = eval_convert(
            &ConvertOp::Bitcast,
            &ConstValue::Float(1.0, 64),
            &t("#bit64"),
        );
        assert!(
            matches!(ints, ConstValue::Int(value, 64) if value == 0x3ff0_0000_0000_0000),
            "{ints:?}"
        );
        // 16/128-bit folding is a best-effort miss.
        assert!(matches!(
            eval_convert(
                &ConvertOp::Bitcast,
                &ConstValue::Float(1.0, 16),
                &t("#bit16")
            ),
            ConstValue::Unknown
        ));
    }

    /// A two-function module: `@parse` is a pure loop over `@load_u8` from
    /// its `@byte_buffer` param accumulating a `#bit_native` sum. When
    /// `set_arg`, `main` passes a `"AB"` data buffer; otherwise the arg
    /// place is never assigned a constant.
    ///
    /// `"AB"` is `[0x41, 0x42]`, so the folded sum is `0x83`.
    fn parser_module(set_arg: bool) -> LirModule {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.data.push(LirData {
            name: "c0".to_owned(),
            bytes: "AB".as_bytes().to_vec(),
            span: test_span(),
        });
        let parse = LirFunc {
            name: "parse".to_owned(),
            ins: vec![("buf".to_owned(), t("#byte_buffer"))],
            outs: vec![("result".to_owned(), t("#bit_native"))],
            is_extern: false,
            inline: false,
            blocks: vec![
                LirBlock {
                    label: "entry".to_owned(),
                    insts: vec![
                        alloca("#bit_native", "i"),
                        alloca("#bit_native", "acc"),
                        alloca("#bit_native", "len"),
                        constant("r0", "0x0", "#bit_native"),
                        copy_place(reg("r0"), "i"),
                        copy_place(reg("r0"), "acc"),
                        call_func(".lilac.byte_buffer_len", &["buf"], &["len"]),
                    ],
                    term: branch("cond"),
                    span: test_span(),
                },
                LirBlock {
                    label: "cond".to_owned(),
                    insts: vec![
                        copy_reg(place("i"), "r1", "#bit_native"),
                        copy_reg(place("len"), "r2", "#bit_native"),
                        arith(ArithOp::ILt, "r1", "r2", "r3", "#bit8"),
                    ],
                    term: branch_if("r3", "body", Some("done")),
                    span: test_span(),
                },
                LirBlock {
                    label: "body".to_owned(),
                    insts: vec![
                        alloca("ref #bit8", "p"),
                        alloca("#bit8", "c"),
                        call_func(".lilac.byte_buffer_ptr", &["buf"], &["p"]),
                        call_func(".lilac.load_u8", &["p", "i"], &["c"]),
                        copy_reg(place("c"), "r4", "#bit8"),
                        extend("r4", "r4b", "#bit_native"),
                        copy_reg(place("acc"), "r5", "#bit_native"),
                        arith(ArithOp::IAdd, "r5", "r4b", "r6", "#bit_native"),
                        copy_place(reg("r6"), "acc"),
                        copy_reg(place("i"), "r7", "#bit_native"),
                        constant("r8", "0x1", "#bit_native"),
                        arith(ArithOp::IAdd, "r7", "r8", "r9", "#bit_native"),
                        copy_place(reg("r9"), "i"),
                    ],
                    term: branch("cond"),
                    span: test_span(),
                },
                LirBlock {
                    label: "done".to_owned(),
                    insts: vec![
                        copy_reg(place("acc"), "r10", "#bit_native"),
                        copy_place(reg("r10"), "result"),
                    ],
                    term: ret(),
                    span: test_span(),
                },
            ],
        };
        let mut main_insts = vec![alloca("#byte_buffer", "arg"), alloca("#bit_native", "res")];
        if set_arg {
            main_insts.push(constant("m0", "!c0", "#byte_buffer"));
            main_insts.push(copy_place(reg("m0"), "arg"));
        }
        main_insts.push(call_func("parse", &["arg"], &["res"]));
        let main = LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: main_insts,
                term: ret(),
                span: test_span(),
            }],
        };
        module.funcs.push(main);
        module.funcs.push(parse);
        module
    }

    #[test]
    fn block_wrapper_parser_folds() {
        let mut module = parser_module(true);
        let diags = Diags::new();
        fold(&mut module);
        assert!(!diags.has_errors(), "{diags:?}");
        let text = module.to_text();
        assert!(text.contains("0x83"), "{text}");
        assert!(!text.contains("call @parse"), "{text}");
    }

    /// A `.lilac.byte_buffer_len` call on a constant buffer folds during
    /// `fold` (which runs before the prelude is injected), so a branch it
    /// controls is resolved and a dead `@compile_error` is pruned silently.
    #[test]
    fn lilac_byte_buffer_len_folds_and_prunes_dead_compile_error() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.data.push(LirData {
            name: "msg".to_owned(),
            bytes: "boom".as_bytes().to_vec(),
            span: test_span(),
        });
        module.data.push(LirData {
            name: "ab".to_owned(),
            bytes: "AB".as_bytes().to_vec(),
            span: test_span(),
        });
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![
                LirBlock {
                    label: "entry".to_owned(),
                    insts: vec![
                        alloca("#byte_buffer", "b"),
                        alloca("#bit_native", "len"),
                        constant("c", "!ab", "#byte_buffer"),
                        copy_place(reg("c"), "b"),
                        call_func(".lilac.byte_buffer_len", &["b"], &["len"]),
                        copy_reg(place("len"), "n", "#bit_native"),
                        constant("two", "0x2", "#bit_native"),
                        arith(ArithOp::Eq, "n", "two", "eq", "#bit_native"),
                    ],
                    term: branch_if("eq", "ok", Some("err")),
                    span: test_span(),
                },
                LirBlock {
                    label: "ok".to_owned(),
                    insts: Vec::new(),
                    term: ret(),
                    span: test_span(),
                },
                LirBlock {
                    label: "err".to_owned(),
                    insts: vec![LirInst::CompileError {
                        msg: "boom".to_owned(),
                        msg_span: test_span(),
                        call_span: test_span(),
                    }],
                    term: ret(),
                    span: test_span(),
                },
            ],
        });
        let diags = Diags::new();
        fold(&mut module);
        assert!(!diags.has_errors(), "{diags:?}");
        let text = module.to_text();
        assert!(!text.contains(".lilac.byte_buffer_len"), "{text}");
        assert!(!text.contains("$err"), "{text}");
    }

    #[test]
    fn call_eval_miss_stays_call() {
        let mut module = parser_module(false);
        let _diags = Diags::new();
        fold(&mut module);
        assert!(
            module.to_text().contains("call @parse"),
            "{}",
            module.to_text()
        );
    }

    #[test]
    fn fuel_exhaustion_stays_call() {
        let mut module = parser_module(true);
        let _diags = Diags::new();
        let report = fold_with_fuel(&mut module, 2, 16);
        assert!(
            module.to_text().contains("call @parse"),
            "{}",
            module.to_text()
        );
        // The miss is recorded so a live `@compile_error` here can say why.
        assert!(!report.exhausted.is_empty(), "{report:?}");
        let report = fold(&mut parser_module(true));
        assert!(report.exhausted.is_empty(), "{report:?}");
    }

    /// A two-function module where `@wipe` mutates its `@byte_buffer` param
    /// (store `0x0` at offset 0, then load it back as a `#bit_native` result).
    /// A pre-fix folder would fold this to `const 0x0`; the mutation guard
    /// must leave the runtime call.
    fn mutating_module() -> LirModule {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.data.push(LirData {
            name: "c0".to_owned(),
            bytes: "AB".as_bytes().to_vec(),
            span: test_span(),
        });
        let wipe = LirFunc {
            name: "wipe".to_owned(),
            ins: vec![("buf".to_owned(), t("#byte_buffer"))],
            outs: vec![("result".to_owned(), t("#bit_native"))],
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![
                    alloca("ref #bit8", "p"),
                    alloca("#bit_native", "z"),
                    alloca("#bit8", "v"),
                    alloca("#bit8", "w"),
                    constant("r0", "0x0", "#bit_native"),
                    copy_place(reg("r0"), "z"),
                    constant("r1", "0x0", "#bit8"),
                    copy_place(reg("r1"), "v"),
                    call_func(".lilac.byte_buffer_ptr", &["buf"], &["p"]),
                    call_func(".lilac.store_u8", &["p", "z", "v"], &[]),
                    call_func(".lilac.load_u8", &["p", "z"], &["w"]),
                    copy_reg(place("w"), "r2", "#bit8"),
                    extend("r2", "r3", "#bit_native"),
                    copy_place(reg("r3"), "result"),
                ],
                term: ret(),
                span: test_span(),
            }],
        };
        let main = LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![
                    alloca("#byte_buffer", "arg"),
                    alloca("#bit_native", "res"),
                    constant("m0", "!c0", "#byte_buffer"),
                    copy_place(reg("m0"), "arg"),
                    call_func("wipe", &["arg"], &["res"]),
                ],
                term: ret(),
                span: test_span(),
            }],
        };
        module.funcs.push(main);
        module.funcs.push(wipe);
        module
    }

    #[test]
    fn store_in_callee_stays_call() {
        let mut module = mutating_module();
        let diags = Diags::new();
        fold(&mut module);
        assert!(!diags.has_errors(), "{diags:?}");
        let text = module.to_text();
        assert!(text.contains("call @wipe"), "{text}");
    }
}
