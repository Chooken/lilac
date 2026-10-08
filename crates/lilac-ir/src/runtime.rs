//! Builds the nine runtime functions as ordinary LIR.
//!
//! The functions are not injected here; the driver appends `build_runtime` to
//! a module between constant folding and dead-code pruning, where DCE keeps
//! only the ones a program reaches.
//!
//! Every place is addressed (the pointer place ABI), so the bodies only need
//! `alloca`/`field`/`field.addr`, `copy`/`store`, `arith`, `branch`, and the
//! `@ptr_add` and `@lilac_abort` leaves. Registers are block-local in the
//! verifier, so each block re-materializes the values it uses.

use lilac_span::Span;

use crate::{ArithOp, LirBlock, LirCallee, LirFunc, LirInst, LirOperand, LirTerm, LirTy};

/// Builds the runtime prelude for a target with `ptr_size`-byte pointers.
///
/// The nine functions are returned in call-dependency order:
/// `byte_buffer_from`, `byte_buffer_ptr`, `byte_buffer_len`, `load_u8`,
/// `store_u8`, `format_args_new`, `format_args_push`, `format_args_count`,
/// `format_args_piece`. The piece stride is `#byte_buffer`'s size,
/// `2 * ptr_size` bytes.
#[must_use]
pub fn build_runtime(ptr_size: u32) -> Vec<LirFunc> {
    let stride = format!("0x{:x}", 2 * ptr_size);
    vec![
        byte_buffer_from(),
        byte_buffer_ptr(),
        byte_buffer_len(),
        load_u8(),
        store_u8(),
        format_args_new(),
        format_args_push(&stride),
        format_args_count(),
        format_args_piece(&stride),
    ]
}

fn span() -> Span {
    Span::dummy()
}

fn alloca(ty: LirTy, place: &str) -> LirInst {
    LirInst::Alloca {
        ty,
        place: place.to_owned(),
        span: span(),
    }
}

fn field(obj: &str, name: &str, idx: u32, dst: &str, ty: LirTy) -> LirInst {
    LirInst::Field {
        obj: obj.to_owned(),
        name: name.to_owned(),
        idx,
        dst: dst.to_owned(),
        ty,
        span: span(),
    }
}

fn field_addr(obj: &str, name: &str, idx: u32, dst: &str, ty: LirTy) -> LirInst {
    LirInst::FieldAddr {
        obj: obj.to_owned(),
        name: name.to_owned(),
        idx,
        dst: dst.to_owned(),
        ty,
        span: span(),
    }
}

fn store_field(value: LirOperand, obj: &str, name: &str, idx: u32) -> LirInst {
    LirInst::StoreField {
        value,
        obj: obj.to_owned(),
        name: name.to_owned(),
        idx,
        span: span(),
    }
}

fn store(src: LirOperand, dst: &str) -> LirInst {
    LirInst::Store {
        src,
        dst: dst.to_owned(),
        span: span(),
    }
}

fn copy_place(src: LirOperand, dst: &str) -> LirInst {
    LirInst::CopyPlace {
        src,
        dst: dst.to_owned(),
        span: span(),
    }
}

fn copy_reg(src: LirOperand, dst: &str, ty: LirTy) -> LirInst {
    LirInst::CopyReg {
        src,
        dst: dst.to_owned(),
        ty,
        span: span(),
    }
}

fn load(src: &str, dst: &str, ty: LirTy) -> LirInst {
    LirInst::Load {
        src: src.to_owned(),
        dst: dst.to_owned(),
        ty,
        span: span(),
    }
}

fn constant(dst: &str, lit: &str, ty: LirTy) -> LirInst {
    LirInst::Const {
        dst: dst.to_owned(),
        lit: lit.to_owned(),
        ty,
        span: span(),
    }
}

fn arith(op: ArithOp, lhs: &str, rhs: &str, dst: &str, ty: LirTy) -> LirInst {
    LirInst::Arith {
        op,
        lhs: lhs.to_owned(),
        rhs: rhs.to_owned(),
        dst: dst.to_owned(),
        ty,
        span: span(),
    }
}

fn call_builtin(name: &str, ins: &[&str], outs: &[&str]) -> LirInst {
    LirInst::Call {
        func: LirCallee::Builtin(name.to_owned()),
        ins: ins.iter().map(|place| (*place).to_owned()).collect(),
        outs: outs.iter().map(|place| (*place).to_owned()).collect(),
        span: span(),
    }
}

fn block(label: &str, insts: Vec<LirInst>, term: LirTerm) -> LirBlock {
    LirBlock {
        label: label.to_owned(),
        insts,
        term,
        span: span(),
    }
}

fn ret() -> LirTerm {
    LirTerm::Return { span: span() }
}

fn unreachable() -> LirTerm {
    LirTerm::Unreachable { span: span() }
}

fn branch(target: &str) -> LirTerm {
    LirTerm::Branch {
        target: target.to_owned(),
        span: span(),
    }
}

fn branch_if(cond: &str, then: &str, els: &str) -> LirTerm {
    LirTerm::BranchIf {
        cond: cond.to_owned(),
        then: then.to_owned(),
        els: Some(els.to_owned()),
        span: span(),
    }
}

fn lir_func(
    name: &str,
    ins: &[(&str, LirTy)],
    outs: &[(&str, LirTy)],
    blocks: Vec<LirBlock>,
) -> LirFunc {
    LirFunc {
        name: name.to_owned(),
        ins: ins
            .iter()
            .map(|(place, ty)| ((*place).to_owned(), ty.clone()))
            .collect(),
        outs: outs
            .iter()
            .map(|(place, ty)| ((*place).to_owned(), ty.clone()))
            .collect(),
        is_extern: false,
        inline: false,
        blocks,
    }
}

/// `.lilac.byte_buffer_from(ref #bit8 %data, #bit_native %len) -> #byte_buffer`.
fn byte_buffer_from() -> LirFunc {
    lir_func(
        ".lilac.byte_buffer_from",
        &[("data", LirTy::byte_ref()), ("len", LirTy::BitNative)],
        &[("out", LirTy::byte_buffer())],
        vec![block(
            "entry",
            vec![
                alloca(LirTy::byte_buffer(), "tmp"),
                store_field(LirOperand::Place("len".to_owned()), "tmp", "len", 0),
                store_field(LirOperand::Place("data".to_owned()), "tmp", "data", 1),
                copy_place(LirOperand::Place("tmp".to_owned()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.byte_buffer_ptr(#byte_buffer %buffer) -> ref #bit8`.
fn byte_buffer_ptr() -> LirFunc {
    lir_func(
        ".lilac.byte_buffer_ptr",
        &[("buffer", LirTy::byte_buffer())],
        &[("out", LirTy::byte_ref())],
        vec![block(
            "entry",
            vec![
                field("buffer", "data", 1, "r", LirTy::byte_ref()),
                copy_place(LirOperand::Reg("r".to_owned()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.byte_buffer_len(#byte_buffer %buffer) -> #bit_native`.
fn byte_buffer_len() -> LirFunc {
    lir_func(
        ".lilac.byte_buffer_len",
        &[("buffer", LirTy::byte_buffer())],
        &[("out", LirTy::BitNative)],
        vec![block(
            "entry",
            vec![
                field("buffer", "len", 0, "r", LirTy::BitNative),
                copy_place(LirOperand::Reg("r".to_owned()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.load_u8(ref #bit8 %base, #bit_native %index) -> #bit8`.
///
/// Addresses `base + index` with `@ptr_add`, then loads the byte through the
/// resulting `ref #bit8` place.
fn load_u8() -> LirFunc {
    lir_func(
        ".lilac.load_u8",
        &[("base", LirTy::byte_ref()), ("index", LirTy::BitNative)],
        &[("out", LirTy::Bit(8))],
        vec![block(
            "entry",
            vec![
                alloca(LirTy::byte_ref(), "slot"),
                call_builtin("@ptr_add", &["base", "index"], &["slot"]),
                load("slot", "r", LirTy::Bit(8)),
                copy_place(LirOperand::Reg("r".to_owned()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.store_u8(ref #bit8 %base, #bit_native %index, #bit8 %value)`.
///
/// Addresses `base + index` with `@ptr_add`, then stores the byte through the
/// resulting `ref #bit8` place.
fn store_u8() -> LirFunc {
    lir_func(
        ".lilac.store_u8",
        &[
            ("base", LirTy::byte_ref()),
            ("index", LirTy::BitNative),
            ("value", LirTy::Bit(8)),
        ],
        &[],
        vec![block(
            "entry",
            vec![
                alloca(LirTy::byte_ref(), "slot"),
                call_builtin("@ptr_add", &["base", "index"], &["slot"]),
                store(LirOperand::Place("value".to_owned()), "slot"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.format_args_new() -> #format_args` (a zeroed piece table).
fn format_args_new() -> LirFunc {
    lir_func(
        ".lilac.format_args_new",
        &[],
        &[("out", LirTy::format_args())],
        vec![block(
            "entry",
            vec![
                alloca(LirTy::format_args(), "tmp"),
                copy_place(LirOperand::Place("tmp".to_owned()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.format_args_push(ref #format_args %table, #byte_buffer %piece)`.
///
/// Appends `piece` while `count < 16`, then increments `count`.
fn format_args_push(stride: &str) -> LirFunc {
    lir_func(
        ".lilac.format_args_push",
        &[
            ("table", LirTy::format_args().to_ref()),
            ("piece", LirTy::byte_buffer()),
        ],
        &[],
        vec![
            block(
                "entry",
                vec![
                    field("table", "count", 0, "c", LirTy::BitNative),
                    constant("cap", "0x10", LirTy::BitNative),
                    arith(ArithOp::ILtU, "c", "cap", "ok", LirTy::Bit(8)),
                ],
                branch_if("ok", "append", "done"),
            ),
            block(
                "append",
                vec![
                    field("table", "count", 0, "c", LirTy::BitNative),
                    field_addr("table", "p0", 1, "p0", LirTy::byte_buffer().to_ref()),
                    alloca(LirTy::byte_ref(), "base"),
                    copy_place(LirOperand::Reg("p0".to_owned()), "base"),
                    constant("stride", stride, LirTy::BitNative),
                    arith(ArithOp::IMul, "c", "stride", "off", LirTy::BitNative),
                    alloca(LirTy::BitNative, "offp"),
                    copy_place(LirOperand::Reg("off".to_owned()), "offp"),
                    alloca(LirTy::byte_ref(), "slot"),
                    call_builtin("@ptr_add", &["base", "offp"], &["slot"]),
                    alloca(LirTy::byte_buffer().to_ref(), "s"),
                    copy_place(LirOperand::Place("slot".to_owned()), "s"),
                    store(LirOperand::Place("piece".to_owned()), "s"),
                    constant("one", "0x1", LirTy::BitNative),
                    arith(ArithOp::IAdd, "c", "one", "n", LirTy::BitNative),
                    store_field(LirOperand::Reg("n".to_owned()), "table", "count", 0),
                ],
                branch("done"),
            ),
            block("done", Vec::new(), ret()),
        ],
    )
}

/// `.lilac.format_args_count(#format_args %table) -> #bit_native`.
fn format_args_count() -> LirFunc {
    lir_func(
        ".lilac.format_args_count",
        &[("table", LirTy::format_args())],
        &[("out", LirTy::BitNative)],
        vec![block(
            "entry",
            vec![
                field("table", "count", 0, "r", LirTy::BitNative),
                copy_place(LirOperand::Reg("r".to_owned()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.format_args_piece(#format_args %table, #bit_native %index) ->
/// #byte_buffer`.
///
/// Reads `p0[index]`, trapping through `@lilac_abort` when `index >= count`.
fn format_args_piece(stride: &str) -> LirFunc {
    lir_func(
        ".lilac.format_args_piece",
        &[("table", LirTy::format_args()), ("index", LirTy::BitNative)],
        &[("out", LirTy::byte_buffer())],
        vec![
            block(
                "entry",
                vec![
                    field("table", "count", 0, "c", LirTy::BitNative),
                    copy_reg(
                        LirOperand::Place("index".to_owned()),
                        "idx",
                        LirTy::BitNative,
                    ),
                    arith(ArithOp::ILtU, "idx", "c", "ok", LirTy::Bit(8)),
                ],
                branch_if("ok", "load", "trap"),
            ),
            block(
                "trap",
                vec![call_builtin("@lilac_abort", &[], &[])],
                unreachable(),
            ),
            block(
                "load",
                vec![
                    copy_reg(
                        LirOperand::Place("index".to_owned()),
                        "idx",
                        LirTy::BitNative,
                    ),
                    field_addr("table", "p0", 1, "p0", LirTy::byte_buffer().to_ref()),
                    alloca(LirTy::byte_ref(), "base"),
                    copy_place(LirOperand::Reg("p0".to_owned()), "base"),
                    constant("stride", stride, LirTy::BitNative),
                    arith(ArithOp::IMul, "idx", "stride", "off", LirTy::BitNative),
                    alloca(LirTy::BitNative, "offp"),
                    copy_place(LirOperand::Reg("off".to_owned()), "offp"),
                    alloca(LirTy::byte_ref(), "slot"),
                    call_builtin("@ptr_add", &["base", "offp"], &["slot"]),
                    alloca(LirTy::byte_buffer().to_ref(), "s"),
                    copy_place(LirOperand::Place("slot".to_owned()), "s"),
                    load("s", "v", LirTy::byte_buffer()),
                    copy_place(LirOperand::Reg("v".to_owned()), "out"),
                ],
                ret(),
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::build_runtime;
    use crate::{verify, ArithOp, LirFunc, LirInst, LirModule};

    fn e070_count(diags: &lilac_error::Diags) -> usize {
        diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E070")
            .count()
    }

    #[test]
    fn runtime_has_nine_functions_in_order() {
        let funcs = build_runtime(8);
        let names: Vec<&str> = funcs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                ".lilac.byte_buffer_from",
                ".lilac.byte_buffer_ptr",
                ".lilac.byte_buffer_len",
                ".lilac.load_u8",
                ".lilac.store_u8",
                ".lilac.format_args_new",
                ".lilac.format_args_push",
                ".lilac.format_args_count",
                ".lilac.format_args_piece",
            ]
        );
        assert!(funcs.iter().all(|f| !f.is_extern));
        // The two byte-access bodies the backend no longer defines.
        assert!(names.contains(&".lilac.load_u8"));
        assert!(names.contains(&".lilac.store_u8"));
    }

    /// Both `format_args` bounds checks use unsigned less-than, so a negative
    /// (high-bit-set) index cannot slip past `index < count`.
    #[test]
    fn runtime_bounds_checks_are_unsigned() {
        let funcs = build_runtime(8);
        for name in [".lilac.format_args_push", ".lilac.format_args_piece"] {
            let func = funcs
                .iter()
                .find(|f| f.name == name)
                .expect("runtime function");
            let ops: Vec<&ArithOp> = func
                .blocks
                .iter()
                .flat_map(|b| &b.insts)
                .filter_map(|inst| match inst {
                    LirInst::Arith { op, .. } => Some(op),
                    _ => None,
                })
                .collect();
            assert!(
                ops.iter().any(|op| matches!(op, ArithOp::ILtU)),
                "{name} must bounds-check with ILtU"
            );
            assert!(
                !ops.iter().any(|op| matches!(op, ArithOp::ILt)),
                "{name} must not use signed ILt"
            );
        }
    }

    #[test]
    fn runtime_module_verifies() {
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs = build_runtime(8);
        let mut diags = lilac_error::Diags::new();
        assert!(
            verify(&module, &mut diags).is_ok(),
            "{:?}",
            diags.diagnostics()
        );
        assert_eq!(e070_count(&diags), 0);
    }

    fn stride_lits(funcs: &[LirFunc], name: &str) -> Vec<String> {
        funcs
            .iter()
            .find(|f| f.name == name)
            .map(|f| {
                f.blocks
                    .iter()
                    .flat_map(|b| &b.insts)
                    .filter_map(|inst| match inst {
                        LirInst::Const { dst, lit, ty, .. }
                            if dst == "stride" && *ty == crate::LirTy::BitNative =>
                        {
                            Some(lit.clone())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn runtime_embeds_pointer_stride() {
        for (ptr_size, expected) in [(8u32, "0x10"), (4, "0x8")] {
            let funcs = build_runtime(ptr_size);
            for name in [".lilac.format_args_push", ".lilac.format_args_piece"] {
                assert_eq!(
                    stride_lits(&funcs, name),
                    vec![expected.to_owned()],
                    "{name} at ptr_size {ptr_size}"
                );
            }
        }
    }
}
