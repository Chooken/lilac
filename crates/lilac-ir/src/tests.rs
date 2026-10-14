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
        target: lilac_target::Target::default(),
        data: Vec::new(),
        formats: Vec::new(),
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
    let expected = "lilac_ir = \"0.5\"\ntarget = \"x86_64-unknown-none\"\nfunction @gen_function__bit32\n  in #bit32 %in\n  out #bit32 %out\n{\n  $entry:\n    &1: #bit32 = copy %in\n    copy &1 -> %out\n    return\n}\n";
    assert_eq!(text, expected);
}

#[test]
fn printer_emits_type_sections() {
    let mut module = canonical_module();
    module.layouts.insert(
        "Number".to_owned(),
        layout(&[("tag", "#bit8"), ("val", "#bit32")]),
    );
    let text = module.to_text();
    assert!(
        text.contains("type #Number size 8 align 4 {\n    tag: #bit8,\n    val: #bit32,\n}\n"),
        "{text}"
    );
    let tagged = LirLayout::new(
        LirLayoutKind::Tagged,
        vec![
            ("tag".to_owned(), t("#bit8")),
            ("A".to_owned(), t("#bit64")),
            ("B".to_owned(), t("#bit8")),
        ],
        &module.layouts,
        &module.target,
    )
    .expect("a tagged layout");
    assert_eq!(
        tagged.fields.iter().map(|f| f.offset).collect::<Vec<_>>(),
        [0, 8, 8]
    );
    module.layouts.insert("Choice".to_owned(), tagged);
    let text = module.to_text();
    assert!(
        text.contains("type #Choice tagged size 16 align 8 {\n    tag: #bit8,\n"),
        "{text}"
    );
}

/// A C struct of `(name, type)` fields on the default target.
fn layout(fields: &[(&str, &str)]) -> LirLayout {
    LirLayout::of_struct(
        fields
            .iter()
            .map(|(name, ty)| ((*name).to_owned(), t(ty)))
            .collect(),
        &HashMap::<String, LirLayout>::new(),
        &lilac_target::Target::default(),
    )
    .expect("a layout")
}

/// Layouts are looked up, and on 32-bit x86 off Windows 8-byte scalars
/// align to 4.
#[test]
fn size_align_follows_the_target() {
    let i686 = lilac_target::Target::parse("x86-unknown-linux-gnu").expect("triple");
    let mut layouts = HashMap::new();
    let pair = LirLayout::of_struct(
        vec![("a".to_owned(), t("#bit8")), ("b".to_owned(), t("#bit64"))],
        &layouts,
        &i686,
    )
    .expect("fits");
    assert_eq!((pair.size, pair.align, pair.fields[1].offset), (12, 4, 4));
    layouts.insert("Pair".to_owned(), pair);
    let size = |ty: &str| size_align(&layouts, &t(ty), &i686).map(|s| (s.size, s.align));
    assert_eq!(size("#Pair"), Some((12, 4)));
    assert_eq!(size("[3 x #Pair]"), Some((36, 4)));
    assert_eq!(size("#fbit64"), Some((8, 4)));
    assert_eq!(size("#fbit128"), Some((16, 16)));
    assert_eq!(size("ref #Pair"), Some((4, 4)));
    assert_eq!(size("#bit_native"), Some((4, 4)));
    assert_eq!(size("#byte_buffer"), Some((8, 4)));
    assert_eq!(size("#format_args"), Some((8, 4)));
    assert_eq!(size("#unknown"), None);
    assert_eq!(size("#Missing"), None);
    // Past the target's limit is no size, never a wrapped one.
    assert_eq!(size("[4294967295 x [4294967295 x #bit64]]"), None);
    let too_large = LirLayout::of_struct(
        vec![
            ("a".to_owned(), t("[1073741824 x #bit8]")),
            ("b".to_owned(), t("[1073741824 x #bit8]")),
        ],
        &layouts,
        &i686,
    )
    .expect_err("too large");
    assert!(matches!(too_large, LayoutError::TooLarge(_)), "{too_large}");
}

/// The verifier recomputes every layout for the module's target.
#[test]
fn verify_rejects_a_layout_that_is_not_the_c_layout() {
    let mut module = canonical_module();
    let mut wrong = layout(&[("tag", "#bit8"), ("val", "#bit32")]);
    wrong.fields[1].offset = 1;
    wrong.size = 5;
    module.layouts.insert("Number".to_owned(), wrong);
    let mut diags = lilac_error::Diags::new();
    assert!(verify(&module, &mut diags).is_err());
    let message = diags.diagnostics()[0].msg.to_string();
    assert!(
        message.contains("size 5 align 4, offsets [0, 1], expected size 8 align 4, offsets [0, 4]"),
        "{message}"
    );

    // A layout computed for another target is wrong here too.
    let mut module = canonical_module();
    let i686 = lilac_target::Target::parse("x86-unknown-linux-gnu").expect("triple");
    let on_i686 = LirLayout::of_struct(
        vec![("a".to_owned(), t("#bit8")), ("b".to_owned(), t("#fbit64"))],
        &HashMap::<String, LirLayout>::new(),
        &i686,
    )
    .expect("fits");
    module.layouts.insert("Pair".to_owned(), on_i686);
    let mut diags = lilac_error::Diags::new();
    assert!(verify(&module, &mut diags).is_err());

    // A field with no size has no layout.
    let mut module = canonical_module();
    let mut sizeless = layout(&[("tag", "#bit8")]);
    sizeless.fields[0].ty = t("#Missing");
    module.layouts.insert("Bad".to_owned(), sizeless);
    let mut diags = lilac_error::Diags::new();
    assert!(verify(&module, &mut diags).is_err());
    assert!(
        diags.diagnostics()[0]
            .msg
            .contains("`#Missing` has no size"),
        "{diags:?}"
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
    let module =
        LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
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
    let mut module =
        LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
    module
        .layouts
        .insert("Object".to_owned(), layout(&[("value", "#bit32")]));
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
    let mut module =
        LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
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
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
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
    let mut module =
        LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
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
    let mut module =
        LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
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
        (LirConst::Float(FloatBits::F32(1.5f32.to_bits())), "#fbit32"),
        (LirConst::Float(FloatBits::F16(0x3C00)), "#fbit16"),
        (LirConst::Float(FloatBits::F128(0x3FFF << 112)), "#fbit128"),
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
            LirConst::Float(FloatBits::F32(1.5f32.to_bits())),
            "#bit32",
            "`const 1.5` cannot be a `#bit32`",
        ),
        (
            LirConst::Float(FloatBits::F32(1.5f32.to_bits())),
            "#fbit64",
            "`const 1.5` cannot be a `#fbit64`",
        ),
        (
            LirConst::Float(FloatBits::F16(0x3C00)),
            "#fbit128",
            "`const 0xH3C00` cannot be a `#fbit128`",
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
        target: lilac_target::Target::default(),
        data: Vec::new(),
        formats: Vec::new(),
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
        target: lilac_target::Target::default(),
        data: Vec::new(),
        formats: Vec::new(),
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
    let mut module =
        LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
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

    // Aggregate `==`: C's `memcmp`, an `int`.
    let memcmp = builtin_sig("@memcmp").expect("@memcmp is a builtin");
    assert_eq!(
        memcmp.ins,
        vec![t("ref #bit8"), t("ref #bit8"), t("#bit_native")]
    );
    assert_eq!(memcmp.outs, vec![t("#bit32")]);
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
    let mut module =
        LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
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
                ty: t("ref #bit8"),
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
    let mut module =
        LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
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
                ty: t("ref #bit8"),
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

/// The canonical module with a `#Pair` layout, a `#Pair` place `%p`, a
/// `ref #Pair` place `%r`, and `insts` after them.
fn pair_module(insts: Vec<LirInst>) -> LirModule {
    let mut module = canonical_module();
    module.layouts.insert(
        "Pair".to_owned(),
        layout(&[("a", "#bit32"), ("b", "#bit8")]),
    );
    let block = &mut module.funcs[0].blocks[0].insts;
    block.push(LirInst::Alloca {
        ty: t("#Pair"),
        place: "p".into(),
        span: test_span(),
    });
    block.push(LirInst::Alloca {
        ty: t("ref #Pair"),
        place: "r".into(),
        span: test_span(),
    });
    block.extend(insts);
    module
}

fn verify_messages(module: &LirModule) -> Vec<String> {
    let mut diags = lilac_error::Diags::new();
    let _ = verify(module, &mut diags);
    diags
        .diagnostics()
        .iter()
        .map(|d| d.msg.to_string())
        .collect()
}

/// Registers hold scalars: every instruction that defines a register of an
/// aggregate type is `E070`.
#[test]
fn verify_rejects_aggregate_registers() {
    let aggregates = [
        LirInst::Load {
            src: "r".into(),
            dst: "2".into(),
            ty: t("#Pair"),
            span: test_span(),
        },
        LirInst::CopyReg {
            src: LirOperand::Place("p".into()),
            dst: "2".into(),
            ty: t("#Pair"),
            span: test_span(),
        },
        LirInst::CopyReg {
            src: LirOperand::Place("p".into()),
            dst: "2".into(),
            ty: t("[4 x #bit8]"),
            span: test_span(),
        },
        LirInst::Field {
            obj: "p".into(),
            name: "a".to_owned(),
            idx: 0,
            dst: "2".into(),
            ty: t("#bit32"),
            span: test_span(),
        },
    ];
    for (index, inst) in aggregates.into_iter().enumerate() {
        let errors = verify_messages(&pair_module(vec![inst]));
        // The `field` of a scalar is the control: it verifies.
        if index == 3 {
            assert!(errors.is_empty(), "{errors:?}");
        } else {
            assert!(
                errors.len() == 1 && errors[0].contains("aggregates live in places"),
                "{errors:?}"
            );
        }
    }
    let mut module = pair_module(Vec::new());
    module.data.push(LirData {
        name: "c0".into(),
        bytes: b"hi".to_vec(),
        span: test_span(),
    });
    module.funcs[0].blocks[0].insts.push(LirInst::Const {
        dst: "2".into(),
        value: LirConst::Data("c0".into()),
        ty: t("#byte_buffer"),
        span: test_span(),
    });
    let errors = verify_messages(&module);
    assert!(
        errors
            .iter()
            .any(|e| e.contains("aggregates live in places"))
            && errors.iter().any(|e| e.contains("needs `ref #bit8`")),
        "{errors:?}"
    );
}

/// `copy ref %r -> %p` reads an aggregate through a reference of its type.
#[test]
fn verify_checks_copy_deref() {
    let copy = |src: &str| LirInst::CopyDeref {
        src: src.into(),
        dst: "p".into(),
        span: test_span(),
    };
    assert!(verify_messages(&pair_module(vec![copy("r")])).is_empty());
    let module = pair_module(vec![copy("p")]);
    assert!(
        module.to_text().contains("copy ref %p -> %p"),
        "{}",
        module.to_text()
    );
    let errors = verify_messages(&module);
    assert!(
        errors.len() == 1 && errors[0].contains("needs `ref #Pair`, found `#Pair`"),
        "{errors:?}"
    );
    let errors = verify_messages(&pair_module(vec![copy("nowhere")]));
    assert!(
        errors
            .iter()
            .any(|e| e.contains("undeclared place `%nowhere`")),
        "{errors:?}"
    );
}

/// A global's initializer is runs over zero bytes: in order, apart, inside
/// the type, and printed with their offsets.
#[test]
fn global_initializer_runs_print_and_verify() {
    let global = |init: Vec<(u64, Vec<u8>)>| {
        let mut module = canonical_module();
        module.layouts.insert(
            "Pair".to_owned(),
            layout(&[("a", "#bit32"), ("b", "#bit8")]),
        );
        module.globals.push(LirGlobal {
            name: "g".into(),
            ty: t("#Pair"),
            init,
            addresses: Vec::new(),
            span: test_span(),
        });
        module
    };
    let module = global(Vec::new());
    assert!(module.to_text().contains("global !g: #Pair = zeroed\n"));
    assert!(verify_messages(&module).is_empty());
    let module = global(vec![(0, vec![1, 0, 0, 0]), (4, vec![2])]);
    assert!(
        module
            .to_text()
            .contains("global !g: #Pair = bytes { 0: \"\\x01\\x00\\x00\\x00\", 4: \"\\x02\" }\n"),
        "{}",
        module.to_text()
    );
    assert!(verify_messages(&module).is_empty());
    for bad in [
        vec![(4, vec![2]), (0, vec![1])],
        vec![(0, vec![1, 1]), (1, vec![1])],
        vec![(7, vec![1, 1])],
        vec![(0, Vec::new())],
    ] {
        let errors = verify_messages(&global(bad.clone()));
        assert!(
            errors.len() == 1 && errors[0].contains("initializer run"),
            "{bad:?}: {errors:?}"
        );
    }
}

/// A global's addresses print in offset order among its runs, and each is
/// a pointer-aligned slot inside the type, clear of every run and other
/// address, naming a function or global of the module.
#[test]
fn global_addresses_print_and_verify() {
    let func = || AddressTarget::Func("gen_function__bit32".into());
    let global = |init: Vec<(u64, Vec<u8>)>, addresses: Vec<(u64, AddressTarget)>| {
        let mut module = canonical_module();
        module.layouts.insert(
            "Table".to_owned(),
            layout(&[
                ("tag", "#bit32"),
                ("call", "ref #unknown"),
                ("next", "ref #unknown"),
            ]),
        );
        module.globals.push(LirGlobal {
            name: "table".into(),
            ty: t("#Table"),
            init,
            addresses: addresses
                .into_iter()
                .map(|(offset, target)| LirAddress { offset, target })
                .collect(),
            span: test_span(),
        });
        module
    };
    let module = global(
        vec![(0, vec![7])],
        vec![(8, func()), (16, AddressTarget::Global("table".into()))],
    );
    assert!(
        module.to_text().contains(
            "global !table: #Table = bytes { 0: \"\\x07\", 8: addr @gen_function__bit32, 16: addr !table }\n"
        ),
        "{}",
        module.to_text()
    );
    assert!(verify_messages(&module).is_empty());
    let module = global(Vec::new(), vec![(8, func())]);
    assert!(module
        .to_text()
        .contains("global !table: #Table = bytes { 8: addr @gen_function__bit32 }\n"));
    for (init, addresses) in [
        // Misaligned.
        (Vec::new(), vec![(4, func())]),
        // Past the end.
        (Vec::new(), vec![(24, func())]),
        // Out of order, and twice at one offset.
        (Vec::new(), vec![(16, func()), (8, func())]),
        (Vec::new(), vec![(8, func()), (8, func())]),
        // Over a run.
        (vec![(0, vec![1; 9])], vec![(8, func())]),
        (vec![(12, vec![1])], vec![(8, func())]),
    ] {
        let errors = verify_messages(&global(init.clone(), addresses.clone()));
        assert!(
            errors.len() == 1 && errors[0].contains("is misaligned, out of order, overlapping"),
            "{init:?} {addresses:?}: {errors:?}"
        );
    }
    for target in [
        AddressTarget::Func("missing".into()),
        AddressTarget::Global("missing".into()),
    ] {
        let errors = verify_messages(&global(Vec::new(), vec![(8, target.clone())]));
        assert!(
            errors.len() == 1 && errors[0].contains(&format!("names the unknown `{target}`")),
            "{target}: {errors:?}"
        );
    }
}
#[test]
fn builtin_layout_has_expected_fields() {
    let byte_buffer = builtin_fields(&t("#byte_buffer")).expect("#byte_buffer has fields");
    assert_eq!(byte_buffer[0], ("len".to_owned(), t("#bit_native")));
    let format_args = builtin_fields(&t("#format_args")).expect("#format_args has fields");
    assert_eq!(format_args.len(), 2);
    assert_eq!(format_args[1], ("holes".to_owned(), t("ref #unknown")));
    let target = lilac_target::Target::default();
    let format_args = builtin_layout(&t("#format_args"), &target).expect("a layout");
    assert_eq!((format_args.size, format_args.align), (16, 8));
    assert_eq!(format_args.fields[1].offset, 8);
    let frame = builtin_layout(&LirTy::trace_frame(), &target).expect("a layout");
    assert_eq!((frame.size, frame.fields[1].offset), (24, 8));
}

/// A `field` on a builtin aggregate resolves against `builtin_layout`
/// (these types never appear in `module.layouts`).
#[test]
fn field_on_byte_buffer_uses_builtin_layout() {
    let mut module =
        LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
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
        .insert("Number".to_owned(), layout(&[("tag", "#bit8")]));
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

    let mut module =
        LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
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
        .insert("Number".to_owned(), layout(&[("tag", "#bit8")]));
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

/// A formatter: `ref <value>, ref #byte_buffer` in, nothing out.
fn formatter(name: &str, value: &str) -> LirFunc {
    let mut func = trivial_func(name);
    func.ins = vec![
        ("value".into(), t(value)),
        ("out".into(), t("ref #byte_buffer")),
    ];
    func
}

/// `canonical_module` plus `!fmt0: format "a" {@fmt_a} "" {@fmt_b} "b"`,
/// which its function formats with `%x: #bit32` and `%y: #bit8`.
fn format_module() -> LirModule {
    let mut module = canonical_module();
    module.funcs.push(formatter("fmt_a", "ref #bit32"));
    module.funcs.push(formatter("fmt_b", "ref #bit8"));
    module.formats.push(LirFormat {
        name: "fmt0".into(),
        pieces: vec![b"a".to_vec(), Vec::new(), b"b".to_vec()],
        formats: vec!["fmt_a".into(), "fmt_b".into()],
        span: test_span(),
    });
    let alloca = |ty: &str, place: &str| LirInst::Alloca {
        ty: t(ty),
        place: place.into(),
        span: test_span(),
    };
    let addr = |place: &str, dst: &str, ty: &str| LirInst::Addr {
        place: place.into(),
        dst: dst.into(),
        ty: t(ty),
        span: test_span(),
    };
    module.funcs[0].blocks[0].insts.extend([
        alloca("#bit32", "x"),
        alloca("#bit8", "y"),
        addr("x", "2", "ref #bit32"),
        addr("y", "3", "ref #bit8"),
        alloca("#format_args", "f"),
        LirInst::Format {
            template: "fmt0".into(),
            holes: vec!["2".into(), "3".into()],
            dst: "f".into(),
            span: test_span(),
        },
    ]);
    module
}

/// Breaks one part of a valid module, for a verifier test.
type Breaker<T> = fn(&mut T);

/// The function's `format` instruction.
fn format_inst(module: &mut LirModule) -> &mut LirInst {
    module.funcs[0].blocks[0]
        .insts
        .iter_mut()
        .find(|inst| matches!(inst, LirInst::Format { .. }))
        .expect("a format")
}

#[test]
fn format_prints_as_the_fstring_reads_and_verifies() {
    let module = format_module();
    let text = module.to_text();
    assert!(
        text.contains("\n!fmt0: format \"a\" {@fmt_a} \"\" {@fmt_b} \"b\"\n"),
        "{text}"
    );
    assert!(
        text.contains("\n    format !fmt0 (&2, &3) -> %f\n"),
        "{text}"
    );
    assert_eq!(verify_messages(&module), Vec::<String>::new());
    let spanned = module.to_text_spanned();
    assert_eq!(spanned.text, text);
    assert_eq!(spanned.lines.len(), text.lines().count());
}

#[test]
fn verify_rejects_bad_templates() {
    let cases: Vec<(&str, Breaker<LirModule>)> = vec![
        ("one more piece than holes", |module| {
            module.formats[0].pieces.pop();
        }),
        ("unknown function", |module| {
            module.formats[0].formats[1] = "missing".into();
        }),
        ("must take `ref #T, ref #W`", |module| {
            module.funcs[1].outs = vec![("o".into(), t("#bit8"))];
        }),
        ("must take `ref #T, ref #W`", |module| {
            module.funcs[2].ins.pop();
        }),
        ("formats into both", |module| {
            module.funcs[2].ins[1].1 = t("ref #bit8");
        }),
        ("duplicate symbol `!fmt0`", |module| {
            module.data.push(LirData {
                name: "fmt0".into(),
                bytes: Vec::new(),
                span: test_span(),
            });
        }),
    ];
    for (expected, break_it) in cases {
        let mut module = format_module();
        break_it(&mut module);
        let errors = verify_messages(&module);
        assert!(
            errors.iter().any(|error| error.contains(expected)),
            "{expected}: {errors:?}"
        );
    }
}

#[test]
fn verify_rejects_bad_format_instructions() {
    let cases: Vec<(&str, Breaker<LirInst>)> = vec![
        ("has 2 holes, but `format` passes 1", |inst| {
            if let LirInst::Format { holes, .. } = inst {
                holes.pop();
            }
        }),
        ("takes `ref #bit32`", |inst| {
            if let LirInst::Format { holes, .. } = inst {
                holes.swap(0, 1);
            }
        }),
        ("undefined register", |inst| {
            if let LirInst::Format { holes, .. } = inst {
                holes[0] = "9".into();
            }
        }),
        ("undeclared template", |inst| {
            if let LirInst::Format { template, .. } = inst {
                *template = "fmt9".into();
            }
        }),
        ("expected `#format_args`", |inst| {
            if let LirInst::Format { dst, .. } = inst {
                *dst = "x".into();
            }
        }),
    ];
    for (expected, break_it) in cases {
        let mut module = format_module();
        break_it(format_inst(&mut module));
        let errors = verify_messages(&module);
        assert!(
            errors.iter().any(|error| error.contains(expected)),
            "{expected}: {errors:?}"
        );
    }
}

/// The template is the C struct `{ uintptr_t holes; struct { struct {
/// uintptr_t len; const uint8_t *data; } text; void (*format)(void); }
/// parts[holes + 1]; }`, at 64 and 32 bits.
#[test]
fn template_layout_is_the_c_struct() {
    for (triple, word) in [("x86_64-unknown-none", 8), ("i686-unknown-linux-gnu", 4)] {
        let target = lilac_target::Target::parse(triple).expect("triple");
        let part = builtin_layout(&LirTy::format_part(), &target).expect("a part");
        assert_eq!(part.size, 3 * word, "{triple}");
        assert_eq!(part.fields[1].offset, 2 * word, "{triple}");
        let template = format_template_layout(2, &target).expect("a template");
        assert_eq!(template.fields[1].offset, word, "{triple}");
        assert_eq!(template.size, word + 3 * part.size, "{triple}");
        assert_eq!(template.align, word, "{triple}");
        let args = builtin_layout(&LirTy::format_args(), &target).expect("an f-string");
        assert_eq!((args.size, args.fields[1].offset), (2 * word, word));
    }
}

#[test]
fn dce_keeps_a_live_template_and_its_formatters() {
    let mut module = format_module();
    module.funcs[0].name = "main".into();
    module.formats.push(LirFormat {
        name: "unused".into(),
        pieces: vec![Vec::new(), Vec::new()],
        formats: vec!["fmt_c".into()],
        span: test_span(),
    });
    module.funcs.push(formatter("fmt_c", "ref #bit8"));
    prune_unreachable(&mut module, &std::collections::HashSet::<String>::new());
    let funcs: Vec<&str> = module.funcs.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(funcs, ["main", "fmt_a", "fmt_b"]);
    let formats: Vec<&str> = module.formats.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(formats, ["fmt0"]);
    let graph = CallGraph::new(&module);
    assert_eq!(graph.callers("fmt_a")[0].kind, EdgeKind::Address);
    assert_eq!(verify_messages(&module), Vec::<String>::new());
}
