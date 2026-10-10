fn t(text: &str) -> crate::LirTy {
    crate::LirTy::parse(text).expect("type spelling")
}

use super::*;
use crate::print::print_op;
use crate::verify::Verifier;
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
            name: "gen_function__bit32".into(),
            ins: vec![("in".into(), t("#bit32"))],
            outs: vec![("out".into(), t("#bit32"))],
            is_extern: false,
            export: None,
            variadic: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".into(),
                insts: vec![
                    LirInst::CopyReg {
                        src: LirOperand::Place("in".into()),
                        dst: "1".into(),
                        ty: t("#bit32"),
                        span: test_span(),
                    },
                    LirInst::CopyPlace {
                        src: LirOperand::Reg("1".into()),
                        dst: "out".into(),
                        span: test_span(),
                    },
                ],
                term: LirTerm::Return { span: test_span() },
                span: test_span(),
            }],
        }],
        layouts: HashMap::new(),
        globals: Vec::new(),
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
        src: LirOperand::Reg("9".into()),
        dst: "out".into(),
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
        name: "mismatch".into(),
        ins: Vec::new(),
        outs: Vec::new(),
        is_extern: false,
        export: None,
        variadic: false,
        inline: false,
        blocks: vec![LirBlock {
            label: "entry".into(),
            insts: vec![
                LirInst::Alloca {
                    ty: t("ref #Object"),
                    place: "r".into(),
                    span: test_span(),
                },
                LirInst::Alloca {
                    ty: t("#Object"),
                    place: "v".into(),
                    span: test_span(),
                },
                LirInst::CopyPlace {
                    src: LirOperand::Place("r".into()),
                    dst: "v".into(),
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
    let msgs: Vec<String> = diags
        .diagnostics()
        .iter()
        .map(|d| d.msg.to_string())
        .collect();
    assert!(msgs.iter().any(|m| m.contains("copy")), "{msgs:?}");
}

/// Verifies `insts` after a `#bit32` place `%x`, its address `&p` and
/// the constant `&v`, returning the `E070` messages.
fn atomic_errors(insts: Vec<LirInst>) -> Vec<String> {
    let span = test_span();
    let mut module = LirModule::new("x86_64-unknown-none");
    let mut func = trivial_func("atomics");
    func.blocks[0].insts = vec![
        LirInst::Alloca {
            ty: t("#bit32"),
            place: "x".into(),
            span,
        },
        LirInst::Addr {
            place: "x".into(),
            dst: "p".into(),
            ty: t("ref #bit32"),
            span,
        },
        LirInst::Const {
            dst: "v".into(),
            value: LirConst::Int(1),
            ty: t("#bit32"),
            span,
        },
        LirInst::Const {
            dst: "w".into(),
            value: LirConst::Int(1),
            ty: t("#bit64"),
            span,
        },
    ];
    func.blocks[0].insts.extend(insts);
    module.funcs.push(func);
    let mut diags = lilac_error::Diags::new();
    let _ = verify(&module, &mut diags);
    diags
        .diagnostics()
        .iter()
        .map(|d| d.msg.to_string())
        .collect()
}

#[test]
fn verify_checks_elem_addr() {
    let span = test_span();
    let elem_errors = |array_ty: &str, index_ty: &str, ty: &str| {
        let mut module = LirModule::new("x86_64-unknown-none");
        let mut func = trivial_func("arrays");
        func.blocks[0].insts = vec![
            LirInst::Alloca {
                ty: t("[4 x #bit32]"),
                place: "a".into(),
                span,
            },
            LirInst::Addr {
                place: "a".into(),
                dst: "p".into(),
                ty: t(array_ty),
                span,
            },
            LirInst::Const {
                dst: "i".into(),
                value: LirConst::Int(1),
                ty: t(index_ty),
                span,
            },
            LirInst::ElemAddr {
                array: "p".into(),
                index: "i".into(),
                dst: "e".into(),
                ty: t(ty),
                span,
            },
        ];
        module.funcs.push(func);
        let mut diags = lilac_error::Diags::new();
        let _ = verify(&module, &mut diags);
        diags
            .diagnostics()
            .iter()
            .map(|d| d.msg.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        elem_errors("ref [4 x #bit32]", "#bit_native", "ref #bit32"),
        Vec::<String>::new()
    );
    for (array, index, ty, want) in [
        (
            "ref [4 x #bit32]",
            "#bit_native",
            "ref #bit8",
            "must be `ref #bit32`",
        ),
        (
            "ref [4 x #bit32]",
            "#bit32",
            "ref #bit32",
            "must be `#bit_native`",
        ),
        (
            "ref #bit32",
            "#bit_native",
            "ref #bit32",
            "must be a `ref` array",
        ),
    ] {
        let errors = elem_errors(array, index, ty);
        assert!(
            errors.iter().any(|error| error.contains(want)),
            "{want}: {errors:?}"
        );
    }
}

#[test]
fn verify_accepts_atomics() {
    let span = test_span();
    let mut insts = Vec::new();
    for (i, order) in AtomicOrdering::ALL.into_iter().enumerate() {
        insts.push(LirInst::AtomicRmw {
            op: RmwOp::ALL[i],
            ptr: "p".into(),
            value: "v".into(),
            dst: format!("r{i}").into(),
            ty: t("#bit32"),
            order,
            span,
        });
        insts.push(LirInst::CmpXchg {
            ptr: "p".into(),
            expected: "v".into(),
            desired: "v".into(),
            dst: format!("c{i}").into(),
            ty: t("#bit32"),
            order,
            span,
        });
    }
    for order in [AtomicOrdering::Relaxed, AtomicOrdering::Acquire] {
        insts.push(LirInst::AtomicLoad {
            ptr: "p".into(),
            dst: format!("l{}", order.name()).into(),
            ty: t("#bit32"),
            order,
            span,
        });
    }
    insts.push(LirInst::AtomicStore {
        value: "v".into(),
        ptr: "p".into(),
        ty: t("#bit32"),
        order: AtomicOrdering::Release,
        span,
    });
    insts.push(LirInst::Fence {
        order: AtomicOrdering::AcqRel,
        span,
    });
    assert_eq!(atomic_errors(insts), Vec::<String>::new());
}

#[test]
fn verify_rejects_bad_atomics() {
    let span = test_span();
    let load = |order| LirInst::AtomicLoad {
        ptr: "p".into(),
        dst: "l".into(),
        ty: t("#bit32"),
        order,
        span,
    };
    let store = |order| LirInst::AtomicStore {
        value: "v".into(),
        ptr: "p".into(),
        ty: t("#bit32"),
        order,
        span,
    };
    let rmw = |ptr: &str, value: &str, ty: &str| LirInst::AtomicRmw {
        op: RmwOp::Add,
        ptr: ptr.into(),
        value: value.into(),
        dst: "r".into(),
        ty: t(ty),
        order: AtomicOrdering::SeqCst,
        span,
    };
    let cases = [
        (
            load(AtomicOrdering::Release),
            "`atomic.load` cannot be `release`",
        ),
        (
            load(AtomicOrdering::AcqRel),
            "`atomic.load` cannot be `acq_rel`",
        ),
        (
            store(AtomicOrdering::Acquire),
            "`atomic.store` cannot be `acquire`",
        ),
        (
            LirInst::Fence {
                order: AtomicOrdering::Relaxed,
                span,
            },
            "`fence` cannot be `relaxed`",
        ),
        (rmw("v", "v", "#bit32"), "address `&v` must be `ref #bit32`"),
        (rmw("p", "w", "#bit32"), "operand `&w` must be `#bit32`"),
        (rmw("p", "w", "#bit64"), "address `&p` must be `ref #bit64`"),
        (rmw("p", "v", "#fbit32"), "needs a bit-container type"),
        (
            rmw("p", "missing", "#bit32"),
            "undefined register `&missing`",
        ),
    ];
    for (inst, want) in cases {
        let errors = atomic_errors(vec![inst]);
        assert!(
            errors.iter().any(|error| error.contains(want)),
            "{want}: {errors:?}"
        );
    }
}

fn trivial_func(name: &str) -> LirFunc {
    LirFunc {
        name: name.into(),
        ins: Vec::new(),
        outs: Vec::new(),
        is_extern: false,
        export: None,
        variadic: false,
        inline: false,
        blocks: vec![LirBlock {
            label: "entry".into(),
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
    let msgs: Vec<String> = diags
        .diagnostics()
        .iter()
        .map(|d| d.msg.to_string())
        .collect();
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
        name: "refcopy".into(),
        ins: Vec::new(),
        outs: Vec::new(),
        is_extern: false,
        export: None,
        variadic: false,
        inline: false,
        blocks: vec![LirBlock {
            label: "entry".into(),
            insts: vec![
                LirInst::Alloca {
                    ty: t("ref #bit8"),
                    place: "a".into(),
                    span: test_span(),
                },
                LirInst::Alloca {
                    ty: t("ref #byte_buffer"),
                    place: "b".into(),
                    span: test_span(),
                },
                LirInst::CopyPlace {
                    src: LirOperand::Place("a".into()),
                    dst: "b".into(),
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
        dst: "2".into(),
        value: LirConst::Int(1),
        ty: t("#bit8"),
        span: test_span(),
    };
    module.funcs[0].blocks[0].term = LirTerm::BranchIf {
        cond: "2".into(),
        then: "entry".into(),
        els: None,
        span: test_span(),
    };
    let mut diags = lilac_error::Diags::new();
    assert!(verify(&module, &mut diags).is_err());
    // The dropped `copy &1 -> %out` also starves the out place; pin the
    // constant-branch error by message instead of by count.
    let msgs: Vec<String> = diags
        .diagnostics()
        .iter()
        .map(|d| d.msg.to_string())
        .collect();
    assert!(
        diags.diagnostics().iter().any(|d| d.code == "E070")
            && msgs.iter().any(|m| m.contains("constant")),
        "{msgs:?}"
    );
}

/// The errors from verifying one unused `const` of `ty` in the canonical
/// module.
fn const_errors(value: LirConst, ty: &str) -> Vec<String> {
    let mut module = canonical_module();
    module.funcs[0].blocks[0].insts.push(LirInst::Const {
        dst: "2".into(),
        value,
        ty: t(ty),
        span: test_span(),
    });
    let mut diags = lilac_error::Diags::new();
    let _ = verify(&module, &mut diags);
    diags
        .diagnostics()
        .iter()
        .map(|d| d.msg.to_string())
        .collect()
}

#[test]
fn verify_accepts_constants_that_fit() {
    for (value, ty) in [
        (LirConst::Int(0xff), "#bit8"),
        (LirConst::Int(u64::MAX), "#bit64"),
        (LirConst::Int(u64::MAX), "#bit_native"),
        (LirConst::Float(1.5), "#fbit32"),
        (LirConst::Int(0), "ref #bit8"),
    ] {
        let errors = const_errors(value.clone(), ty);
        assert!(errors.is_empty(), "{value} as {ty}: {errors:?}");
    }
}

#[test]
fn verify_rejects_constants_of_the_wrong_type() {
    for (value, ty, want) in [
        (
            LirConst::Int(0x100),
            "#bit8",
            "`const 0x100` cannot be a `#bit8`",
        ),
        (
            LirConst::Float(1.5),
            "#bit32",
            "`const 1.5` cannot be a `#bit32`",
        ),
        (
            LirConst::Int(1),
            "#fbit64",
            "`const 0x1` cannot be a `#fbit64`",
        ),
        (
            LirConst::Int(8),
            "ref #bit8",
            "`const 0x8` cannot be a `ref #bit8`",
        ),
        (
            LirConst::Data("missing".into()),
            "#byte_buffer",
            "const of undeclared data `!missing`",
        ),
    ] {
        let errors = const_errors(value, ty);
        assert!(
            errors.iter().any(|e| e.contains(want)),
            "{want}: {errors:?}"
        );
    }
}

#[test]
fn verify_rejects_bad_branch_target() {
    let mut module = canonical_module();
    module.funcs[0].blocks[0].term = LirTerm::Branch {
        target: "nope".into(),
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
        cond: "1".into(),
        then: "entry".into(),
        els: None,
        span: test_span(),
    };
    let mut diags = lilac_error::Diags::new();
    assert!(verify(&module, &mut diags).is_err());
    assert_eq!(e070_count(&diags), 1, "{:?}", diags.diagnostics());
    let msgs: Vec<String> = diags
        .diagnostics()
        .iter()
        .map(|d| d.msg.to_string())
        .collect();
    assert!(msgs.iter().any(|m| m.contains("else")), "{msgs:?}");
}

/// The same terminator is fine when a following block supplies the false
/// edge.
#[test]
fn verify_accepts_branchif_fallthrough_with_next_block() {
    let mut module = canonical_module();
    module.funcs[0].blocks[0].term = LirTerm::BranchIf {
        cond: "1".into(),
        then: "entry".into(),
        els: None,
        span: test_span(),
    };
    module.funcs[0].blocks.push(LirBlock {
        label: "next".into(),
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
        src: LirOperand::Place("in".into()),
        dst: "2".into(),
        ty: t(lhs_ty),
        span: test_span(),
    });
    module.funcs[0].blocks[0].insts.push(LirInst::CopyReg {
        src: LirOperand::Place("in".into()),
        dst: "3".into(),
        ty: t(rhs_ty),
        span: test_span(),
    });
    module.funcs[0].blocks[0].insts.push(LirInst::Arith {
        op,
        lhs: "2".into(),
        rhs: "3".into(),
        dst: "4".into(),
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
        let msgs: Vec<String> = diags
            .diagnostics()
            .iter()
            .map(|d| d.msg.to_string())
            .collect();
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
            name: "puts".into(),
            ins: vec![("s".into(), t("#byte_buffer"))],
            outs: Vec::new(),
            is_extern: true,
            export: None,
            variadic: false,
            inline: false,
            blocks: Vec::new(),
        }],
        layouts: HashMap::new(),
        globals: Vec::new(),
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
            name: "puts".into(),
            ins: vec![("s".into(), t("#byte_buffer"))],
            outs: Vec::new(),
            is_extern: true,
            export: None,
            variadic: false,
            inline: false,
            blocks: Vec::new(),
        }],
        layouts: HashMap::new(),
        globals: Vec::new(),
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
    module.funcs[0].outs = vec![("s".into(), t("#byte_buffer"))];
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
    module.funcs[0].outs = vec![("p".into(), t("ref #unknown"))];
    let mut diags = lilac_error::Diags::new();
    assert!(verify(&module, &mut diags).is_ok(), "{diags:?}");
    assert_eq!(e070_count(&diags), 0);
}

#[test]
fn verifier_rejects_bare_unknown() {
    let mut module = extern_module();
    module.funcs[0].outs = vec![("p".into(), t("#unknown"))];
    let mut diags = lilac_error::Diags::new();
    assert!(verify(&module, &mut diags).is_err());
    assert_eq!(e070_count(&diags), 1);
}

#[test]
fn verifier_rejects_non_extern_duplicate_places() {
    let mut module = canonical_module();
    module.funcs[0].ins = vec![("in".into(), t("#bit32")), ("in".into(), t("#bit64"))];
    let mut diags = lilac_error::Diags::new();
    assert!(verify(&module, &mut diags).is_err());
    assert_eq!(e070_count(&diags), 1);
}

#[test]
fn builtin_call_prints_and_verifies() {
    let mut module = LirModule::new("x86_64-unknown-none");
    module.funcs.push(LirFunc {
        name: "main".into(),
        ins: Vec::new(),
        outs: Vec::new(),
        is_extern: false,
        export: None,
        variadic: false,
        inline: false,
        blocks: vec![LirBlock {
            label: "entry".into(),
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
        name: "c0".into(),
        bytes: "hi".as_bytes().to_vec(),
        span: test_span(),
    });
    module.funcs.push(LirFunc {
        name: "main".into(),
        ins: Vec::new(),
        outs: Vec::new(),
        is_extern: false,
        export: None,
        variadic: false,
        inline: false,
        blocks: vec![LirBlock {
            label: "entry".into(),
            insts: vec![LirInst::Const {
                dst: "1".into(),
                value: LirConst::Data("c0".into()),
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
        name: "main".into(),
        ins: Vec::new(),
        outs: Vec::new(),
        is_extern: false,
        export: None,
        variadic: false,
        inline: false,
        blocks: vec![LirBlock {
            label: "entry".into(),
            insts: vec![LirInst::Const {
                dst: "1".into(),
                value: LirConst::Data("c9".into()),
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
        name: "main".into(),
        ins: vec![("buffer".into(), t("#byte_buffer"))],
        outs: vec![("out".into(), t("ref #bit8"))],
        is_extern: false,
        export: None,
        variadic: false,
        inline: false,
        blocks: vec![LirBlock {
            label: "entry".into(),
            insts: vec![
                LirInst::Field {
                    obj: "buffer".into(),
                    name: "data".to_owned(),
                    idx: 1,
                    dst: "r".into(),
                    ty: t("ref #bit8"),
                    span: test_span(),
                },
                LirInst::CopyPlace {
                    src: LirOperand::Reg("r".into()),
                    dst: "out".into(),
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
        name: "c0".into(),
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
        name: "main".into(),
        ins: Vec::new(),
        outs: Vec::new(),
        is_extern: false,
        export: None,
        variadic: false,
        inline: false,
        blocks: vec![LirBlock {
            label: "entry".into(),
            insts: vec![
                LirInst::Const {
                    dst: "1".into(),
                    value: LirConst::Int(1),
                    ty: t("#bit_native"),
                    span: const1_span,
                },
                LirInst::Const {
                    dst: "2".into(),
                    value: LirConst::Int(2),
                    ty: t("#bit_native"),
                    span: const2_span,
                },
                LirInst::Arith {
                    op: ArithOp::IAdd,
                    lhs: "1".into(),
                    rhs: "2".into(),
                    dst: "3".into(),
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
        name: "puts".into(),
        ins: vec![("s".into(), t("#bit8"))],
        outs: vec![],
        is_extern: true,
        export: None,
        variadic: false,
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
        name: "c0".into(),
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
