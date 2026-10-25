//! Checker tests: each runs the real pipeline front half (module, parse,
//! resolve, check) on a single in-memory file.

use std::fmt::Write as _;

use lilac_error::Diags;
use lilac_hir::{HirExprKind, HirProgram, HirStmt};
use lilac_span::SourceMap;
use lilac_ty::Types;

fn run_native(
    src: &str,
    native_bits: u32,
) -> (Diags, HirProgram, Types, SourceMap, lilac_resolve::Resolved) {
    let mut sm = SourceMap::new();
    let mut diags = Diags::new();
    let project = lilac_module::single_file("test.lilac", src, &mut sm);
    let (programs, _) = lilac_module::parse_all(&project, &mut diags, false);
    let res = lilac_resolve::resolve(&project, &programs, &mut diags);
    let mut types = Types::new();
    let options = crate::Options {
        target: target_of_width(native_bits),
        require_entry: false,
        cancel: None,
    };
    let checked = crate::check(&res, &mut types, &mut diags, options);
    (diags, checked.hir, types, sm, res)
}

/// A target whose `@bit_native` is `native_bits` wide: the default x86-64
/// one, or bare-metal 32-bit RISC-V.
fn target_of_width(native_bits: u32) -> lilac_target::Target {
    match native_bits {
        64 => lilac_target::Target::default(),
        32 => lilac_target::Target::parse("riscv32-unknown-none").expect("triple"),
        other => panic!("no target is {other} bits wide"),
    }
}

fn run(src: &str) -> (Diags, HirProgram, Types, SourceMap) {
    let (diags, hir, types, sm, _) = run_native(src, 64);
    (diags, hir, types, sm)
}

fn check(src: &str) -> (Diags, HirProgram, Types) {
    let (diags, hir, types, _) = run(src);
    (diags, hir, types)
}

fn check_src(src: &str) -> Diags {
    check_src_native(src, 64)
}

/// Like [`check_src`], but with an explicit target native width so the
/// `@bit_native` literal path can be exercised at 32 bits.
fn check_src_native(src: &str, native_bits: u32) -> Diags {
    run_native(src, native_bits).0
}

fn has_code(diags: &Diags, code: &str) -> bool {
    diags.diagnostics().iter().any(|d| d.code == code)
}

fn messages(diags: &Diags) -> Vec<String> {
    diags
        .diagnostics()
        .iter()
        .map(|d| d.msg.to_string())
        .collect()
}

fn render_all(diags: &Diags, sm: &SourceMap) -> String {
    use lilac_error::{Emitter, EmitterHuman};
    diags
        .diagnostics()
        .iter()
        .map(|diagnostic| EmitterHuman::new().render(diagnostic, sm))
        .collect()
}

/// The checked type and value of the literal in `main`'s first statement
/// (an integer in hex, a float as `FloatBits` displays it): a bare literal
/// statement takes no expected type, while `x: T = lit` takes `T`.
fn literal_type_and_bytes(src: &str) -> (String, String) {
    let (diags, hir, types, _, res) = run_native(src, 64);
    assert!(!diags.has_errors(), "{diags:?}");
    let main = hir
        .funcs
        .iter()
        .find(|f| res.def(f.key.def).name == "main")
        .expect("main");
    let body = main.body.as_ref().expect("main has a body");
    let value = match &body.stmts[0] {
        HirStmt::Expr(value) | HirStmt::Let { value, .. } => value,
        other => panic!("expected a value statement, got {other:?}"),
    };
    let bytes = match &value.kind {
        HirExprKind::Int(value) => format!("0x{value:X}"),
        HirExprKind::Float(bits) => bits.to_string(),
        other => panic!("expected a number literal, got {other:?}"),
    };
    (types.display(value.ty.id), bytes)
}

/// The checked type of the literal in `main`'s first statement.
fn literal_type(src: &str) -> String {
    literal_type_and_bytes(src).0
}

/// Numeric objects mirroring `lib/Standard`: one exact-width
/// `@conversion(from: <bit|fbit>)` per type, no widening pairs.
fn numeric_objects() -> &'static str {
    "Int8: structure { private { data: @bit8 } @conversion: function (from: @bit8) to: Int8 { to = Int8 { data = from } } @negate: function (value: Int8) to: Int8 { to = value } }\n\
     Int16: structure { private { data: @bit16 } @conversion: function (from: @bit16) to: Int16 { to = Int16 { data = from } } @negate: function (value: Int16) to: Int16 { to = value } }\n\
     Int32: structure { private { data: @bit32 } @conversion: function (from: @bit32) to: Int32 { to = Int32 { data = from } } @negate: function (value: Int32) to: Int32 { to = value } }\n\
     Int64: structure { private { data: @bit64 } @conversion: function (from: @bit64) to: Int64 { to = Int64 { data = from } } @negate: function (value: Int64) to: Int64 { to = value } }\n\
     Int: structure { private { data: @bit_native } @conversion: function (from: @bit_native) to: Int { to = Int { data = from } } @negate: function (value: Int) to: Int { to = value } }\n\
     Float32: structure { private { data: @fbit32 } @conversion: function (from: @fbit32) to: Float32 { to = Float32 { data = from } } }\n\
     Float64: structure { private { data: @fbit64 } @conversion: function (from: @fbit64) to: Float64 { to = Float64 { data = from } } }\n"
}

fn method_src() -> &'static str {
    "Object: structure {\n    member: @bit32\n    get: function (instance: ref Object) result: @bit32 {\n        result = instance.member\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    member: @bit32 = variable.get()\n}\n"
}

fn conversion_hook() -> &'static str {
    "@conversion: function (from: @bit32) to: @bit8 {\n    to = @truncate(from)\n}\n"
}

#[test]
fn integer_literal_types_by_smallest_fit() {
    for (src, want) in [
        ("0", "@bit8"),
        ("0xFF", "@bit8"),
        ("0x1FF", "@bit16"),
        ("65535", "@bit16"),
        ("65536", "@bit32"),
        ("0xFFFF_FFFF", "@bit32"),
        ("0x1_0000_0000", "@bit64"),
    ] {
        let source = format!("main: function () nothing {{\n    {src}\n}}\n");
        assert_eq!(literal_type(&source), want, "{src}");
    }
}

#[test]
fn integer_literal_grows_to_expected_width() {
    assert_eq!(
        literal_type("main: function () nothing {\n    x: @bit32 = 0x1\n}\n"),
        "@bit32"
    );
    assert_eq!(
        literal_type("main: function () nothing {\n    x: @bit32 = 1\n}\n"),
        "@bit32"
    );
    assert_eq!(
        literal_type("main: function () nothing {\n    x: @bit8 = 0xFF\n}\n"),
        "@bit8"
    );
}

#[test]
fn integer_literal_over_64_bits_is_e060() {
    let diags = check_src("main: function () nothing {\n    0x1_0000_0000_0000_0000\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
    let diags =
        check_src("main: function () nothing {\n    x: @bit64 = 0x1_0000_0000_0000_0000\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn integer_literal_overflow_of_expected_width_is_e060() {
    let diags = check_src("main: function () nothing {\n    x: @bit8 = 0x1FF\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn bit_native_literal_uses_target_width() {
    // `@bit_native` is the target's pointer width, not always 64: a value
    // needing 33 bits fits a 64-bit target but overflows a 32-bit one.
    let src = "main: function () nothing {\n    x: @bit_native = 0x1_0000_0000\n}\n";
    let diags = check_src_native(src, 64);
    assert!(!diags.has_errors(), "64-bit: {diags:?}");
    let diags = check_src_native(src, 32);
    assert!(has_code(&diags, "E060"), "32-bit: {diags:?}");
    // 32 bits exactly fits either target.
    let src = "main: function () nothing {\n    x: @bit_native = 0xFFFF_FFFF\n}\n";
    let diags = check_src_native(src, 32);
    assert!(!diags.has_errors(), "32-bit: {diags:?}");
    // A folded literal is range-checked at the native width too.
    let src = "main: function () nothing {\n    x: @bit_native = 0xFFFF_FFFF + 0x1\n}\n";
    let diags = check_src_native(src, 32);
    assert!(has_code(&diags, "E060"), "32-bit fold: {diags:?}");
}

#[test]
fn float_literal_defaults_to_fbit64() {
    for src in ["3.14", "1e3", "2E10", "1.5e-4"] {
        let source = format!("main: function () nothing {{\n    {src}\n}}\n");
        assert_eq!(literal_type(&source), "@fbit64", "{src}");
    }
}

#[test]
fn float_literal_pins_to_expected_fbit() {
    assert_eq!(
        literal_type("main: function () nothing {\n    x: @fbit32 = 3.14\n}\n"),
        "@fbit32"
    );
    let diags = check_src("main: function () nothing {\n    x: @fbit32 = 3.14\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn float_literal_in_bit_slot_is_e010() {
    let diags = check_src("main: function () nothing {\n    x: @bit64 = 3.14\n}\n");
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn float_literal_out_of_range_is_e060() {
    let diags = check_src("main: function () nothing {\n    x: @fbit16 = 100000.0\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn negative_literal_folds_to_twos_complement() {
    // Each is pinned to the signed-smallest-fit width so the folded value
    // is exactly the two's-complement bit pattern at that width.
    for (src, want_ty, want_bytes) in [
        ("-1", "@bit8", "0xFF"),
        ("-128", "@bit8", "0x80"),
        ("-129", "@bit16", "0xFF7F"),
        ("-32768", "@bit16", "0x8000"),
        ("-32769", "@bit32", "0xFFFF7FFF"),
        ("-0x8000_0000_0000_0000", "@bit64", "0x8000000000000000"),
    ] {
        let source = format!("main: function () nothing {{\n    x: {want_ty} = {src}\n}}\n");
        let (ty, bytes) = literal_type_and_bytes(&source);
        assert_eq!(ty, want_ty, "{src} type");
        assert_eq!(bytes, want_bytes, "{src} bytes");
    }
}

#[test]
fn negative_literal_grows_to_expected_width() {
    for (src, want_ty, want_bytes) in [
        ("x: @bit16 = -1", "@bit16", "0xFFFF"),
        ("x: @bit64 = -1", "@bit64", "0xFFFFFFFFFFFFFFFF"),
        ("x: @bit32 = -129", "@bit32", "0xFFFFFF7F"),
    ] {
        let source = format!("main: function () nothing {{\n    {src}\n}}\n");
        let (ty, bytes) = literal_type_and_bytes(&source);
        assert_eq!(ty, want_ty, "{src} type");
        assert_eq!(bytes, want_bytes, "{src} bytes");
    }
}

#[test]
fn negative_literal_overflow_is_e060() {
    // A signed value that does not fit the expected width.
    let diags = check_src("main: function () nothing {\n    x: @bit8 = -129\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
    // `@i_less_than_8` constrains `-1` to `@bit8`; `-129` needs 16.
    let diags =
        check_src("main: function () nothing {\n    x: @bit8 = @i_less_than_8(-129, 0)\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
    // Past the signed 64-bit range.
    let diags =
        check_src("main: function () nothing {\n    x: @bit64 = -0x8000_0000_0000_0001\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn negative_float_literal_folds_at_the_slot_type() {
    // Two float objects with `@negate`s: `-1.5` in an `@fbit32` slot must
    // not pick one of them; it is the negated literal.
    let src = "F16: structure {\n    data: @fbit16\n    @negate: function (value: F16) F16 => F16 { data = @f_negate_16(value.data) }\n    @conversion: function (from: @fbit16) F16 => F16 { data = from }\n}\n\
               F32: structure {\n    data: @fbit32\n    @negate: function (value: F32) F32 => F32 { data = @f_negate_32(value.data) }\n    @conversion: function (from: @fbit32) F32 => F32 { data = from }\n}\n\
               main: function () nothing {\n    a: @bit8 = @f_less_than_32(@bits_to_fbit_32(0x0), -1.5)\n    b: F16 = -2.0\n}\n";
    let diags = check_src(src);
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn negative_literal_needs_negate_on_a_numeric_object() {
    let src = "U: structure {\n    data: @bit8\n    @conversion: function (from: @bit8) U => U { data = from }\n}\n\
               main: function () nothing {\n    a: U = -1\n}\n";
    let diags = check_src(src);
    assert!(has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn negate_on_non_literal_is_unchanged() {
    // `@negate` on a non-literal still resolves the user hook.
    let src =
        "@negate: function (value: @bit32) result: @bit32 {\n    result = @i_negate_32(value)\n}\n\
               main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit32 = -a\n}\n";
    let diags = check_src(src);
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn literal_fold_operators() {
    // Hex operands so the pre-typeck decimal fold (still present until
    // Task 5) does not consume them first: each pair reaches this fold.
    for (src, want_ty, want_bytes) in [
        ("0x2 + 0x3", "@bit8", "0x5"),
        ("0xC8 + 0x64", "@bit16", "0x12C"),
        ("0x5 * 0x3", "@bit8", "0xF"),
        ("0x10 / 0x3", "@bit8", "0x5"),
        ("0x10 % 0x3", "@bit8", "0x1"),
        ("0x2 - 0x3", "@bit8", "0xFF"),
    ] {
        let source = format!("main: function () nothing {{\n    {src}\n}}\n");
        let (ty, bytes) = literal_type_and_bytes(&source);
        assert_eq!(ty, want_ty, "{src} type");
        assert_eq!(bytes, want_bytes, "{src} bytes");
    }
}

#[test]
fn nested_literal_fold() {
    // Folding is bottom-up: `2 * 3 + 4` parses as `(2 * 3) + 4`, so the
    // inner `*` must fold before the outer `+` (design §2/§7).
    let source = "main: function () nothing {\n    2 * 3 + 4\n}\n";
    let (ty, bytes) = literal_type_and_bytes(source);
    assert_eq!(ty, "@bit8");
    assert_eq!(bytes, "0xA");
}

#[test]
fn nested_literal_fold_grows_to_expected_width() {
    let source = "main: function () nothing {\n    x: @bit32 = 2 * 3 + 4\n}\n";
    let (ty, bytes) = literal_type_and_bytes(source);
    assert_eq!(ty, "@bit32");
    assert_eq!(bytes, "0xA");
}

#[test]
fn nested_literal_fold_negative_intermediate() {
    // `(2 - 5) * 2` folds the negative intermediate, then types by §5.
    let source = "main: function () nothing {\n    2 - 5 + 1\n}\n";
    let (ty, bytes) = literal_type_and_bytes(source);
    assert_eq!(ty, "@bit8");
    assert_eq!(bytes, "0xFE");
}

#[test]
fn nested_literal_fold_overflow_is_e060() {
    // `200 * 2` needs 9 bits: a nested overflow is still `E060`.
    let diags = check_src("main: function () nothing {\n    x: @bit8 = 200 * 2\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
    // An intermediate past 64 bits is `E060` even though `/ 2` would fit.
    let diags = check_src("main: function () nothing {\n    0xFFFF_FFFF_FFFF_FFFF * 2 / 2\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn nested_literal_fold_divide_by_zero_is_e060() {
    let diags = check_src("main: function () nothing {\n    2 * (3 / 0)\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn nested_literal_fold_mixed_kind_is_float() {
    // `2 * 3` folds as integers, then meets `4.5` and becomes a float.
    let source = "main: function () nothing {\n    2 * 3 + 4.5\n}\n";
    assert_eq!(
        literal_type_and_bytes(source),
        ("@fbit64".into(), "10.5".into())
    );
}

#[test]
fn foldable_literal_anchors_non_literal_width() {
    // A non-literal operand anchors the width, so the all-literal subtree
    // `(2 + 3)` must grow to `@bit32` (not its own `@bit8` fit) for the
    // `@bit32` `@mul` hook to resolve.
    let source = "@mul: function (lhs: @bit32, rhs: @bit32) result: @bit32 {\n    result = @i_mul_32(lhs, rhs)\n}\n\
                  main: function () nothing {\n    x: @bit32 = 2\n    y: @bit32 = x * (2 + 3)\n}\n";
    let diags = check_src(source);
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn literal_fold_grows_to_expected_width() {
    let source = "main: function () nothing {\n    x: @bit32 = 0x2 + 0x3\n}\n";
    let (ty, bytes) = literal_type_and_bytes(source);
    assert_eq!(ty, "@bit32");
    assert_eq!(bytes, "0x5");
    let source = "main: function () nothing {\n    x: @bit16 = 0xC8 + 0x64\n}\n";
    let (ty, bytes) = literal_type_and_bytes(source);
    assert_eq!(ty, "@bit16");
    assert_eq!(bytes, "0x12C");
}

#[test]
fn literal_fold_overflow_is_e060() {
    // 300 needs 9 bits; `@bit8` holds 8.
    let diags = check_src("main: function () nothing {\n    x: @bit8 = 0xC8 + 0x64\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
    // 2^64 does not fit any 64-bit container.
    let diags = check_src("main: function () nothing {\n    0xFFFF_FFFF_FFFF_FFFF + 0x1\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn literal_fold_divide_by_zero_is_e060() {
    for src in ["0x5 / 0x0", "0x5 % 0x0"] {
        let source = format!("main: function () nothing {{\n    {src}\n}}\n");
        let diags = check_src(&source);
        assert!(has_code(&diags, "E060"), "{src}: {diags:?}");
    }
}

#[test]
fn literal_fold_mixed_kind_is_float() {
    // An integer meeting a float literal is rounded to the float type, on
    // either side; the result is a float of the type the expression takes.
    for (src, value) in [
        ("5 + 2.5", "7.5"),
        ("2.5 - 5", "-2.5"),
        ("(1 - 3) * 1.5", "-3.0"),
    ] {
        let source = format!("main: function () nothing {{\n    {src}\n}}\n");
        assert_eq!(
            literal_type_and_bytes(&source),
            ("@fbit64".into(), value.into()),
            "{src}"
        );
    }
    let source = "main: function () nothing {\n    x: @fbit32 = 5 / 2.0\n}\n";
    assert_eq!(
        literal_type_and_bytes(source),
        ("@fbit32".into(), "2.5".into())
    );
    // A float result in an integer slot is still `E010`.
    let diags = check_src("main: function () nothing {\n    x: @bit32 = 5 + 2.5\n}\n");
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn literal_fold_float_operators() {
    let source = "main: function () nothing {\n    1.0 + 2.0\n}\n";
    let (ty, bytes) = literal_type_and_bytes(source);
    assert_eq!(ty, "@fbit64");
    let value: f64 = bytes.parse().expect("folded float parses");
    assert!((value - 3.0).abs() < f64::EPSILON, "{bytes}");
    let source = "main: function () nothing {\n    x: @fbit32 = 5.0 / 2.0\n}\n";
    let (ty, _) = literal_type_and_bytes(source);
    assert_eq!(ty, "@fbit32");
}

#[test]
fn literal_grows_through_object_conversion() {
    // A literal assigned to a numeric object grows to the object's single
    // conversion source width (never shrinks), so the exact-width
    // conversion applies: `Int16 = 5` uses `@bit16`, not `@bit8`.
    let src = format!(
        "{}main: function () nothing {{\n    a: Int8 = 5\n    b: Int16 = 5\n    c: Int32 = 5\n    d: Int64 = 5\n    e: Int = 5\n    f: Float32 = 3.14\n    g: Float64 = 3.14\n}}\n",
        numeric_objects()
    );
    let diags = check_src(&src);
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn literal_overflow_through_object_is_e060() {
    // `Int8`'s source is `@bit8`; `300` needs 9 bits, so it cannot shrink.
    let src = format!(
        "{}main: function () nothing {{\n    x: Int8 = 300\n}}\n",
        numeric_objects()
    );
    let diags = check_src(&src);
    assert!(has_code(&diags, "E060"), "{diags:?}");
    assert!(!has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn float_literal_into_float_object_compiles() {
    // `@fbitN -> FloatN` is the value conversion; a float literal pins to it.
    let src = format!(
        "{}main: function () nothing {{\n    f: Float32 = 3.14\n    g: Float64 = 3.14\n}}\n",
        numeric_objects()
    );
    let diags = check_src(&src);
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn integer_literal_into_float_object_is_not_a_bitcast() {
    // The float objects expose no `@bitN` conversion, so an integer literal
    // cannot silently reinterpret its bits; it is a clean conversion miss.
    for ty in ["Float32", "Float64"] {
        let src = format!(
            "{}main: function () nothing {{\n    x: {ty} = 3\n}}\n",
            numeric_objects()
        );
        let diags = check_src(&src);
        assert!(has_code(&diags, "E043"), "{ty}: {diags:?}");
    }
}

#[test]
fn float_literals_round_once_to_their_type() {
    for (src, want_ty, want_bits) in [
        ("x: @fbit16 = 0.1", "@fbit16", "0xH2E66"),
        ("x: @fbit16 = 65504.0", "@fbit16", "0xH7BFF"),
        ("x: @fbit32 = 0.1", "@fbit32", "0.1"),
        ("x: @fbit64 = 0.1", "@fbit64", "0.1"),
        // 113 bits of `0.1`, not 53 widened.
        (
            "x: @fbit128 = 0.1",
            "@fbit128",
            "0xQ3FFB999999999999999999999999999A",
        ),
        (
            "x: @fbit128 = 1.18973149535723176508575932662800702e4932",
            "@fbit128",
            "0xQ7FFEFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
        ),
    ] {
        let source = format!("main: function () nothing {{\n    {src}\n}}\n");
        let (ty, bits) = literal_type_and_bytes(&source);
        assert_eq!(ty, want_ty, "{src}");
        assert_eq!(bits, want_bits, "{src}");
    }
}

#[test]
fn float_literal_past_its_type_is_e060() {
    for src in [
        "x: @fbit16 = 65520.0",
        "x: @fbit32 = 3.5e38",
        "x: @fbit64 = 1.8e308",
        "x: @fbit128 = 1.2e4932",
        "x: @fbit16 = 70000.0 - 60000.0",
    ] {
        let diags = check_src(&format!("main: function () nothing {{\n    {src}\n}}\n"));
        assert!(has_code(&diags, "E060"), "{src}: {diags:?}");
    }
}

#[test]
fn float_folds_at_its_type() {
    for (src, want) in [
        // At `@fbit64`, `0.1 + 0.2` is not `0.3`.
        ("x: @fbit64 = 0.1 + 0.2", "0.30000000000000004"),
        ("x: @fbit32 = 0.1 + 0.2", "0.3"),
        // `1 + 0.4 ulp` rounds to `1.0` at 24 bits before `0.2 ulp` is
        // added; at 53 bits the sum would be `1 + 0.6 ulp`, then `1.0000001`.
        (
            "x: @fbit32 = 1.0000000476837158 + 0.00000002384185791015625",
            "1.0",
        ),
        ("x: @fbit32 = 16777216.0 + 1.0", "16777216.0"),
        ("x: @fbit16 = 2048.0 + 1.0", "0xH6800"),
        ("x: @fbit16 = 1.0 / 3.0", "0xH3555"),
        (
            "x: @fbit128 = 0.1 + 0.2",
            "0xQ3FFD3333333333333333333333333334",
        ),
        ("x: @fbit64 = 5.5 % 2.0", "1.5"),
    ] {
        let source = format!("main: function () nothing {{\n    {src}\n}}\n");
        let (_, bits) = literal_type_and_bytes(&source);
        assert_eq!(bits, want, "{src}");
    }
    for src in ["1.0 / 0.0", "0.0 / 0.0", "1.0 % 0.0"] {
        let diags = check_src(&format!("main: function () nothing {{\n    {src}\n}}\n"));
        assert!(has_code(&diags, "E060"), "{src}: {diags:?}");
    }
    let diags = check_src("main: function () nothing {\n    x: @fbit16 = 60000.0 + 60000.0\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn literal_patterns_check_against_the_scrutinee() {
    let body = |scrutinee: &str, pattern: &str| {
        format!(
            "main: function () nothing {{\n    x: {scrutinee} = 0x0\n    match x {{\n        {pattern} => {{}}\n    }} else {{}}\n}}\n"
        )
    };
    for (scrutinee, pattern) in [
        ("@bit8", "0xFF"),
        ("@bit8", "'a'"),
        ("@bit32", "'\u{1F600}'"),
        ("@bit64", "0xFFFF_FFFF_FFFF_FFFF"),
    ] {
        let diags = check_src(&body(scrutinee, pattern));
        assert!(!diags.has_errors(), "{scrutinee} {pattern}: {diags:?}");
    }
    for (scrutinee, pattern, code) in [
        ("@bit8", "0x100", "E060"),
        ("@bit8", "'\u{100}'", "E060"),
        ("@bit16", "'\u{1F600}'", "E060"),
        ("@bit32", "1.5", "E010"),
        ("@bit32", "\"a\"", "E010"),
    ] {
        let diags = check_src(&body(scrutinee, pattern));
        assert!(has_code(&diags, code), "{scrutinee} {pattern}: {diags:?}");
    }
}

#[test]
fn negative_literal_grows_through_object_conversion() {
    // A folded negative literal assigned to a numeric object grows to the
    // object's single `@conversion(from: @bitN)` source width, exactly as
    // a positive integer literal does (§4/§5).
    let src = format!(
        "{}main: function () nothing {{\n    a: Int8 = -1\n    b: Int16 = -1\n    c: Int32 = -1\n    d: Int64 = -1\n    e: Int = -1\n}}\n",
        numeric_objects()
    );
    let diags = check_src(&src);
    assert!(!diags.has_errors(), "{diags:?}");
    // `Int8`'s source is `@bit8`; `-129` needs a signed 16-bit width.
    let src = format!(
        "{}main: function () nothing {{\n    x: Int8 = -129\n}}\n",
        numeric_objects()
    );
    let diags = check_src(&src);
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn mismatch_without_conversion_is_e043() {
    // Task 5: without a binding, a type mismatch is a failed conversion
    // search. The diagnostic still shows both sides.
    let diags =
        check_src("main: function () nothing {\n    y: @bit32 = 0x1\n    x: @bit8 = y\n}\n");
    assert!(has_code(&diags, "E043"), "{diags:?}");
    let msgs = messages(&diags);
    assert!(
        msgs.iter()
            .any(|m| m.contains("@bit8") && m.contains("@bit32")),
        "{msgs:?}"
    );
}

#[test]
fn unresolved_name_is_e021() {
    let diags = check_src("main: function () nothing {\n    x: @bit8 = nope\n}\n");
    assert!(has_code(&diags, "E021"), "{diags:?}");
}

#[test]
fn arity_mismatch_is_e022() {
    let diags = check_src(
        "get: function () result: @bit32 {\n    result = 0x1\n}\nmain: function () nothing {\n    x: @bit8 = get(0x1)\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn bare_literal_is_typed_not_e024() {
    let diags = check_src("main: function () nothing {\n    0x1\n}\n");
    assert!(!has_code(&diags, "E024"), "{diags:?}");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn locals_shadow_globals() {
    let diags = check_src(
        "get: function () result: @bit32 {\n    result = 0x1\n}\nmain: function () nothing {\n    g: @bit32 = get()\n    x: @bit8 = g()\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
    assert_eq!(diags.diagnostics().len(), 1, "{diags:?}");
}

#[test]
fn equality_needs_same_width() {
    let diags = check_src(
        "main: function () nothing {\n    a: @bit8 = 0x1\n    b: @bit32 = 0x2\n    c: @bit8 = a == b\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
    let e010: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|d| d.code == "E010")
        .collect();
    assert_eq!(e010.len(), 1, "{diags:?}");
    assert_eq!(e010[0].labels.len(), 2, "{diags:?}");
}

#[test]
fn cmp_operands_ignore_expected_result_width() {
    // Return position expects `@bit8`; operands are `@bit32`.
    let diags = check_src("f: function (a: @bit32, b: @bit32) @bit8 => @i_less_than_32(a, b)\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn width_suffixed_numeric_intrinsics() {
    // An exact-width integer add typechecks; literals adopt the width.
    let diags = check_src("main: function () nothing {\n    x: @bit8 = @i_add_8(0x1, 0x2)\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
    // A width mismatch is `E010`, with no auto-unification.
    let diags = check_src(
        "main: function () nothing {\n    a: @bit16 = 0x1\n    b: @bit16 = 0x2\n    x: @bit16 = @i_add_8(a, b)\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
    // Float ops require real `@fbit32` operands, not integer containers.
    let diags = check_src(
        "main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit32 = 0x2\n    x: @fbit32 = @f_add_32(a, b)\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn old_unsuffixed_intrinsics_are_gone() {
    // The unsuffixed names are gone. The name is assembled so this source
    // itself does not contain the retired spelling.
    let old = ["@i", "_add"].concat();
    let src = format!("main: function () nothing {{\n    x: @bit8 = {old}(0x1, 0x2)\n}}\n");
    let diags = check_src(&src);
    assert!(diags.has_errors(), "{diags:?}");
}

/// `@panic` takes a `@byte_buffer` message.
#[test]
fn panic_wants_a_byte_buffer() {
    let ok = check_src("main: function () nothing {\n    @panic(\"stop\")\n}\n");
    assert!(!ok.has_errors(), "{ok:?}");
    let bad = check_src("main: function () nothing {\n    @panic(0x1)\n}\n");
    assert!(bad.has_errors(), "{bad:?}");
}

#[test]
fn bitcast_intrinsics_round_trip() {
    // `@fbit_to_bits_W(@bits_to_fbit_W(x))` typechecks at every width that
    // has a bit container (`@bit16/32/64`).
    for (w, bit, fbit) in [
        ("16", "@bit16", "@fbit16"),
        ("32", "@bit32", "@fbit32"),
        ("64", "@bit64", "@fbit64"),
    ] {
        let diags = check_src(&format!(
            "main: function () nothing {{\n    x: {bit} = 0x1\n    f: {fbit} = @bits_to_fbit_{w}(x)\n    y: {bit} = @fbit_to_bits_{w}(f)\n}}\n"
        ));
        assert!(!diags.has_errors(), "{w}: {diags:?}");
    }
}

#[test]
fn bitcast_wrong_source_is_e010() {
    // `@bits_to_fbit_32` needs an `@bit32` operand.
    let diags = check_src(
        "main: function (x: @bit16) nothing {\n    f: @fbit32 = @bits_to_fbit_32(x)\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
    // `@fbit_to_bits_64` needs an `@fbit64` operand.
    let diags = check_src(
        "main: function (f: @fbit32) nothing {\n    y: @bit64 = @fbit_to_bits_64(f)\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn ref_mismatch_labels_both_sides() {
    let diags = check_src(
        "set: function (r: ref @bit8) nothing {\n}\nmain: function () nothing {\n    v: @bit8 = 0x1\n    set(v)\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
    let e010: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|d| d.code == "E010")
        .collect();
    assert_eq!(e010.len(), 1, "{diags:?}");
    assert_eq!(e010[0].labels.len(), 2, "{diags:?}");
    assert!(
        e010[0].msg.contains("ref @bit8") && e010[0].msg.contains("@bit8"),
        "{diags:?}"
    );
}

#[test]
fn enum_tag_validates() {
    let diags = check_src(
        "Maybe: either {\n    Some: @bit8\n    None\n}\nmain: function () nothing {\n    x: Maybe = .None\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn hex_literal_takes_expected_bit_type() {
    let diags = check_src("main: function () nothing {\n    x: @bit8 = 0x1\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn decimal_literal_is_a_bit_value() {
    let diags = check_src("main: function () nothing {\n    x: @bit8 = 5\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn hex_literal_overflow_is_e060() {
    let diags = check_src("main: function () nothing {\n    x: @bit8 = 0x1FF\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn hex_literal_over_u64_is_e060() {
    let diags =
        check_src("main: function () nothing {\n    x: @bit8 = 0x1_0000_0000_0000_0000\n}\n");
    assert!(has_code(&diags, "E060"), "{diags:?}");
}

#[test]
fn decimal_literals_adopt_intrinsic_width() {
    let diags = check_src("main: function () nothing {\n    x: @bit32 = @i_add_32(5, 6)\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn poison_types_never_panic() {
    // Unknown parameter types poison the signature; calling with a
    // literal must report E023, never crash the compiler.
    let diags = check_src(
        "foo: function (x: Nope) nothing {\n}\nmain: function () nothing {\n    foo(0x1)\n}\n",
    );
    assert!(has_code(&diags, "E023"), "{diags:?}");
}

#[test]
fn method_call_checks() {
    let diags = check_src(method_src());
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn unknown_member_is_e021_not_e040() {
    let diags = check_src(
        "Object: structure {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    variable_2: @bit8 = variable.data\n}\n",
    );
    assert!(has_code(&diags, "E021"), "{diags:?}");
    assert!(!has_code(&diags, "E040"), "{diags:?}");
}

#[test]
fn calling_a_field_is_e022() {
    let diags = check_src(
        "Object: structure {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    variable.member()\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn multilet_clean() {
    let diags = check_src(
        "get_multi: function () condition: @bit32, result: @bit32 {\n    condition = 0x1\n    result = 0x2\n}\nmain: function () nothing {\n    condition: @bit32, result: @bit32 = get_multi()\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn multilet_arity_mismatch_is_e022() {
    let diags = check_src(
        "get_multi: function () condition: @bit32, result: @bit32 {\n    condition = 0x1\n    result = 0x2\n}\nmain: function () nothing {\n    a: @bit32, b: @bit32, c: @bit32 = get_multi()\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
    let msgs = messages(&diags);
    assert!(
        msgs.iter().any(|m| m.contains('3') && m.contains('2')),
        "{msgs:?}"
    );
}

const TWO_OUTS: &str = "get: function () a: @bit32, b: @bit8 {\n    a = 0x1\n    b = 0x2\n}\n";

#[test]
fn unbound_outs_are_dropped() {
    // A call statement, a call as a value and a short multi-`let`.
    let diags = check_src(&format!(
        "{TWO_OUTS}three: function () a: @bit32, b: @bit8, c: @bit8 {{\n    a = 0x1\n    b = 0x2\n    c = 0x3\n}}\nmain: function () nothing {{\n    get()\n    first: @bit32 = get()\n    x: @bit32 = @i_add_32(get(), 0x1)\n    p: @bit32, q: @bit8 = three()\n}}\n"
    ));
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn more_bindings_than_outs_is_e022() {
    let diags = check_src(&format!(
        "{TWO_OUTS}main: function () nothing {{\n    p: @bit32, q: @bit8, r: @bit8 = get()\n}}\n"
    ));
    assert!(has_code(&diags, "E022"), "{diags:?}");
    let msgs = messages(&diags);
    assert!(
        msgs.iter()
            .any(|m| m == "3 values are bound, but the call returns 2"),
        "{msgs:?}"
    );
}

#[test]
fn values_with_no_effect_are_recorded_not_errors() {
    let src = "E: either {\n    A\n}\nmain: function () nothing {\n    x: @bit32 = 0x1\n    0x5 + 0x5\n    x\n    @size_of(@bit32)\n    E.A\n    @trap()\n}\n";
    let (diags, hir, _) = check(src);
    assert!(!diags.has_errors(), "{diags:?}");
    let texts: Vec<&str> = hir
        .no_effect
        .iter()
        .map(|span| &src[span.lo.0 as usize..span.hi.0 as usize])
        .collect();
    assert_eq!(texts, ["0x5 + 0x5", "x", "@size_of(@bit32)", "E.A"]);
}

#[test]
fn value_from_nothing_is_e022() {
    let diags = check_src(
        "none: function () nothing {\n}\nmain: function () nothing {\n    x: @bit32 = none()\n}\n",
    );
    let msgs = messages(&diags);
    assert!(
        msgs.iter().any(|m| m == "this call returns no value"),
        "{msgs:?}"
    );
}

#[test]
fn structlit_missing_field_is_e022() {
    let diags = check_src(
        "Object: structure {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n    }\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
    let msgs = messages(&diags);
    assert!(msgs.iter().any(|m| m.contains("member")), "{msgs:?}");
}

#[test]
fn structlit_unknown_field_is_e021() {
    let diags = check_src(
        "Object: structure {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n        data = 0x1\n    }\n}\n",
    );
    assert!(has_code(&diags, "E021"), "{diags:?}");
}

#[test]
fn func_value_call_checks() {
    let diags = check_src(
        "main: function () nothing {\n    add: ref function (@bit32) @bit32 = ref addOne\n    value: @bit32 = 0x0\n    value = add(value)\n    add = ref addTwo\n    value = add(value)\n}\naddOne: function (value: @bit32) result: @bit32 {\n    result = @i_add_32(value, 0x1)\n}\naddTwo: function (value: @bit32) result: @bit32 {\n    result = @i_add_32(value, 0x2)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

const GENERIC_ITEMS: &str = "id[T]: function (value: T) out: T {\n    out = value\n}\nBox[T]: structure {\n    value: T\n    make: function (value: T) out: Box[T] {\n        out = Box[T] { value = value }\n    }\n    convert[U]: function (self, extra: U) out: U {\n        out = extra\n    }\n}\nPlain: structure {\n    count: @bit8\n    pick[T]: function (first: T, second: T) out: T {\n        out = second\n    }\n    picker: function () out: ref function (first: @bit8, second: @bit8) @bit8 {\n        out = ref pick[@bit8]\n    }\n}\nG: @bit8 = 0x1\n";

#[test]
fn generic_instances_are_function_values() {
    let diags = check_src(&format!(
        "{GENERIC_ITEMS}main: function () nothing {{\n    a: ref function (value: @bit32) @bit32 = ref id[@bit32]\n    b: ref function (value: @bit8) Box[@bit8] = ref Box[@bit8].make\n    c: ref function (first: @bit8, second: @bit8) @bit8 = ref Plain.pick[@bit8]\n    d: ref function (self: ref Box[@bit8], extra: @bit32) @bit32 = ref Box[@bit8].convert[@bit32]\n}}\nlater[T]: function (value: T) out: T {{\n    f: ref function (value: T) T = ref id[T]\n    out = f(value)\n}}\n"
    ));
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn generic_instance_errors() {
    let cases = [
        // No arguments, too many, and arguments for what takes none.
        ("ref id", "E044"),
        ("ref id[@bit32, @bit8]", "E044"),
        ("ref Box[@bit8].make[@bit8]", "E044"),
        ("ref Box.make", "E044"),
        ("ref G[@bit8]", "E044"),
        ("ref Box[@bit8].convert", "E044"),
        // Not behind `ref`.
        ("id[@bit32]", "E077"),
        ("Box[@bit8].convert[@bit32]", "E077"),
        // A type, a missing name, a bound method.
        ("ref Box[@bit8]", "E021"),
        ("ref Missing[@bit8]", "E021"),
        ("ref Plain { count = 0x1 }.pick[@bit8]", "E010"),
    ];
    for (value, code) in cases {
        let diags = check_src(&format!(
            "{GENERIC_ITEMS}main: function () nothing {{\n    f: ref unknown = {value} as ref unknown\n}}\n"
        ));
        assert_eq!(codes_of(&diags), [code], "{value}: {diags:?}");
    }
}

#[test]
fn recursive_inline_is_allowed() {
    let diags = check_src(
        "f: function (x: @bit32) @bit32 => f(x)\nmain: function () nothing {\n    y: @bit32 = f(0x1)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn static_call_checks() {
    let diags = check_src(
        "Object: structure {\n    member: @bit32\n    add: function (num: @bit32) result: @bit32 {\n        result = @i_add_32(num, 0x1)\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    member: @bit32 = Object.add(0x2)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn method_without_self_is_e022() {
    let diags = check_src(
        "Object: structure {\n    member: @bit8\n    add: function (num: @bit32) result: @bit32 {\n        result = num\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    member: @bit32 = variable.add(0x2)\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn declaration_shorthand_calls_a_static_function() {
    // `variable: Object.create(0x1)` is `variable: Object = Object.create(0x1)`,
    // and its arguments convert like any call's.
    let diags = check_src(
        "Object: structure {\n    value: @bit32\n    create: function (value: @bit32) result: Object {\n        result = Object { value = value }\n    }\n}\n@conversion: function (from: @bit8) to: @bit32 { to = @extend_zero(from) }\nmain: function () nothing {\n    b: @bit8 = 0x2\n    variable: Object.create(b)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn declaration_shorthand_method_is_e056() {
    let diags = check_src(
        "Object: structure {\n    value: @bit8\n    copy: function (self) result: Object {\n        result = Object { value = self.value }\n    }\n}\nmain: function () nothing {\n    a: Object { value = 0x1 }\n    b: Object.copy(ref a)\n}\n",
    );
    assert!(has_code(&diags, "E056"), "{diags:?}");
}

#[test]
fn declaration_shorthand_variant_is_e056() {
    let diags = check_src(
        "Shape: either {\n    Dot\n}\nmain: function () nothing {\n    s: Shape.Dot()\n}\n",
    );
    assert!(has_code(&diags, "E056"), "{diags:?}");
}

#[test]
fn calling_a_type_is_e056() {
    let diags = check_src(
        "Object: structure {\n    value: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object(0x1)\n}\n",
    );
    assert!(has_code(&diags, "E056"), "{diags:?}");
}

#[test]
fn init_hook_is_e056() {
    let diags = check_src(
        "Object: structure {\n    value: @bit8\n    @init: function (value: @bit8) result: Object {\n        result = Object { value = value }\n    }\n}\nmain: function () nothing {\n}\n",
    );
    assert!(has_code(&diags, "E056"), "{diags:?}");
}

#[test]
fn on_drop_bad_shape_is_e022() {
    let diags = check_src(
        "Object: structure {\n    member: @bit8\n    @on_drop: function (member: @bit8) nothing {\n    }\n}\nmain: function () nothing {\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn as_cast_without_conversion_is_e043() {
    let diags = check_src(
        "main: function () nothing {\n    a: @bit8 = 0x1\n    b: @bit16 = a as @bit16\n}\n",
    );
    assert!(has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn as_cast_literal_pins_to_target() {
    assert_eq!(
        literal_type("main: function () nothing {\n    0x1 as @bit16\n}\n"),
        "@bit16"
    );
    assert_eq!(
        literal_type("main: function () nothing {\n    3.14 as @fbit32\n}\n"),
        "@fbit32"
    );
}

#[test]
fn as_cast_ref_mismatch_is_e010() {
    // A non-`unknown` ref mismatch is not the §6 exception.
    let diags = check_src(
        "main: function (p: ref @bit8) nothing {\n    q: ref @bit16 = p as ref @bit16\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
    assert!(!has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn as_cast_ref_to_nonref_is_e010() {
    // `ref T as T` is not the §6 exception (ref-to-ref only): a `ref`
    // never silently becomes a non-`ref` value, so this is `E010`.
    let diags =
        check_src("main: function (p: ref @bit8) nothing {\n    q: @bit8 = p as @bit8\n}\n");
    assert!(has_code(&diags, "E010"), "{diags:?}");
    assert!(!has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn if_condition_must_be_bit8() {
    let diags =
        check_src("main: function () nothing {\n    x: @bit8 = 0x1\n    if x {\n    }\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
    let diags =
        check_src("main: function () nothing {\n    x: @bit32 = 0x1\n    if x {\n    }\n}\n");
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn if_condition_takes_one_conversion() {
    let diags = check_src(
        "Flag: structure {\n    v: @bit8\n    @conversion: function (from: Flag) to: @bit8 {\n        to = from.v\n    }\n}\nmain: function () nothing {\n    f: Flag = Flag { v = 0x1 }\n    if f {\n    }\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

/// A function that can fail, for the failure tests: `halve` fails on an
/// odd number.
const HALVE: &str = "halve: function (n: @bit32) half: @bit32, rest: @bit32 else odd: @bit32 {\n    if n == 0x3 {\n        odd = n\n        fail\n    }\n    half = n\n    rest = 0x0\n}\n";

fn check_failing(main: &str) -> Diags {
    check_src(&format!("{HALVE}main: function () nothing {{\n{main}}}\n"))
}

#[test]
fn captures_bind_by_position() {
    let diags = check_failing(
        "    if halve(0x2) -> a: @bit32, b: @bit32 {\n    } else -> why: @bit32 {\n    }\n    c: @bit32 = halve(0x2) else -> d: @bit32 {\n        return\n    }\n    loop if halve(0x2) -> e: @bit32 {\n    } else break\n    halve(0x2) else {\n        return\n    }\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn unhandled_failure_is_e057() {
    for main in [
        "    a: @bit32 = halve(0x2)\n",
        "    halve(0x2)\n",
        "    a: @bit32, b: @bit32 = halve(0x2)\n",
        "    if halve(halve(0x2)) -> a: @bit32 {\n    }\n",
    ] {
        let diags = check_failing(main);
        assert!(has_code(&diags, "E057"), "{main}: {diags:?}");
    }
}

#[test]
fn failing_functions_are_not_values_or_types() {
    let diags = check_failing("    f: function (n: @bit32) half: @bit32 = halve\n");
    assert!(has_code(&diags, "E057"), "{diags:?}");
    let diags = check_src("run: function (f: function () else nothing) nothing {\n}\n");
    assert!(has_code(&diags, "E057"), "{diags:?}");
    let diags = check_src("write: extern function () value: @bit32 else nothing\n");
    assert!(has_code(&diags, "E028"), "{diags:?}");
}

#[test]
fn fail_outside_a_failing_function_is_e058() {
    let diags = check_src("main: function () nothing {\n    fail\n}\n");
    assert!(has_code(&diags, "E058"), "{diags:?}");
}

#[test]
fn guard_else_must_leave() {
    let diags = check_failing("    a: @bit32 = halve(0x2) else {\n    }\n");
    assert!(has_code(&diags, "E059"), "{diags:?}");
    let diags = check_failing(
        "    x: @bit8 = 0x1\n    a: @bit32 = halve(0x2) else {\n        if x {\n            return\n        } else {\n            @trap()\n        }\n    }\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn capture_mismatch_is_e062() {
    // More bindings than outs.
    for main in [
        "    a: @bit32, b: @bit32, c: @bit32 = halve(0x2) else {
        return
    }
",
        "    if halve(0x2) -> a: @bit32 {
    } else -> b: @bit32, c: @bit32 {
    }
",
    ] {
        let diags = check_failing(main);
        assert!(has_code(&diags, "E062"), "{main}: {diags:?}");
    }
    // Captures or a guard on a call that cannot fail.
    for main in [
        "    if plain() -> a: @bit32 {
    }
",
        "    a: @bit32 = plain() else {
        return
    }
",
    ] {
        let diags = check_src(&format!(
            "plain: function () value: @bit32 {{
    value = 0x1
}}
main: function () nothing {{
{main}}}
"
        ));
        assert!(has_code(&diags, "E062"), "{main}: {diags:?}");
    }
}

#[test]
fn stray_break_is_e022() {
    let diags = check_src("main: function () nothing {\n    break\n}\n");
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn loop_if_checks() {
    let diags =
        check_src("main: function () nothing {\n    loop if 0x0 {\n    } else {\n    }\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn if_value_branch_mismatch_is_e043() {
    let diags = check_src(
        "main: function () nothing {\n    a: @bit32 = 0x1\n    x: @bit8 = if 0x1 then a else 0x0\n}\n",
    );
    assert!(has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn match_tag_capture_payload() {
    let diags = check_src(
        "OptionalBit32: either {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    number: OptionalBit32 = .Some(0x1)\n    match number {\n        .Some -> some: @bit32 {\n        }\n        .None => return\n    }\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn match_capture_mismatch_is_e010() {
    let diags = check_src(
        "OptionalBit32: either {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    number: OptionalBit32 = .Some(0x1)\n    match number {\n        .Some -> some: @bit16 {\n        }\n        .None => return\n    }\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn capture_on_bare_tag_is_e022() {
    let diags = check_src(
        "OptionalBit32: either {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    number: OptionalBit32 = .None\n    match number {\n        .Some -> some: @bit32 {\n        }\n        .None -> x: @bit8 {\n        }\n    }\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn match_value_unifies_branches() {
    let diags = check_src(
        "main: function () nothing {\n    variable: @bit32 = match 0x0 {\n        0x1 => 0x1\n    } else 0x3\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn and_or_require_bit8() {
    let diags = check_src(
        "main: function () nothing {\n    y: @bit32 = 0x2\n    x: @bit8 = 0x1 and y\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn compile_error_recorded_silent() {
    let diags = check_src(
        "main: function () nothing {\n    if 0x1 {\n    } else {\n        @compile_error(\"dead\")\n    }\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn compile_error_value_position_silent() {
    // Folding (Task 9) decides liveness; checking stays silent.
    let diags =
        check_src("main: function () nothing {\n    x: @bit32 = @compile_error(\"boom\")\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn loop_break_checks() {
    let diags = check_src("main: function () nothing {\n    loop {\n        break\n    }\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

/// A writer type with a `@format` for `@bit32`, before each f-string test.
const FORMAT_PRELUDE: &str = "Out: structure {}\n\n@format: function (value: ref @bit32, out: ref Out) nothing {}\n\nnumber: function () value: @bit32 {\n    value = 0x1\n}\n\n";

fn check_format(body: &str) -> Diags {
    check_src(&format!("{FORMAT_PRELUDE}{body}"))
}

#[test]
fn fstring_takes_its_writer_from_the_destination() {
    let diags = check_format(
        "main: function () nothing {\n    n: @bit32 = 0x1\n    x: @format_args(Out) = $\"a{n}b{number()}\"\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn fstring_with_nothing_expected_is_e073() {
    let diags = check_format("main: function () nothing {\n    $\"a\"\n}\n");
    assert!(has_code(&diags, "E073"), "{diags:?}");
}

#[test]
fn fstring_into_a_type_with_no_format_conversion_is_e043() {
    let diags = check_format("main: function () nothing {\n    x: @bit32 = $\"a\"\n}\n");
    assert!(has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn fstring_converts_through_the_one_format_conversion() {
    let diags = check_format(
        "Text: structure {\n    @conversion: function (from: @format_args(Out)) to: Text {\n        to = Text {}\n    }\n}\n\nmain: function () nothing {\n    n: @bit32 = 0x1\n    x: Text = $\"{n}\"\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn hole_with_no_format_hook_is_e071() {
    let diags = check_format(
        "main: function () nothing {\n    b: @bit8 = 0x1\n    x: @format_args(Out) = $\"{b}\"\n}\n",
    );
    assert!(has_code(&diags, "E071"), "{diags:?}");
}

#[test]
fn format_slot_takes_a_literal_or_one_value() {
    let diags = check_format(
        "show: function (text: @format_args(Out)) nothing {}\n\nmain: function () nothing {\n    n: @bit32 = 0x1\n    show(\"plain\")\n    show(n)\n    show($\"{n}\")\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
    let diags = check_format(
        "show: function (text: @format_args(Out)) nothing {}\n\nmain: function () nothing {\n    b: @bit8 = 0x1\n    show(b)\n}\n",
    );
    assert!(has_code(&diags, "E071"), "{diags:?}");
}

#[test]
fn a_nested_fstring_hole_formats_through_the_same_writer() {
    let diags = check_format(
        "@format: function (value: ref @format_args(Out), out: ref Out) nothing {}\n\nmain: function () nothing {\n    n: @bit32 = 0x1\n    x: @format_args(Out) = $\"<{$\"{n}\"}>\"\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn forty_holes_check() {
    let holes = "{n}".repeat(40);
    let diags = check_format(&format!(
        "main: function () nothing {{\n    n: @bit32 = 0x1\n    x: @format_args(Out) = $\"{holes}\"\n}}\n"
    ));
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn bad_format_hook_shape_is_e022() {
    for hook in [
        "@format: function (value: ref @bit32) nothing {}",
        "@format: function (value: @bit32, out: ref Out) nothing {}",
        "@format: function (value: ref unknown, out: ref Out) nothing {}",
        "@format: function (value: ref @bit8, out: ref Out) done: @bit8 {\n    done = 0x1\n}",
    ] {
        let diags = check_format(&format!("{hook}\n\nmain: function () nothing {{}}\n"));
        assert!(has_code(&diags, "E022"), "{hook}: {diags:?}");
    }
}

#[test]
fn a_format_where_it_cannot_live_is_e072() {
    for (what, src) in [
        ("field", "Holder: structure {\n    text: @format_args(Out)\n}\n\nmain: function () nothing {}\n"),
        ("payload", "Choice: either {\n    Full: @format_args(Out)\n}\n\nmain: function () nothing {}\n"),
        ("array", "main: function () nothing {\n    x: @array(@format_args(Out), 2) = @array(@format_args(Out), 2) {}\n}\n"),
        ("out", "make: function () text: @format_args(Out) {\n    text = \"\"\n}\n\nmain: function () nothing {}\n"),
        ("global", "text: @format_args(Out)\n\nmain: function () nothing {}\n"),
        ("store through ref", "put: function (to: ref @format_args(Out), from: @format_args(Out)) nothing {\n    to = from\n}\n\nmain: function () nothing {}\n"),
        ("@write", "put: function (to: ref @format_args(Out), from: @format_args(Out)) nothing {\n    @write(to, from)\n}\n\nmain: function () nothing {}\n"),
        ("temporary assigned", "main: function () nothing {\n    x: @format_args(Out) = \"\"\n    x = $\"{number()}\"\n}\n"),
        ("temporary in an arm", "main: function () nothing {\n    n: @bit32 = 0x1\n    x: @format_args(Out) = if n == 0x1 then $\"{number()}\" else \"\"\n}\n"),
    ] {
        let diags = check_format(src);
        assert!(has_code(&diags, "E072"), "{what}: {diags:?}");
    }
}

#[test]
fn a_lasting_hole_may_be_assigned() {
    let diags = check_format(
        "main: function () nothing {\n    n: @bit32 = 0x1\n    x: @format_args(Out) = \"\"\n    x = $\"{n}\"\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn format_consumers_check_their_operands() {
    let ok = check_format(
        "main: function () nothing {\n    n: @bit32 = 0x1\n    out: Out = Out {}\n    x: @format_args(Out) = $\"{n}\"\n    count: @bit_native = @format_args_holes(x)\n    piece: @byte_buffer = @format_args_piece(x, 0x0)\n    @format_args_hole(x, 0x0, ref out)\n}\n",
    );
    assert!(!ok.has_errors(), "{ok:?}");
    let not_format =
        check_format("main: function () nothing {\n    n: @bit32 = 0x1\n    count: @bit_native = @format_args_holes(n)\n}\n");
    assert!(has_code(&not_format, "E010"), "{not_format:?}");
    let wrong_writer = check_format(
        "main: function () nothing {\n    n: @bit32 = 0x1\n    x: @format_args(Out) = $\"{n}\"\n    @format_args_hole(x, 0x0, ref n)\n}\n",
    );
    assert!(has_code(&wrong_writer, "E010"), "{wrong_writer:?}");
}

#[test]
fn a_template_hole_of_a_parameter_is_no_error() {
    let diags = check_format(
        "show[T]: function (value: T) nothing {\n    text: @format_args(Out) = $\"{value}\"\n}\n\nmain: function () nothing {}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn an_instance_whose_hole_has_no_format_is_e042() {
    let src = "show[T]: function (value: T) nothing {\n    text: @format_args(Out) = $\"{value}\"\n}\n\nmain: function () nothing {\n    n: @bit32 = 0x1\n    show[@bit32](n)\n    b: @bit8 = 0x1\n    show[@bit8](b)\n}\n";
    let diags = check_format(src);
    assert!(has_code(&diags, "E042"), "{diags:?}");
    assert!(!has_code(&diags, "E071"), "{diags:?}");
    assert_eq!(
        diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E042")
            .count(),
        1,
        "{diags:?}"
    );
}

#[test]
fn removed_format_intrinsics_are_e021_with_a_note() {
    let diags = check_format(
        "main: function () nothing {\n    x: @format_args(Out) = \"\"\n    count: @bit_native = @format_args_count(x)\n}\n",
    );
    assert!(has_code(&diags, "E021"), "{diags:?}");
    let notes: Vec<&String> = diags
        .diagnostics()
        .iter()
        .flat_map(|diag| &diag.notes)
        .collect();
    assert!(
        notes.iter().any(|note| note.contains("@format_args_holes")),
        "{notes:?}"
    );
}

#[test]
fn nested_private_object_field_resolves() {
    // Regression: a field wrapped in nested `private` blocks must flatten
    // into the object's member fields, not be routed as a top-level item
    // (which silently dropped it and produced a spurious `E021`).
    let src = "Object: structure {\n    private {\n        private {\n            f: @bit8\n        }\n    }\n\n    create: function () result: Object {\n        result = Object {\n            f = 0x1\n        }\n    }\n\n    get: function (instance: ref Object) result: @bit8 {\n        result = instance.f\n    }\n}\n\nmain: function () nothing {\n    variable: Object = Object.create()\n    value: @bit8 = variable.get()\n}\n";
    let diags = check_src(src);
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn nested_private_object_field_reaches_its_namespace() {
    // A member `private` reaches the type's whole namespace, so `main`
    // beside `Object` reads the field (`E026` from another namespace is
    // `m6.rs`'s `nested_private_field_denied_is_e026`).
    let src = "Object: structure {\n    private {\n        private {\n            f: @bit8\n        }\n    }\n\n    create: function () result: Object {\n        result = Object {\n            f = 0x1\n        }\n    }\n}\n\nmain: function () nothing {\n    variable: Object = Object.create()\n    value: @bit8 = variable.f\n}\n";
    let diags = check_src(src);
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn fails_generic_duck_typing_short() {
    let src = "gen_function[T]: function () out: T {\n    out = T.other()\n}\n\nmain: function () nothing {\n    variable: @bit32 = gen_function[@bit32]()\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    let e042: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == "E042")
        .collect();
    assert_eq!(e042.len(), 1, "{rendered}");
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("required by instantiation of gen_function[@bit32]"),
        "{rendered}"
    );
    assert!(rendered.contains("missing method `other`"), "{rendered}");
    assert!(!rendered.contains("error[E010]"), "{rendered}");
}

#[test]
fn generic_missing_member_shows_required_signature() {
    // A bare-type receiver (`T.other()`) makes the captured body use the
    // `E042` primary, and the inferred signature is a note using the
    // use-site expected return type, with the pretty instance name in the
    // instantiation note.
    let src = "gen_function[T]: function () out: T {\n    out = T.other()\n}\n\nmain: function () nothing {\n    variable: @bit32 = gen_function[@bit32]()\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    let e042: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == "E042")
        .collect();
    assert_eq!(e042.len(), 1, "{rendered}");
    let primary = e042[0]
        .labels
        .iter()
        .find(|label| label.primary)
        .expect("E042 primary label");
    let (line, _) = sm.line_col(primary.span).expect("primary span has a file");
    assert_eq!(line, 2, "primary must be the body use: {rendered}");
    assert!(
        rendered.contains("other: function () T"),
        "missing inferred signature: {rendered}"
    );
    assert!(
        rendered.contains("required by instantiation of gen_function[@bit32]"),
        "missing pretty instantiation note: {rendered}"
    );
    // The single active frame is already named by the instantiation note,
    // so the `in instantiation of` chain must not duplicate it.
    assert!(
        !rendered.contains("in instantiation of gen_function[@bit32]"),
        "single-frame chain duplicated: {rendered}"
    );
    assert!(
        lilac_ty::demangle_text(&rendered) == rendered,
        "mangled symbol leaked: {rendered}"
    );
}

#[test]
fn value_receiver_shows_required_signature() {
    // A value receiver of generic-param type renders `self`; the argument
    // is probe-checked (a decimal literal is `@bit8`) and the return
    // comes from the generic parameter.
    let src = "gen[T]: function (value: T) out: T {\n    out = value.other(5)\n}\n\nmain: function () nothing {\n    x: @bit32 = gen[@bit32](0x1)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("other: function (self, a: @bit8) T"),
        "missing value-receiver signature: {rendered}"
    );
}

#[test]
fn generic_param_form_in_required_signature() {
    // The receiver and an argument render their generic parameter names
    // (`T`, `U`) while a genuinely concrete return stays concrete (`@bit8`).
    let src = "gen[T, U]: function (value: T, extra: U) out: @bit8 {\n    out = value.other(extra)\n}\n\nmain: function () nothing {\n    x: @bit8 = gen[@bit32, @bit64](0x1, 0x2)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("other: function (self, a: U) @bit8"),
        "missing generic-param signature: {rendered}"
    );
}

#[test]
fn concrete_return_stays_concrete_in_required_signature() {
    // A return type that is not one of the instantiation arguments (a user
    // object) must render concretely, never as a generic parameter.
    let src = "Widget: structure {\n    value: @bit32\n}\n\ngen[T]: function (value: T) out: Widget {\n    out = value.other(5)\n}\n\nmain: function () nothing {\n    x: Widget = gen[@bit32](0x1)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("other: function (self, a: @bit8) Widget"),
        "concrete return not rendered concretely: {rendered}"
    );
}

#[test]
fn field_miss_shows_required_field_note() {
    // A field read on a generic-derived value gets the field form of the
    // inferred note, using the generic parameter for the expected type.
    let src = "gen[T]: function (value: T) out: T {\n    out = value.data\n}\n\nmain: function () nothing {\n    x: @bit32 = gen[@bit32](0x1)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("missing member `data` on `@bit32`"),
        "{rendered}"
    );
    assert!(
        rendered.contains("based on this usage, the generic body requires a field matching:\n")
            && rendered.contains("    data: T\n"),
        "missing field note: {rendered}"
    );
}

#[test]
fn concrete_field_type_stays_concrete() {
    // The field note's type comes from the use-site expected type; a user
    // object stays concrete rather than becoming a generic parameter.
    let src = "Widget: structure {\n    value: @bit32\n}\n\ngen[T]: function (value: T) out: Widget {\n    out = value.data\n}\n\nmain: function () nothing {\n    x: Widget = gen[@bit32](0x1)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("requires a field matching:\n")
            && rendered.contains("    data: Widget\n"),
        "concrete field type not rendered concretely: {rendered}"
    );
}

#[test]
fn concrete_field_miss_has_no_inferred_note() {
    // An ordinary field miss outside a generic instantiation keeps the
    // plain `E021` shape with no inferred-requirement note.
    let src = "Object: structure {\n    value: @bit32\n}\n\nmain: function () nothing {\n    o: Object = Object {\n        value = 0x1\n    }\n    x: @bit32 = o.data\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert!(
        rendered.contains("no field `data` on `Object`"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("requires a field matching"),
        "concrete field miss gained a note: {rendered}"
    );
}

#[test]
fn no_cascade_one_error() {
    let src = "gen_function[T]: function () out: T {\n    out = T.other()\n}\n\nmain: function () nothing {\n    a: @bit32 = gen_function[@bit32]()\n    b: @bit32 = gen_function[@bit32]()\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
}

#[test]
fn failing_object_method_reported_at_use_site() {
    // A missing member inside a generic object method points the `E042`
    // primary at the failing use in the method body; the instantiation note
    // carries the pretty method instance rather than a mangled symbol.
    let src = "GenObject[T]: structure {\n    value: T\n    get: function (self) result: T {\n        result = self.other()\n    }\n}\n\nmain: function () nothing {\n    variable: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    r: @bit32 = variable.get()\n}\n";
    let (diags, _, _, sm) = run(src);
    let e042: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == "E042")
        .collect();
    assert_eq!(e042.len(), 1, "{diags:?}");
    let primary = e042[0]
        .labels
        .iter()
        .find(|label| label.primary)
        .expect("E042 has a primary label");
    let (line, _) = sm.line_col(primary.span).expect("primary span has a file");
    assert_eq!(line, 4, "primary should be the body use: {diags:?}");
    let rendered = render_all(&diags, &sm);
    assert!(
        rendered.contains("required by instantiation of GenObject.get[@bit32]"),
        "{rendered}"
    );
    assert!(rendered.contains("missing method `other`"), "{rendered}");
    assert_eq!(lilac_ty::demangle_text(&rendered), rendered);
}

#[test]
fn field_miss_has_clean_tone() {
    // A field lookup miss inside a generic body must not leak the internal
    // milestone label; it reads as a requirement phrase.
    let src = "Object: structure {\n    value: @bit32\n}\n\ngen[T]: function (v: T) out: @bit8 {\n    out = v.data\n}\n\nmain: function () nothing {\n    o: Object = Object {\n        value = 0x1\n    }\n    x: @bit8 = gen[Object](o)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("missing member `data` on `Object`"),
        "{rendered}"
    );
    assert!(!rendered.contains("M4"), "{rendered}");
    assert!(!rendered.contains("E040"), "{rendered}");
}

#[test]
fn generic_instance_miss_has_did_you_mean() {
    // The lookup owner is an instance's display form
    // (`GenObject[@bit32]`); did-you-mean must still find the base's
    // members.
    let src = "GenObject[T]: structure {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\n\ngen[T]: function (value: T) out: @bit32 {\n    out = value.geet()\n}\n\nmain: function () nothing {\n    v: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    x: @bit32 = gen[GenObject[@bit32]](v)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert!(
        rendered.contains("`GenObject[@bit32]` has `get`, did you mean `get`?"),
        "{rendered}"
    );
}

#[test]
fn cyclic_growing_args_is_e051() {
    let (diags, _, _) = check(
        "Box[T]: structure {\n    value: T\n}\n\ngrow[T]: function () nothing {\n    grow[Box[T]]()\n}\n\nmain: function () nothing {\n    grow[@bit32]()\n}\n",
    );
    let count = diags
        .diagnostics()
        .iter()
        .filter(|d| d.code == "E051")
        .count();
    assert_eq!(count, 1, "{diags:?}");
    let note = diags
        .diagnostics()
        .iter()
        .find(|d| d.code == "E051")
        .and_then(|d| d.notes.first())
        .cloned()
        .unwrap_or_default();
    assert!(note.contains("grow[@bit32]"), "readable chain: {note}");
    assert!(
        note.contains("grow[Box[@bit32]]"),
        "readable nested arg: {note}"
    );
    assert_eq!(
        lilac_ty::demangle_text(&note),
        note,
        "mangled symbol leaked"
    );
}

#[test]
fn cyclic_growing_object_layout_is_e051() {
    let (diags, _, _) = check(
        "GenObj[T]: structure {\n    next: GenObj[GenObj[T]]\n}\n\nconsume[T]: function () nothing {\n}\n\nmain: function () nothing {\n    consume[GenObj[@bit32]]()\n}\n",
    );
    let count = diags
        .diagnostics()
        .iter()
        .filter(|d| d.code == "E051")
        .count();
    assert_eq!(count, 1, "{diags:?}");
    let note = diags
        .diagnostics()
        .iter()
        .find(|d| d.code == "E051")
        .and_then(|d| d.notes.first())
        .cloned()
        .unwrap_or_default();
    assert!(note.contains("GenObj[@bit32]"), "readable chain: {note}");
    assert!(
        note.contains("GenObj[GenObj[@bit32]]"),
        "readable nested arg: {note}"
    );
    assert_eq!(
        lilac_ty::demangle_text(&note),
        note,
        "mangled symbol leaked"
    );
}

/// The `E051` notes of `src`'s one `E051`.
fn e051_notes(src: &str) -> Vec<String> {
    let (diags, _, _) = check(src);
    let found: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|d| d.code == "E051")
        .collect();
    assert_eq!(found.len(), 1, "{diags:?}");
    found[0].notes.clone()
}

#[test]
fn e051_shows_how_the_arguments_grow() {
    let notes = e051_notes(
        "Box[T]: structure {\n    value: T\n}\n\ngrow[T]: function () nothing {\n    grow[Box[T]]()\n}\n\nmain: function () nothing {\n    grow[@bit32]()\n}\n",
    );
    assert!(
        notes.contains(
            &"the type arguments grow each time: grow[@bit32] -> grow[Box[@bit32]]".to_owned()
        ),
        "{notes:?}"
    );
    assert!(
        notes[0].ends_with("(65 frames)"),
        "the chain reaches the 65th `grow`: {notes:?}"
    );
    let notes = e051_notes(
        "GenObj[T]: structure {\n    next: GenObj[GenObj[T]]\n}\n\nconsume[T]: function () nothing {\n}\n\nmain: function () nothing {\n    consume[GenObj[@bit32]]()\n}\n",
    );
    assert!(
        notes.contains(
            &"the type arguments grow each time: GenObj[@bit32] -> GenObj[GenObj[@bit32]]"
                .to_owned()
        ),
        "{notes:?}"
    );
}

#[test]
fn e051_counts_recursion_per_definition() {
    // Mutual recursion: `a` and `b` alternate, and `a` is the first to
    // appear a 65th time; the chain holds both.
    let notes = e051_notes(
        "Box[T]: structure {\n    value: T\n}\n\na[T]: function () nothing {\n    b[Box[T]]()\n}\n\nb[T]: function () nothing {\n    a[T]()\n}\n\nmain: function () nothing {\n    a[@bit32]()\n}\n",
    );
    assert!(notes[0].ends_with("(129 frames)"), "{notes:?}");
    assert!(
        notes
            .iter()
            .any(|note| note.contains("`a` would appear once more")),
        "{notes:?}"
    );
    // Arguments that change without each containing the one it replaced
    // get no growth note: `swap` exchanges its two (wrapping one, so they
    // never repeat).
    let notes = e051_notes(
        "Box[T]: structure {\n    value: T\n}\n\nswap[A, B]: function () nothing {\n    swap[B, Box[A]]()\n}\n\nmain: function () nothing {\n    swap[@bit32, @bit8]()\n}\n",
    );
    assert!(
        !notes.iter().any(|note| note.contains("grow each time")),
        "{notes:?}"
    );
}

#[test]
fn branching_recursion_reports_once() {
    // Each level requests two new instances: followed depth first, the
    // first branch meets `E051`, and every other branch stops at `grow`.
    let notes = e051_notes(
        "W[T]: structure {\n    value: T\n}\n\nV[T]: structure {\n    value: T\n}\n\ngrow[T]: function () nothing {\n    grow[W[T]]()\n    grow[V[T]]()\n}\n\nmain: function () nothing {\n    grow[@bit32]()\n}\n",
    );
    assert!(
        notes.contains(
            &"the type arguments grow each time: grow[@bit32] -> grow[W[@bit32]]".to_owned()
        ),
        "the first branch: {notes:?}"
    );
    // The same for a type laid out inside itself twice per level.
    e051_notes(
        "W[T]: structure {\n    value: T\n}\n\nV[T]: structure {\n    value: T\n}\n\nG[T]: structure {\n    a: ref G[W[T]]\n    b: ref G[V[T]]\n}\n\nmain: function () nothing {\n    x: @bit64 = @size_of(G[@bit32])\n}\n",
    );
}

#[test]
fn unused_branching_recursion_checks_quickly() {
    // Unused, `grow` is checked as a template: its recursion stops at the
    // limit too (silently, as template errors about its parameters do).
    let (diags, _, _) = check(
        "W[T]: structure {\n    value: T\n}\n\nV[T]: structure {\n    value: T\n}\n\ngrow[T]: function () nothing {\n    grow[W[T]]()\n    grow[V[T]]()\n}\n\nmain: function () nothing {\n}\n",
    );
    assert_eq!(diags.error_count(), 0, "{diags:?}");
}

#[test]
fn distinct_types_nest_past_the_old_limit() {
    // Seventeen distinct objects nested by value: no recursion at all (the
    // old limit counted every level, and stopped at sixteen).
    let mut src = String::new();
    for index in 1..20 {
        let _ = write!(src, "O{index}: structure {{\n    a: O{}\n}}\n\n", index + 1);
    }
    src += "O20: structure {\n    a: @bit32\n}\n\nmain: function () nothing {\n}\n";
    let (diags, _, _) = check(&src);
    assert_eq!(diags.error_count(), 0, "{diags:?}");
}

#[test]
fn bare_generic_object_static_is_e044() {
    let (diags, _, _) = check(
        "GenObj[T]: structure {\n    value: T\n    ping: function () nothing {\n    }\n}\n\nmain: function () nothing {\n    GenObj.ping()\n}\n",
    );
    assert!(
        diags.diagnostics().iter().any(|d| d.code == "E044"),
        "{diags:?}"
    );
    assert!(
        !diags.diagnostics().iter().any(|d| d.code == "E070"),
        "{diags:?}"
    );
}

#[test]
fn generic_inline_control_flow_checks() {
    // A generic `=>` body with control flow now carries a statement prelude
    // that each instantiation expands, so it checks cleanly.
    let (diags, _, _) = check(
        "pick[T]: function (x: T, c: @bit8) T => if c then x else x\n\nmain: function () nothing {\n    y: @bit32 = pick[@bit32](0x1, 0x1)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn generic_inline_named_slot_ok() {
    // The first return slot is assigned by the `=>` body, so no `E027`.
    let (diags, _, _) = check(
        "gen_inline[T]: function (x: T) out: T => x\n\nmain: function () nothing {\n    y: @bit32 = gen_inline[@bit32](0x1)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn recursive_generic_inline_is_allowed() {
    // `=>` bodies are ordinary (always-inline) functions, so recursion is
    // legal; LLVM simply declines to inline the recursive call.
    let (diags, _, _) = check(
        "rec_inline[T]: function (x: T) T => rec_inline[T](x)\n\nmain: function () nothing {\n    y: @bit32 = rec_inline[@bit32](0x1)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn value_based_duck_typing_ok() {
    let (diags, _, _) = check(
        "GenObject[T]: structure {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\n\ngen[T]: function (value: T) out: @bit32 {\n    out = value.get()\n}\n\nmain: function () nothing {\n    v: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    x: @bit32 = gen[GenObject[@bit32]](v)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn static_method_on_object_ok() {
    let (diags, _, _) = check(
        "Object: structure {\n    get: function () result: @bit32 {\n        result = 0x2\n    }\n}\n\ngen[T]: function () out: @bit32 {\n    out = T.get()\n}\n\nmain: function () nothing {\n    x: @bit32 = gen[Object]()\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn bit32_to_bit8_converts() {
    let diags = check_src(&format!(
        "{conversion_hook}main: function () nothing {{\n    a: @bit32 = 0x1\n    b: @bit8 = a\n}}\n",
        conversion_hook = conversion_hook(),
    ));
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn missing_conversion_is_e043() {
    let diags =
        check_src("main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = a\n}\n");
    assert!(has_code(&diags, "E043"), "{diags:?}");
    let msgs: Vec<String> = diags
        .diagnostics()
        .iter()
        .map(|d| d.msg.to_string())
        .collect();
    assert!(
        msgs.iter()
            .any(|m| m.contains("@bit32") && m.contains("@bit8")),
        "{msgs:?}"
    );
}

#[test]
fn operator_does_not_convert_operands() {
    // `@add(@bit32,@bit32)` + a `@bit8 -> @bit32` conversion: `b + a`
    // (b:@bit8, a:@bit32) must NOT convert `b`; it is E043.
    let diags = check_src(
        "@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 { result = @i_add_32(lhs, rhs) }\n\
         @conversion: function (from: @bit8) to: @bit32 { to = @extend_zero(from) }\n\
         main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = 0x2\n    z: @bit32 = b + a\n}\n",
    );
    assert!(has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn hook_call_does_not_convert_arguments() {
    let diags = check_src(
        "@f: function (x: @bit32) @bit32 => @i_add_32(x, 0x1)\n\
         @conversion: function (from: @bit8) to: @bit32 { to = @extend_zero(from) }\n\
         main: function () nothing {\n    b: @bit8 = 0x2\n    z: @bit32 = @f(b)\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn typed_context_still_converts() {
    let diags = check_src(
        "@conversion: function (from: @bit32) to: @bit8 { to = @truncate(from) }\n\
         main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = a\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn binop_anchors_left() {
    let diags = check_src(
        "@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 {\n    result = @i_add_32(lhs, rhs)\n}\n@conversion: function (from: @bit8) to: @bit32 {\n    to = @extend_zero(from)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = 0x2\n    z: @bit32 = a + b\n    w: @bit32 = b + a\n}\n",
    );
    assert_eq!(
        diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E043")
            .count(),
        2,
        "{diags:?}"
    );
}

#[test]
fn exact_match_beats_conversion() {
    let diags = check_src(
        "@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 {\n    result = @i_add_8(lhs, rhs)\n}\n@add: function (lhs: @bit8, rhs: @bit32) result: @bit32 {\n    result = @i_add_32(@truncate(lhs), rhs)\n}\n@conversion: function (from: @bit8) to: @bit32 {\n    to = @extend_zero(from)\n}\nmain: function () nothing {\n    x: @bit8 = 0x1\n    y: @bit8 = 0x2\n    z: @bit8 = x + y\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn binop_literal_takes_sibling_width() {
    let diags = check_src(
        "@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 {\n    result = @i_add_32(lhs, rhs)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    z: @bit32 = a + 0x2\n    w: @bit32 = 0x3 + a\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn equality_literal_takes_sibling_width() {
    let diags =
        check_src("main: function () nothing {\n    a: @bit8 = 0x1\n    c: @bit8 = a == 0x1\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn e043_labels_both_sides() {
    let diags =
        check_src("main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = a\n}\n");
    let e043: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|d| d.code == "E043")
        .collect();
    assert_eq!(e043.len(), 1, "{diags:?}");
    assert_eq!(e043[0].labels.len(), 2, "{diags:?}");
}

#[test]
fn ref_conversion_shape_is_e022() {
    let diags = check_src(
        "@conversion: function (from: ref @bit32) to: @bit8 {\n    to = 0x0\n}\nmain: function () nothing {\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn direct_hook_call_picks_exact_overload() {
    let diags = check_src(
        "@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 {\n    result = @i_add_8(lhs, rhs)\n}\n@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 {\n    result = @i_add_32(lhs, rhs)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit32 = 0x2\n    z: @bit32 = @add(a, b)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn every_hook_overload_body_is_checked() {
    // The second conversion references an unknown name: it must be reported.
    let diags = check_src(
        "Int64: structure {\n    data: @bit64\n\
         @conversion: function (from: @bit64) Int64 => Int64 { data = from }\n\
         @conversion: function (from: @byte_buffer) Int64 => Int64 { data = @nope(from) }\n}\n",
    );
    assert!(
        diags.diagnostics().iter().any(|d| d.code == "E021"),
        "{diags:?}"
    );
}

#[test]
fn member_operator_resolves() {
    let diags = check_src(
        "Int64: structure {\n    data: @bit64\n\
         @add: function (lhs: Int64, rhs: Int64) Int64 => Int64 { data = @i_add_64(lhs.data, rhs.data) }\n}\n\
         main: function () nothing {\n    a: Int64 = Int64 { data = 0x1 }\n    b: Int64 = a + a\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn cross_scope_identical_conversion_pair_is_e020() {
    // The member hook is collected first, then the identical top-level hook
    // is checked against the shared hook set (dedup by signature).
    let diags = check_src(
        "Int64: structure {\n    data: @bit64\n\
         @conversion: function (from: @bit32) to: @bit8 { to = @truncate(from) }\n}\n\
         @conversion: function (from: @bit32) to: @bit8 { to = @truncate(from) }\n",
    );
    assert!(has_code(&diags, "E020"), "{diags:?}");
}

#[test]
fn private_member_conversion_resolves() {
    let diags = check_src(
        "Int64: structure {\n    data: @bit64\n\
         private {\n\
         @conversion: function (from: @bit32) to: @bit8 { to = @truncate(from) }\n\
         }\n}\n\
         main: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = a\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn method_and_hook_with_same_base_name_coexist() {
    let diags = check_src(
        "Int64: structure {\n    data: @bit64\n\
         add: function (self, x: @bit64) result: @bit64 { result = @i_add_64(x, self.data) }\n\
         @add: function (lhs: Int64, rhs: Int64) Int64 => Int64 { data = @i_add_64(lhs.data, rhs.data) }\n}\n\
         main: function () nothing {\n    a: Int64 = Int64 { data = 0x1 }\n    b: Int64 = a + a\n    c: @bit64 = a.add(0x2)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn generic_object_hook_not_e052() {
    let diags = check_src(
        "GenObj[T]: structure {\n    value: T\n    @on_drop: function (from: ref GenObj[T]) nothing {\n    }\n}\nmain: function () nothing {\n    g: GenObj[@bit8] = GenObj[@bit8] { value = 0x1 }\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn blanket_generic_hook_is_e052() {
    let diags = check_src("@add[T]: function (lhs: T, rhs: T) result: T {\n    result = lhs\n}\n");
    assert!(has_code(&diags, "E052"), "{diags:?}");
}

#[test]
fn unknown_param_type_is_e023() {
    let diags = check_src("f: function (x: Nope) nothing {\n}\nmain: function () nothing {\n}\n");
    assert!(has_code(&diags, "E023"), "{diags:?}");
}

#[test]
fn generic_type_arity_is_e044() {
    let diags = check_src(
        "Box[T]: structure {\n    value: T\n}\nmain: function () nothing {\n    b: Box[@bit8, @bit8] = Box[@bit8] { value = 0x1 }\n}\n",
    );
    assert!(has_code(&diags, "E044"), "{diags:?}");
}

#[test]
fn self_containing_type_is_e053() {
    let diags = check_src(
        "Node: structure {\n    next: Node\n}\nmain: function () nothing {\n    n: Node\n}\n",
    );
    assert!(has_code(&diags, "E053"), "{diags:?}");
    // Through a reference is fine.
    let diags =
        check_src("Node: structure {\n    next: ref Node\n}\nmain: function () nothing {\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

/// A `ref`'s referent is laid out after the type holding the `ref`, so a
/// type first met inside another (`Opt[ref Node]` in `Holder`) is finished
/// before `Node`'s own `Opt[ref Node]` field needs it: no false `E053`. A
/// type that grows through a `ref` still stops at `E051`, and a referent's
/// own error is still reported.
#[test]
fn ref_referents_are_laid_out_after_their_holder() {
    let opt = "Opt[T]: either {\n    Some: T\n    None\n}\n";
    let src = format!(
        "{opt}Holder: structure {{\n    free: Opt[ref Node]\n}}\nNode: structure {{\n    next: Opt[ref Node]\n}}\nmain: function () nothing {{\n}}\n"
    );
    assert!(!check_src(&src).has_errors(), "{src}");
    let growing =
        "W[T]: structure {\n    w: ref W[W[T]]\n    v: T\n}\nf: function (x: W[@bit8]) nothing {\n}\n";
    assert_eq!(codes_of(&check_src(growing)), ["E051"]);
    let broken = "G[T]: structure {\n    a: G[T]\n}\nH: structure {\n    r: ref G[@bit8]\n}\nf: function (h: H) nothing {\n}\n";
    assert_eq!(codes_of(&check_src(broken)), ["E053"]);
}

#[test]
fn same_block_redeclaration_is_e020_but_nested_shadowing_is_fine() {
    let diags =
        check_src("main: function () nothing {\n    x: @bit8 = 0x1\n    x: @bit8 = 0x2\n}\n");
    assert!(has_code(&diags, "E020"), "{diags:?}");
    let diags = check_src(
        "main: function () nothing {\n    x: @bit8 = 0x1\n    {\n        x: @bit32 = 0x2\n    }\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn atomics_check_orderings_and_widths() {
    let ok = check_src(
        "main: function () nothing {\n    x: @bit16 = 0x1\n    old: @bit16 = @atomic_or_release_16(ref x, 0x2)\n    @atomic_store_16(ref x, old)\n    @atomic_fence_acquire()\n}\n",
    );
    assert!(ok.diagnostics().is_empty(), "{ok:?}");
    // A load cannot release: the name is not an intrinsic.
    let diags = check_src(
        "main: function () nothing {\n    x: @bit32 = 0x1\n    y: @bit32 = @atomic_load_release_32(ref x)\n}\n",
    );
    assert!(has_code(&diags, "E021"), "{diags:?}");
    // The operand must be exactly `@bitW`, and the address needs a `ref`.
    let diags = check_src(
        "main: function () nothing {\n    x: @bit32 = 0x1\n    y: @bit64 = 0x1\n    z: @bit32 = @atomic_add_32(ref x, y)\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
    let diags = check_src(
        "main: function () nothing {\n    x: @bit32 = 0x1\n    z: @bit32 = @atomic_load_32(x)\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn arrays_check_elements_lengths_and_operands() {
    let ok = check_src(
        "Box: structure {\n    items: @array(@bit32, 0x4)\n}\n\nmain: function () nothing {\n    b: Box { items = @array(@bit32, 4) {} }\n    slot: ref @bit32 = @array_at(ref b.items, 0x1)\n    n: @bit_native = @array_len(b.items)\n}\n",
    );
    assert!(ok.diagnostics().is_empty(), "{ok:?}");
    let errors = [
        // An element with an RAII hook, itself or in a field.
        "Noisy: structure {\n    v: @bit8\n\n    @on_drop: function (self) nothing {}\n}\n\nOuter: structure {\n    n: Noisy\n}\n\nmain: function () nothing {\n    a: @array(Outer, 2) {}\n}\n",
        // A `ref` element, a zero length and a length past `u32`.
        "main: function () nothing {\n    a: @array(ref @bit8, 2) {}\n}\n",
        "main: function () nothing {\n    a: @array(@bit8, 0) {}\n}\n",
        "main: function () nothing {\n    a: @array(@bit8, 0x100000000) {}\n}\n",
        // An array literal has no fields.
        "main: function () nothing {\n    a: @array(@bit8, 2) { x = 0x1 }\n}\n",
        // `@array_at` needs a `ref` array; `@array_len` an array.
        "main: function () nothing {\n    a: @array(@bit8, 2) {}\n    b: ref @bit8 = @array_at(a, 0x0)\n}\n",
        "main: function () nothing {\n    a: @bit8 = 0x1\n    n: @bit_native = @array_len(a)\n}\n",
    ];
    for src in errors {
        let diags = check_src(src);
        assert!(has_code(&diags, "E010"), "{src}\n{diags:?}");
    }
}

#[test]
fn nested_types_are_e036() {
    let sources = [
        "Outer: structure {
    Inner: structure {
        x: @bit8
    }
}

main: function () nothing {}
",
        "Status: either {
    Idle
    Kind: either { A, B }
}

main: function () nothing {}
",
        "Outer: structure {
    private {
        Secret: either { A }
    }
}

main: function () nothing {}
",
    ];
    for src in sources {
        let diags = check_src(src);
        let codes: Vec<&str> = diags.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(
            codes,
            ["E036"],
            "{src}
{diags:?}"
        );
    }
}

#[test]
fn nested_type_name_is_unknown() {
    // The rejected declaration defines nothing, so its uses do not resolve.
    let src = "Outer: structure {
    Inner: structure {
        x: @bit8
    }
    inner: Inner
}

main: function () nothing {
    a: Outer.Inner = Outer.Inner { x = 0x1 }
}
";
    let diags = check_src(src);
    assert!(has_code(&diags, "E036"), "{diags:?}");
    assert!(has_code(&diags, "E023"), "{diags:?}");
}

#[test]
fn unused_generic_drops_errors_about_its_parameter() {
    // Nothing instantiates these, so `T` is abstract: each failure is about
    // a type containing `T` and could hold for some argument.
    let diags = check_src(
        "Box[T]: structure {\n    value: T\n}\n\
         p1[T]: function () out: T {\n    out = T.default()\n}\n\
         p2[T]: function (x: T) out: @bit32 {\n    out = x.foo()\n}\n\
         p3[T]: function (x: T) out: @bit32 {\n    out = x\n}\n\
         p4[T]: function () out: T {\n    out = 0x5\n}\n\
         p5[T]: function () out: T {\n    value: T.create(0x1)\n    out = value\n}\n\
         p6[T]: function (x: T) out: @bit32 {\n    out = 0x0\n    if x {\n        out = 0x1\n    }\n}\n\
         p7[T]: function (x: T) out: Box[T] {\n    out = Box[T] { value = 0x1 }\n}\n\
         main: function () nothing {\n}\n",
    );
    assert!(!diags.has_errors(), "{:?}", messages(&diags));
}

#[test]
fn unused_generic_reports_errors_independent_of_its_parameter() {
    let diags = check_src(
        "Box[T]: structure {\n    value: T\n}\n\
         q1[T]: function (x: T) out: @bit32 {\n    out = nonexistent()\n}\n\
         q2[T]: function (x: T) out: @bit32 {\n    small: @bit8 = 0x1\n    out = small\n}\n\
         q3[T]: function (x: T) out: Box[T] {\n    out = Box[T] { valeu = x }\n}\n\
         main: function () nothing {\n}\n",
    );
    let messages = messages(&diags);
    assert!(
        messages.contains(&"unresolved name `nonexistent`".to_owned()),
        "{messages:?}"
    );
    assert!(
        messages.contains(&"no conversion from @bit8 to @bit32".to_owned()),
        "{messages:?}"
    );
    assert!(
        messages.contains(&"no field `valeu` on `Box[T]`".to_owned()),
        "{messages:?}"
    );
    assert!(!has_code(&diags, "E042"), "{messages:?}");
}

#[test]
fn unused_generic_error_reported_once() {
    // `outer` checks `inner[T]` (the same abstract `T` as `inner`'s own
    // template) and `other[T]` (a different parameter than `other[U]`):
    // each body error is still reported once.
    let diags = check_src(
        "inner[T]: function (x: T) nothing {\n    a: @bit32 = missing_a()\n}\n\
         other[U]: function (u: U) nothing {\n    b: @bit32 = missing_b()\n}\n\
         outer[T]: function (x: T) nothing {\n    inner[T](x)\n    other[T](x)\n}\n\
         main: function () nothing {\n}\n",
    );
    let messages = messages(&diags);
    for name in ["missing_a", "missing_b"] {
        let text = format!("unresolved name `{name}`");
        assert_eq!(
            messages.iter().filter(|m| **m == text).count(),
            1,
            "{messages:?}"
        );
    }
}

#[test]
fn instantiated_generic_keeps_reporting_through_its_instance() {
    // A used generic is not checked as a template: its error is the
    // instance's, `E042` at the call.
    let diags = check_src(
        "g[T]: function (x: T) out: @bit32 {\n    out = x.foo()\n}\n\
         main: function () nothing {\n    v: @bit32 = g[@bit32](0x1)\n}\n",
    );
    assert_eq!(
        diags
            .diagnostics()
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>(),
        ["E042"]
    );
}

#[test]
fn unused_generic_constant_index_out_of_bounds_is_e037() {
    let diags = check_src(
        "LIMIT: @bit_native => 0x9\n\
         bad[T]: function (x: T) nothing {\n    a: @array(T, 4) = @array(T, 4) {}\n    \
         b: ref T = @array_at(ref a, 0x4)\n    c: ref T = @array_at(ref a, LIMIT)\n    \
         d: ref T = @array_at(ref a, 0x3)\n}\n\
         runtime[T]: function (x: T, flag: @bit8) nothing {\n    a: @array(T, 4) = @array(T, 4) {}\n    \
         if flag {\n        b: ref T = @array_at(ref a, 0x5)\n    }\n}\n\
         main: function () nothing {\n}\n",
    );
    assert_eq!(
        messages(&diags),
        [
            "index 4 is out of bounds for an array of length 4",
            "index 9 is out of bounds for an array of length 4",
            "index 5 is out of bounds for an array of length 4",
        ]
    );
}

#[test]
fn unused_generic_index_under_a_possibly_constant_condition_is_skipped() {
    // `@size_of(T)` is constant once `T` is known, so lowering may drop the
    // arm for every argument actually used.
    let diags = check_src(
        "guarded[T]: function (x: T) nothing {\n    a: @array(T, 4) = @array(T, 4) {}\n    \
         if @i_greater_than_u_native(@size_of(T), 0x8) {\n        \
         b: ref T = @array_at(ref a, 0x7)\n    }\n}\n\
         main: function () nothing {\n}\n",
    );
    assert!(!diags.has_errors(), "{:?}", messages(&diags));
}

/// A type whose own `@on_drop` makes it move.
const HANDLE: &str =
    "Handle: structure {\n    id: @bit32\n\n    @on_drop: function (self) nothing {\n    }\n}\n\
                      take: function (handle: Handle) nothing {\n}\n";

/// The function with a local named `local`.
fn body_of<'a>(hir: &'a HirProgram, local: &str) -> &'a lilac_hir::HirFunc {
    hir.funcs
        .iter()
        .find(|func| func.locals.iter().any(|slot| slot.name == local))
        .expect("function")
}

#[test]
fn reading_a_moving_type_wraps_a_move_and_drops_the_rest() {
    let (diags, hir, _) = check(&format!(
        "{HANDLE}main: function () nothing {{\n    a: Handle = Handle {{ id = 0x1 }}\n    \
         b: Handle = a\n    n: @bit32 = 0x2\n    m: @bit32 = n\n}}\n"
    ));
    assert!(!diags.has_errors(), "{:?}", messages(&diags));
    let main = body_of(&hir, "b");
    let stmts = &main.body.as_ref().expect("body").stmts;
    // `b = a` moves; `m = n` copies.
    assert!(
        matches!(&stmts[1], HirStmt::Let { value, .. } if matches!(value.kind, HirExprKind::Move(_)))
    );
    assert!(
        matches!(&stmts[3], HirStmt::Let { value, .. } if matches!(value.kind, HirExprKind::Local(_)))
    );
    // Only `b` is full at the end: one drop, no flag.
    let Some(HirStmt::Drop { drops, .. }) = stmts.last() else {
        panic!("no drop at the end: {stmts:?}");
    };
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0].flag, None);
    assert_eq!(main.local(drops[0].path.local).name, "b");
}

#[test]
fn a_move_on_one_branch_gets_a_drop_flag() {
    let (diags, hir, _) = check(&format!(
        "{HANDLE}main: function () nothing {{\n    a: Handle = Handle {{ id = 0x1 }}\n    \
         if 0x1 {{\n        take(a)\n    }}\n}}\n"
    ));
    assert!(!diags.has_errors(), "{:?}", messages(&diags));
    let main = body_of(&hir, "a");
    assert_eq!(main.flags.len(), 1);
    let flag = main.flags[0].1;
    let Some(HirStmt::Drop { drops, .. }) = main.body.as_ref().expect("body").stmts.last() else {
        panic!("no drop at the end");
    };
    assert_eq!(drops[0].flag, Some(flag));
}

#[test]
fn an_out_is_empty_until_its_first_assignment() {
    let (diags, hir, _) = check(&format!(
        "{HANDLE}make: function () made: Handle {{\n    made = Handle {{ id = 0x1 }}\n    \
         made = Handle {{ id = 0x2 }}\n}}\nmain: function () nothing {{\n}}\n"
    ));
    assert!(!diags.has_errors(), "{:?}", messages(&diags));
    let make = body_of(&hir, "made");
    let stmts = &make.body.as_ref().expect("body").stmts;
    assert!(
        matches!(&stmts[0], HirStmt::Assign { old: lilac_hir::HirOld::Empty(drops), .. } if drops.is_empty())
    );
    assert!(matches!(
        &stmts[1],
        HirStmt::Assign {
            old: lilac_hir::HirOld::Full,
            ..
        }
    ));
    // The out is never dropped on return.
    assert!(!stmts
        .iter()
        .any(|stmt| matches!(stmt, HirStmt::Drop { .. })));
}

#[test]
fn ownership_errors_have_their_codes() {
    for (body, code) in [
        (
            "    a: Handle = Handle { id = 0x1 }\n    take(a)\n    take(a)\n",
            "E063",
        ),
        (
            "    a: Handle = Handle { id = 0x1 }\n    r: ref Handle = ref a\n    take(r)\n",
            "E064",
        ),
        (
            "    a: @bit32 = 0x1\n    b: @bit32 = @move(a)\n    c: @bit32 = a\n",
            "E063",
        ),
        ("    b: @bit32 = @move(0x1)\n", "E064"),
        (
            "    a: Handle = Handle { id = 0x1 }\n    @discard(a)\n",
            "E066",
        ),
        (
            "    a: Handle = Handle { id = 0x1 }\n    @drop(a)\n",
            "E010",
        ),
    ] {
        let diags = check_src(&format!("{HANDLE}main: function () nothing {{\n{body}}}\n"));
        assert!(has_code(&diags, code), "{body}: {:?}", messages(&diags));
    }
    let diags = check_src(
        "half: function (n: @bit8) h: @bit8 {\n    if n {\n        return\n    }\n    h = n\n}\n",
    );
    assert!(has_code(&diags, "E065"), "{:?}", messages(&diags));
}

#[test]
fn a_generic_that_moves_its_argument_twice_fails_as_e042() {
    // The template copies `T`; the `Handle` instance cannot.
    let diags = check_src(&format!(
        "{HANDLE}twice[T]: function (x: T) nothing {{\n    a: T = x\n    b: T = x\n}}\n\
         copies: function () nothing {{\n    twice[@bit32](0x1)\n}}\n\
         main: function () nothing {{\n    twice[Handle](Handle {{ id = 0x1 }})\n}}\n"
    ));
    assert_eq!(
        diags
            .diagnostics()
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>(),
        ["E042"],
        "{:?}",
        messages(&diags)
    );
}

/// A value that cannot be copied does not move implicitly once a `ref` to
/// it, or to a field, was taken on a path that reaches the use (`E079`):
/// `ref`, a `self` receiver, a by-reference capture, and a `ref` taken
/// later in a loop's body all borrow. `@move`, a copyable type and a
/// borrow on another branch do not stop it.
#[test]
fn borrowed_values_move_only_by_move() {
    let prefix = format!(
        "{HANDLE}Pair: structure {{\n    h: Handle\n}}\n\
         Slot: either {{\n    Empty\n    Full: Handle\n}}\n\
         peek: function (handle: ref Handle) nothing {{\n}}\n"
    );
    let a = "    a: Handle = Handle { id = 0x1 }\n";
    for (body, code) in [
        ("    r: ref Handle = ref a\n    take(a)\n", Some("E079")),
        ("    peek(ref a)\n    b: Handle = a\n", Some("E079")),
        (
            "    p: Pair = Pair { h = a }\n    r: ref Handle = ref p.h\n    q: Pair = p\n",
            Some("E079"),
        ),
        (
            "    p: Pair = Pair { h = a }\n    r: ref Pair = ref p\n    h: Handle = p.h\n",
            Some("E079"),
        ),
        (
            "    loop if c {\n        take(a)\n        a = Handle { id = 0x2 }\n        \
             r: ref Handle = ref a\n    } else break\n",
            Some("E079"),
        ),
        (
            "    s: Slot = .Full(a)\n    r: ref Slot = ref s\n    \
             match s {\n        .Full -> h: Handle {\n            take(h)\n        }\n    } else {}\n",
            Some("E079"),
        ),
        (
            "    s: Slot = .Full(a)\n    match s {\n        .Full -> h: ref Handle {\n        }\n    } else {}\n    \
             take2: Slot = s\n",
            Some("E079"),
        ),
        ("    r: ref Handle = ref a\n    take(@move(a))\n", None),
        (
            "    n: @bit32 = 0x1\n    r: ref @bit32 = ref n\n    m: @bit32 = n\n",
            None,
        ),
        (
            "    if c {\n        r: ref Handle = ref a\n    } else {\n        take(a)\n    }\n",
            None,
        ),
        ("    take(a)\n    a = Handle { id = 0x2 }\n", None),
    ] {
        let diags = check_src(&format!(
            "{prefix}run: function (c: @bit8) nothing {{\n{a}{body}}}\n\
             main: function () nothing {{\n}}\n"
        ));
        let codes: Vec<&str> = codes_of(&diags);
        match code {
            Some(code) => assert_eq!(codes, [code], "{body}: {:?}", messages(&diags)),
            None => assert!(codes.is_empty(), "{body}: {:?}", messages(&diags)),
        }
    }
}

/// `E079` names the `ref` and offers `@move` as a fix.
#[test]
fn borrowed_move_points_at_the_ref_and_offers_move() {
    let diags = check_src(&format!(
        "{HANDLE}main: function () nothing {{\n    a: Handle = Handle {{ id = 0x1 }}\n    \
         r: ref Handle = ref a\n    take(a)\n}}\n"
    ));
    let [diagnostic] = diags.diagnostics() else {
        panic!("{:?}", messages(&diags));
    };
    assert_eq!(diagnostic.code, "E079");
    assert_eq!(diagnostic.labels.len(), 2);
    let fixes: Vec<&str> = diagnostic
        .suggestions()
        .map(|fix| fix.replacement.as_str())
        .collect();
    assert_eq!(fixes, ["@move(a)"]);
}

/// `E063` after an implicit move says which type stopped the copy: the
/// moved type, or a type it holds. An `@move` needs no reason.
#[test]
fn use_after_implicit_move_says_why_it_moved() {
    let explain = |body: &str| {
        let diags = check_src(&format!(
            "{HANDLE}Pair: structure {{\n    handle: Handle\n    n: @bit32\n}}\n\
             keep: function (pair: Pair) nothing {{\n}}\n\
             main: function () nothing {{\n{body}}}\n"
        ));
        let [diagnostic] = diags.diagnostics() else {
            panic!("{:?}", messages(&diags));
        };
        assert_eq!(diagnostic.code, "E063");
        let moved = diagnostic
            .labels
            .iter()
            .find(|label| !label.primary)
            .expect("a label at the move");
        (moved.msg.clone(), diagnostic.notes.clone())
    };
    let (label, notes) = explain("    a: Handle = Handle { id = 0x1 }\n    take(a)\n    take(a)\n");
    assert_eq!(label, "moved here: `Handle` cannot be copied");
    assert_eq!(
        notes,
        ["`Handle` has an `@on_drop` and no `@on_copy`, so a use by value moves it"]
    );
    let (label, notes) = explain(
        "    p: Pair = Pair { handle = Handle { id = 0x1 }, n = 0x2 }\n    keep(p)\n    keep(p)\n",
    );
    assert_eq!(label, "moved here: `Pair` cannot be copied");
    assert_eq!(
        notes,
        ["`Pair` holds a `Handle`, which has an `@on_drop` and no `@on_copy`, so a use by value moves it"]
    );
    let (label, notes) =
        explain("    a: Handle = Handle { id = 0x1 }\n    take(@move(a))\n    take(a)\n");
    assert_eq!(label, "moved here");
    assert!(notes.is_empty(), "{notes:?}");
}

/// A cancelled check stops before the next instance: nothing more is
/// checked, and it reports nothing about what it skipped.
#[test]
fn a_cancelled_check_stops() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let src = "f: function () nothing {\n    x: @bit8 = nope\n}\nmain: function () nothing {\n    f()\n}\n";
    let mut sm = SourceMap::new();
    let mut diags = Diags::new();
    let project = lilac_module::single_file("test.lilac", src, &mut sm);
    let (programs, _) = lilac_module::parse_all(&project, &mut diags, false);
    let res = lilac_resolve::resolve(&project, &programs, &mut diags);
    let mut types = Types::new();
    let options = crate::Options {
        target: lilac_target::Target::default(),
        require_entry: false,
        cancel: Some(Arc::new(AtomicBool::new(true))),
    };
    let checked = crate::check(&res, &mut types, &mut diags, options);
    assert!(checked.hir.funcs.is_empty(), "{:?}", checked.hir.funcs);
    assert!(!diags.has_errors(), "{diags:?}");
}

/// `E067`: a field that takes an object past the target's limit is
/// reported at that field, with the size reached; 32-bit targets stop at
/// 2^31 - 1 bytes.
#[test]
fn oversized_object_is_e067_at_the_crossing_field() {
    let src = "Big: structure {\n    a: @bit64\n    b: @array(@bit8, 0x7FFFFFF9)\n}\n";
    let (diags, ..) = run_native(src, 32);
    let e067: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code == "E067")
        .collect();
    assert_eq!(e067.len(), 1, "{diags:?}");
    let label = &e067[0].labels[0];
    assert!(
        label.msg == "this field brings `Big` to 2147483649 bytes",
        "{label:?}"
    );
    assert!(
        e067[0]
            .notes
            .iter()
            .any(|note| note.contains("at most 2147483647 bytes")),
        "{diags:?}"
    );
    // The same object fits a 64-bit target.
    assert!(!check_src(src).has_errors());
}

/// An enum is a tagged union: its payloads share one area, so two 1 GiB
/// payloads fit a 32-bit target, and `E067` is reported at the variant
/// whose payload takes the union past the limit.
#[test]
fn enum_sizes_are_tagged_unions() {
    let fits =
        "Two: either {\n    A: @array(@bit8, 0x40000000)\n    B: @array(@bit8, 0x40000000)\n}\n";
    let (diags, ..) = run_native(fits, 32);
    assert!(!diags.has_errors(), "{diags:?}");
    let src = "Big: either {\n    A: @bit8\n    B: @array(@bit8, 0x7FFFFFFF)\n    C\n}\n";
    let (diags, _, _, sm, _) = run_native(src, 32);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E067]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("this variant brings `Big` to 2147483648 bytes"),
        "{rendered}"
    );
    assert!(rendered.contains("test.lilac:3:5"), "{rendered}");
}

/// An array whose elements together cross the limit is reported at its
/// length, never wrapped (problem 2).
#[test]
fn oversized_array_is_e067_at_its_length() {
    let src = "Big: structure {\n    a: @array(@array(@bit64, 0xFFFFFFFF), 0xFFFFFFFF)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E067]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("this length makes the array 147573952520956936200 bytes"),
        "{rendered}"
    );
    assert!(rendered.contains("test.lilac:2:43"), "{rendered}");
}

/// A generic instance that builds an oversized type fails as `E042`, and
/// its template alone reports nothing.
#[test]
fn oversized_generic_instance_folds_into_e042() {
    let src = "Row: structure {\n    a: @array(@bit64, 0xFFFFFFFF)\n}\n\nmake[T]: function () nothing {\n    rows: @array(T, 0xFFFFFFFF) = @array(T, 0xFFFFFFFF) {}\n}\n\nmain: function () nothing {\n    make[Row]()\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(!rendered.contains("error[E067]"), "{rendered}");
    assert!(
        rendered.contains("this length makes the array"),
        "{rendered}"
    );
    let template =
        "make[T]: function () nothing {\n    rows: @array(T, 0xFFFFFFFF) = @array(T, 0xFFFFFFFF) {}\n}\n";
    assert!(!check_src(template).has_errors());
}

/// The codes of every diagnostic, in order.
fn codes_of(diags: &Diags) -> Vec<&'static str> {
    diags.diagnostics().iter().map(|d| d.code).collect()
}

const SELF_CONTAINING: &str = "E: either {\n    A: E\n    B\n}\n\n";

/// A type that failed (`E053`) is an error type for every later use: a tag
/// or literal checked against it asks for no type (no `E024`), a value of
/// it is not "not callable", and an inferred literal stays silent.
#[test]
fn failed_type_is_an_error_type_for_later_uses() {
    let uses = [
        // A binding, an assignment and a payload.
        "main: function () nothing {\n    e: E = .B\n    e = .A(.B)\n    f: E = 0x1\n}\n",
        // A constant and a global, and assigning the global.
        "C: E => .B\nG: E = .B\nmain: function () nothing {\n    G = .B\n}\n",
        // A comparison, either way round, and a cast.
        "make: function () out: E {\n    out = .B\n}\nmain: function () nothing {\n    x: E = make()\n    if x == .B {\n    }\n    if .B == x {\n    }\n    y: E = .B as E\n}\n",
        // An `if` value whose `then` has the failed type.
        "make: function () out: E {\n    out = .B\n}\nmain: function () nothing {\n    x: E = make()\n    y: E = if x == .B then x else .B\n}\n",
        // A struct literal inferred from it, and one of a type holding it.
        "Box[T]: structure {\n    v: T\n}\nHolder: structure {\n    e: E\n}\nmain: function () nothing {\n    b: Box[E] = Box { v = .B }\n    h: Holder = Holder { e = .B }\n}\n",
        // Calling a local whose function type names it.
        "take: function (e: E) nothing {\n}\nmain: function () nothing {\n    f: ref function (e: E) nothing = ref take\n    f(.B)\n}\n",
    ];
    for uses in uses {
        let src = format!("{SELF_CONTAINING}{uses}");
        assert_eq!(codes_of(&check_src(&src)), ["E053"], "{src}");
    }
}

/// Errors that do not follow from the failed type are still reported:
/// inside a global's initializer, a failed struct literal's values, and
/// an assignment whose place failed.
#[test]
fn failed_type_hides_no_other_error() {
    let src = format!(
        "{SELF_CONTAINING}G: E = nope\nmain: function () nothing {{\n    b: E = E {{ v = nope }}\n    missing = .B\n}}\n"
    );
    assert_eq!(codes_of(&check_src(&src)), ["E053", "E021", "E021", "E021"]);
}

/// A slot after one that failed is resolved but not laid out, so a type
/// that grows and meets itself reports once, not once per level, and a
/// later field's unknown name is still reported.
#[test]
fn failed_layout_stops_at_its_first_slot() {
    let growing =
        "W[T]: structure {\n    v: T\n    w: W[W[T]]\n}\nmain: function () nothing {\n    x: W[@bit8] = .B\n}\n";
    assert_eq!(codes_of(&check_src(growing)), ["E053"]);
    let deeper =
        "W[T]: structure {\n    w: W[W[T]]\n    v: T\n}\nmain: function () nothing {\n    x: W[@bit8] = .B\n}\n";
    assert_eq!(codes_of(&check_src(deeper)), ["E051"]);
    let named = "O: structure {\n    a: O\n    b: Nope\n}\n";
    assert_eq!(codes_of(&check_src(named)), ["E053", "E023"]);
    let two_cycles =
        "A: structure {\n    b: B\n    c: C\n}\nB: structure {\n    a: A\n}\nC: structure {\n    a: A\n}\n";
    assert_eq!(codes_of(&check_src(two_cycles)), ["E053"]);
}

/// An object too large for the target (`E067`) is an error type too.
#[test]
fn oversized_type_is_an_error_type_for_later_uses() {
    let src = "Big: structure {\n    a: @array(@array(@bit64, 0xFFFFFFFF), 0xFFFFFFFF)\n}\n\nmain: function () nothing {\n    b: Big = .B\n    c: Big = Big { a = .X }\n}\n";
    assert_eq!(codes_of(&check_src(src)), ["E067"]);
}

// ---- type aliases -------------------------------------------------------

const BOX: &str = "Box[T]: structure {\n    value: T\n    make: function (value: T) b: Box[T] {\n        b = Box[T] { value = value }\n    }\n}\n";

/// An alias is its type: values move between the two spellings with no
/// conversion, through statics, struct literals and generic instances.
#[test]
fn an_alias_is_the_type_it_names() {
    let src = format!(
        "{BOX}Small: alias Box[@bit8]\nByte: alias @bit8\nOther: alias Small\n\
         main: function () nothing {{\n    \
             a: Small = Small.make(0x1)\n    \
             b: Box[@bit8] = a\n    \
             c: Other = Small {{ value = 0x2 }}\n    \
             d: Byte = c.value\n    \
             e: ref Small = ref b\n\
         }}\n"
    );
    let diags = check_src(&src);
    assert!(!diags.has_errors(), "{diags:?}");
}

/// Two hooks whose signatures differ only in spelling are one signature.
#[test]
fn hooks_see_through_aliases() {
    let src = format!(
        "{BOX}Small: alias Box[@bit8]\n\
         @negate: function (value: Small) Small => value\n\
         @negate: function (value: Box[@bit8]) Box[@bit8] => value\n"
    );
    assert!(has_code(&check_src(&src), "E020"));
}

/// A type written with arguments that one alias names displays as the
/// alias; a plain name never does, and two aliases of one type leave it
/// as written.
#[test]
fn diagnostics_name_types_through_their_alias() {
    let src = format!(
        "{BOX}Small: alias Box[@bit8]\nByte: alias @bit8\n\
         main: function () nothing {{\n    a: Small = Small.make(0x1)\n    b: @bit32 = a\n    c: Byte = 0x1\n    d: @bit32 = c\n}}\n"
    );
    let shown = messages(&check_src(&src));
    assert!(
        shown
            .iter()
            .any(|m| m == "no conversion from Small to @bit32"),
        "{shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|m| m == "no conversion from @bit8 to @bit32"),
        "{shown:?}"
    );
    let twice = format!(
        "{BOX}Small: alias Box[@bit8]\nTiny: alias Box[@bit8]\n\
         main: function () nothing {{\n    a: Small = Small.make(0x1)\n    b: @bit32 = a\n}}\n"
    );
    let shown = messages(&check_src(&twice));
    assert!(
        shown
            .iter()
            .any(|m| m == "no conversion from Box[@bit8] to @bit32"),
        "{shown:?}"
    );
}

#[test]
fn an_alias_that_reaches_itself_is_e074_once() {
    let direct = format!("{BOX}Loop: alias Box[Loop]\nuse: function (x: Loop) nothing {{}}\n");
    assert_eq!(codes_of(&check_src(&direct)), ["E074"]);
    let chain = "First: alias Second\nSecond: alias Third\nThird: alias First\nuse: function (x: First, y: Second) nothing {}\n";
    let diags = check_src(chain);
    assert_eq!(codes_of(&diags), ["E074"]);
    assert_eq!(messages(&diags), ["alias `First` refers to itself"]);
}

#[test]
fn arguments_on_an_alias_are_e044() {
    let src = format!(
        "{BOX}Small: alias Box[@bit8]\nBare: alias Box\nuse: function (x: Small[@bit8]) nothing {{}}\n"
    );
    assert_eq!(codes_of(&check_src(&src)), ["E044", "E044"]);
}

/// An alias's error is reported once, at the alias, even when only a
/// generic instance names it.
#[test]
fn an_alias_error_is_reported_at_the_alias() {
    let src = format!(
        "{BOX}Broken: alias Box[Missing]\n\
         take[T]: function (value: T) nothing {{\n    size: @bit_native = @size_of(Broken)\n}}\n\
         main: function () nothing {{\n    take(0x1 as @bit8)\n}}\n"
    );
    let diags = check_src(&src);
    assert_eq!(codes_of(&diags)[0], "E023", "{diags:?}");
    assert!(!has_code(&diags, "E042"), "{diags:?}");
}

#[test]
fn an_alias_is_not_a_value() {
    let diags =
        check_src("Byte: alias @bit8\nmain: function () nothing {\n    x: @bit8 = Byte\n}\n");
    assert_eq!(codes_of(&diags), ["E021"]);
}

/// `Pair[T]` whose `@add` checks only for a `T` with an `@add` of its own.
const PAIR: &str = "@add: function (lhs: @bit32, rhs: @bit32) out: @bit32 {\n    \
    out = @i_add_32(lhs, rhs)\n}\n\
    Pair[T]: structure {\n    value: T\n    \
    @add: function (lhs: Pair[T], rhs: Pair[T]) out: Pair[T] {\n        \
    out = Pair[T] { value = lhs.value + rhs.value }\n    }\n}\n";

#[test]
fn a_generic_hook_is_checked_only_for_the_arguments_it_is_used_with() {
    // `Pair[@bit8].@add` would fail (no `@add` for `@bit8`), but nothing
    // adds two `Pair[@bit8]`s.
    let diags = check_src(&format!(
        "{PAIR}main: function () nothing {{\n    \
         a: Pair[@bit32] = Pair {{ value = 0x1 }}\n    b: Pair[@bit32] = a + a\n    \
         c: Pair[@bit8] = Pair {{ value = 0x1 }}\n}}\n"
    ));
    assert_eq!(
        codes_of(&diags),
        Vec::<&str>::new(),
        "{:?}",
        messages(&diags)
    );
}

#[test]
fn a_generic_hook_that_fails_is_e042_where_it_is_used() {
    for use_ in ["c + c", "@add(c, c)"] {
        let src = format!(
            "{PAIR}main: function () nothing {{\n    \
             c: Pair[@bit8] = Pair {{ value = 0x1 }}\n    d: Pair[@bit8] = {use_}\n}}\n"
        );
        let diags = check_src(&src);
        assert_eq!(codes_of(&diags), ["E042"], "{:?}", messages(&diags));
        let shown = diags.diagnostics()[0]
            .labels
            .iter()
            .filter(|label| label.primary)
            .map(|label| &src[label.span.lo.0 as usize..label.span.hi.0 as usize])
            .collect::<Vec<_>>();
        assert_eq!(shown, [use_], "{use_}");
    }
}

#[test]
fn an_unused_generic_hook_still_reports_what_no_argument_fixes() {
    // `Pair[@bit8]` parks its `@add`; the template check still finds the
    // unresolved name, once.
    let diags = check_src(
        "Pair[T]: structure {\n    value: T\n    \
         @add: function (lhs: Pair[T], rhs: Pair[T]) out: Pair[T] {\n        \
         missing()\n        out = lhs\n    }\n}\n\
         main: function () nothing {\n    c: Pair[@bit8] = Pair { value = 0x1 }\n}\n",
    );
    assert_eq!(messages(&diags), ["unresolved name `missing`"]);
}

const COUNTER: &str = "Counter: structure {\n    n: @bit32\n\n    count: function (self) nothing {\n        self.n += 0x1\n    }\n}\n\
                       @add: function (a: @bit32, b: @bit32) sum: @bit32 {\n    sum = @i_add_32(a, b)\n}\n";

#[test]
fn not_equal_is_not_of_equality() {
    let (diags, hir, _) = check(
        "main: function () nothing {\n    a: @bit8 = 0x1\n    b: @bit8 = 0x2\n    c: @bit8 = a != b\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
    let main = body_of(&hir, "c");
    let HirStmt::Let { value, .. } = &main.body.as_ref().expect("body").stmts[2] else {
        panic!("expected a let");
    };
    let HirExprKind::Not(inner) = &value.kind else {
        panic!("expected `Not`, got {:?}", value.kind);
    };
    assert!(matches!(inner.kind, HirExprKind::Eq(..)));
    let diags = check_src(
        "main: function () nothing {\n    a: @bit8 = 0x1\n    b: @bit32 = 0x2\n    c: @bit8 = a != b\n}\n",
    );
    let msgs = messages(&diags);
    assert!(
        msgs.iter()
            .any(|m| m.contains("`!=` needs identical types")),
        "{msgs:?}"
    );
}

#[test]
fn not_takes_a_condition() {
    let diags = check_src(
        "main: function () nothing {\n    a: @bit8 = 0x1\n    c: @bit8 = not a\n    d: @bit8 = not 0x0\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
    let diags =
        check_src("main: function () nothing {\n    a: @bit32 = 0x1\n    c: @bit8 = not a\n}\n");
    assert!(has_code(&diags, "E010"), "{diags:?}");
    assert!(messages(&diags)
        .iter()
        .any(|m| m.contains("`not` condition")));
    let diags = check_src(&format!(
        "{}main: function () nothing {{\n    a: @bit32 = 0x1\n    c: @bit8 = not a\n}}\n",
        conversion_hook()
    ));
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn compound_assignment_calls_the_hook_and_assigns() {
    let (diags, hir, _) = check(&format!(
        "{COUNTER}main: function () nothing {{\n    x: @bit32 = 0x1\n    x += 0x2\n}}\n"
    ));
    assert!(!diags.has_errors(), "{:?}", messages(&diags));
    let main = body_of(&hir, "x");
    let HirStmt::Assign { place, value, .. } = &main.body.as_ref().expect("body").stmts[1] else {
        panic!("expected an assignment");
    };
    assert!(matches!(place.kind, HirExprKind::Local(_)));
    let HirExprKind::Call { args, .. } = &value.kind else {
        panic!("expected the `@add` call, got {:?}", value.kind);
    };
    assert!(matches!(args[0].kind, HirExprKind::Local(_)));
    assert!(matches!(args[1].kind, HirExprKind::Int(2)));
}

#[test]
fn compound_assignment_with_no_hook_is_e043_at_the_operator() {
    let src = "main: function () nothing {\n    x: @bit32 = 0x1\n    x -= 0x2\n}\n";
    let diags = check_src(src);
    let [error] = diags.diagnostics() else {
        panic!("one error: {diags:?}");
    };
    assert_eq!(error.code, "E043");
    let label = error
        .labels
        .iter()
        .find(|label| label.primary)
        .expect("label");
    let at = u32::try_from(src.find("-=").expect("operator")).expect("small");
    assert_eq!((label.span.lo.0, label.span.hi.0), (at, at + 2));
    assert!(error
        .notes
        .iter()
        .any(|note| note.contains("`x -= e` is `x = x - e`")));
}

#[test]
fn compound_assignment_through_a_ref_receiver() {
    let diags = check_src(&format!("{COUNTER}main: function () nothing {{\n}}\n"));
    assert!(!diags.has_errors(), "{:?}", messages(&diags));
}

#[test]
fn compound_assignment_moves_a_non_copyable_target() {
    let (diags, hir, _) = check(&format!(
        "{HANDLE}@add: function (a: Handle, b: @bit32) sum: Handle {{\n    sum = Handle {{ id = b }}\n    take(a)\n}}\n\
         main: function () nothing {{\n    h: Handle = Handle {{ id = 0x1 }}\n    n: @bit32 = 0x2\n    h += n\n}}\n"
    ));
    assert!(!diags.has_errors(), "{:?}", messages(&diags));
    let main = body_of(&hir, "h");
    let HirStmt::Assign { value, old, .. } = &main.body.as_ref().expect("body").stmts[2] else {
        panic!("expected an assignment");
    };
    assert!(matches!(old, lilac_hir::HirOld::Empty(_)), "{old:?}");
    let HirExprKind::Call { args, .. } = &value.kind else {
        panic!("expected the `@add` call");
    };
    assert!(matches!(args[0].kind, HirExprKind::Move(_)));
}

#[test]
fn compound_assignment_in_a_generic_folds_into_e042() {
    let diags = check_src(&format!(
        "{COUNTER}bump[T]: function (x: T, y: T) nothing {{\n    x += y\n}}\n\
         main: function () nothing {{\n    bump[@bit32](0x1, 0x2)\n    bump[@bit8](0x1, 0x2)\n}}\n"
    ));
    assert!(has_code(&diags, "E042"), "{diags:?}");
    assert!(!has_code(&diags, "E043"), "{diags:?}");
}

const CELL: &str = "Point: structure {\n    x: @bit32\n    y: @bit32\n}\n\
                    Cell: structure {\n    p: Point\n\n    \
                    get: function (self) value: ref Point {\n        value = ref self.p\n    }\n\n    \
                    copy: function (self) value: Point => self.p\n\n    \
                    raw: function (self) value: ref unknown => (ref self.p) as ref unknown\n}\n";

/// The statements of the function declaring `local`.
fn stmts_of<'a>(hir: &'a HirProgram, local: &str) -> &'a [HirStmt] {
    &body_of(hir, local).body.as_ref().expect("body").stmts
}

#[test]
fn assigning_through_a_returned_ref_writes_through_it() {
    let (diags, hir, _) = check(&format!(
        "{CELL}main: function () nothing {{\n    c: Cell = Cell {{ p = Point {{ x = 0x1, y = 0x2 }} }}\n    \
         c.get() = Point {{ x = 0x3, y = 0x4 }}\n    c.get().y = 0x5\n}}\n"
    ));
    assert!(!diags.has_errors(), "{:?}", messages(&diags));
    let stmts = stmts_of(&hir, "c");
    let HirStmt::Assign { place, .. } = &stmts[1] else {
        panic!("expected an assignment, got {:?}", stmts[1]);
    };
    let HirExprKind::Deref(call) = &place.kind else {
        panic!("expected a place through the `ref`, got {:?}", place.kind);
    };
    assert!(matches!(call.kind, HirExprKind::Call { .. }));
    let HirStmt::Assign { place, .. } = &stmts[2] else {
        panic!("expected an assignment, got {:?}", stmts[2]);
    };
    let HirExprKind::Field { base, index: 1 } = &place.kind else {
        panic!("expected field `y`, got {:?}", place.kind);
    };
    assert!(matches!(base.kind, HirExprKind::Deref(_)));
}

#[test]
fn assigning_to_a_call_without_a_ref_is_e010() {
    for call in ["c.copy()", "c.raw()"] {
        let diags = check_src(&format!(
            "{CELL}main: function () nothing {{\n    c: Cell = Cell {{ p = Point {{ x = 0x1, y = 0x2 }} }}\n    \
             {call} = Point {{ x = 0x3, y = 0x4 }}\n}}\n"
        ));
        assert_eq!(codes_of(&diags), ["E010"], "{call}: {diags:?}");
        assert!(messages(&diags)
            .iter()
            .any(|m| m.contains("cannot assign to a call's result")));
    }
}

#[test]
fn compound_assignment_through_a_call_calls_it_once() {
    let (diags, hir, _) = check(&format!(
        "{CELL}{COUNTER}main: function () nothing {{\n    c: Cell = Cell {{ p = Point {{ x = 0x1, y = 0x2 }} }}\n    \
         c.get().x += 0x3\n}}\n"
    ));
    assert!(!diags.has_errors(), "{:?}", messages(&diags));
    let stmts = stmts_of(&hir, "c");
    // The call's `ref` is bound once, then read and written through.
    let HirStmt::Let { local, value } = &stmts[1] else {
        panic!("expected the call bound first, got {:?}", stmts[1]);
    };
    assert!(matches!(value.kind, HirExprKind::Call { .. }));
    let HirStmt::Assign { place, value, .. } = &stmts[2] else {
        panic!("expected an assignment, got {:?}", stmts[2]);
    };
    let through_hidden = |expr: &lilac_hir::HirExpr| {
        matches!(&expr.kind, HirExprKind::Field { base, .. }
            if matches!(&base.kind, HirExprKind::Deref(inner)
                if matches!(inner.kind, HirExprKind::Local(l) if l == *local)))
    };
    assert!(through_hidden(place), "{place:?}");
    let HirExprKind::Call { args, .. } = &value.kind else {
        panic!("expected the `@add` call, got {:?}", value.kind);
    };
    assert!(through_hidden(&args[0]), "{:?}", args[0]);
}

const ADD_ONE: &str =
    "add_one: function (value: @bit32) result: @bit32 {\n    result = @i_add_32(value, 0x1)\n}\n";

/// A function reference points at code: nothing reads, writes or drops
/// through it, it has no fields, and `==` needs one function type.
#[test]
fn function_references_are_never_dereferenced() {
    for (body, code) in [
        ("    x: @bit32 = f.value\n", "E021"),
        ("    x: @bit32 = @read(f)\n", "E010"),
        ("    @drop(f)\n", "E010"),
        (
            "    g: ref function (@bit64) @bit64 = @address_to_ptr(0x0) as ref function (@bit64) @bit64\n    if f == g {\n    }\n",
            "E010",
        ),
        ("    g: ref unknown = f as ref unknown\n    if f == g {\n    }\n", "E010"),
    ] {
        let src = format!(
            "{ADD_ONE}main: function () nothing {{\n    f: ref function (@bit32) @bit32 = ref add_one\n{body}}}\n"
        );
        assert_eq!(codes_of(&check_src(&src)), [code], "{src}");
    }
}

/// Assigning a function reference rebinds the place, through a `ref` to
/// it too, and the same function compares equal.
#[test]
fn function_references_rebind_and_compare() {
    let src = format!(
        "{ADD_ONE}main: function () nothing {{\n    f: ref function (@bit32) @bit32 = ref add_one\n    f = ref add_one\n    if f == ref add_one {{\n    }}\n}}\n"
    );
    assert!(!check_src(&src).has_errors(), "{src}");
    let src = format!(
        "{ADD_ONE}Holder: structure {{\n    f: ref function (@bit32) @bit32\n}}\nmain: function () nothing {{\n    holder: Holder = Holder {{ f = ref add_one }}\n    through: ref Holder = ref holder\n    through.f = ref add_one\n    x: @bit32 = through.f(0x1)\n    if holder.f == through.f {{\n    }}\n}}\n"
    );
    assert!(!check_src(&src).has_errors(), "{src}");
}

/// `E077` recovers as the `ref` type: one error, and later uses check.
#[test]
fn bare_function_type_reports_once() {
    let src = format!(
        "{ADD_ONE}Callback: alias function (@bit32) @bit32\nmain: function () nothing {{\n    f: function (@bit32) @bit32 = ref add_one\n    x: @bit32 = f(0x1)\n    g: Callback = ref add_one\n    y: @bit32 = g(x)\n    h: ref function (@bit32) @bit32 = add_one\n    z: @bit32 = h(y)\n}}\n"
    );
    assert_eq!(
        codes_of(&check_src(&src)),
        ["E077", "E077", "E077"],
        "{src}"
    );
}

/// A template's local and field types are laid out (unsized), so a tag
/// finds the variants of an enum over a parameter, and a misspelled one
/// is still `E021`.
#[test]
fn template_tags_of_enums_over_parameters() {
    let opt = "Opt[T]: either {\n    None\n    Some: T\n}\n";
    let src = format!(
        "{opt}Holder[T]: structure {{\n    data: Opt[ref T]\n    make: function () h: Holder[T] {{\n        h = Holder[T] {{ data = .None }}\n    }}\n}}\nf[T]: function () nothing {{\n    a: Opt[T] = .None\n    b: Opt[ref T] = .None\n    match b {{\n        .None => {{}}\n    }} else {{}}\n}}\nmain: function () nothing {{}}\n"
    );
    assert!(!check_src(&src).has_errors(), "{src}");
    let src = format!("{opt}f[T]: function () nothing {{\n    a: Opt[ref T] = .Nnoe\n}}\nmain: function () nothing {{}}\n");
    assert_eq!(codes_of(&check_src(&src)), ["E021"], "{src}");
}

/// In a template, `value.f(x)` is decided per instance: a method in one,
/// a function-reference field in another.
#[test]
fn template_calls_a_method_or_a_field() {
    let src = format!(
        "{ADD_ONE}WithMethod: structure {{\n    f: function (self, value: @bit32) result: @bit32 {{\n        result = value\n    }}\n}}\nWithField: structure {{\n    f: ref function (@bit32) @bit32\n}}\nrun[T]: function (value: ref T) result: @bit32 {{\n    result = value.f(0x1)\n}}\nmain: function () nothing {{\n    a: WithMethod = WithMethod {{}}\n    b: WithField = WithField {{ f = ref add_one }}\n    x: @bit32 = run[WithMethod](ref a)\n    y: @bit32 = run[WithField](ref b)\n}}\n"
    );
    assert!(!check_src(&src).has_errors(), "{src}");
}

/// `ref` takes a function's address by path or as a type's member; a
/// method's receiver becomes its first parameter, and there are no bound
/// methods.
#[test]
fn function_addresses_by_path_and_member() {
    let src = format!(
        "{ADD_ONE}Box: structure {{\n    value: @bit32\n    get: function (self) value: @bit32 {{\n        value = self.value\n    }}\n    take: function (self: Box) value: @bit32 {{\n        value = self.value\n    }}\n    make: function () out: Box {{\n        out = Box {{ value = 0x1 }}\n    }}\n}}\nmain: function () nothing {{\n    get: ref function (ref Box) @bit32 = ref Box.get\n    take: ref function (Box) @bit32 = ref Box.take\n    make: ref function () Box = ref Box.make\n    b: Box = make()\n    x: @bit32 = get(ref b)\n}}\n"
    );
    assert!(!check_src(&src).has_errors(), "{src}");
    let src = format!(
        "{ADD_ONE}Box: structure {{\n    value: @bit32\n    get: function (self) value: @bit32 {{\n        value = self.value\n    }}\n}}\nmain: function () nothing {{\n    b: Box = Box {{ value = 0x1 }}\n    get: ref function (ref Box) @bit32 = ref b.get\n}}\n"
    );
    let diags = check_src(&src);
    assert_eq!(codes_of(&diags), ["E010"], "{src}");
    assert!(
        diags.diagnostics()[0]
            .helps
            .iter()
            .any(|help| help.text.contains("`ref Box.get`")),
        "{diags:?}"
    );
}

/// Hooks, failing functions, variadic externs and generic functions are
/// not function values.
#[test]
fn some_functions_are_not_values() {
    for (items, value, code) in [
        (
            "risky: function () value: @bit32 else nothing {\n    value = 0x1\n}\n",
            "ref risky",
            "E057",
        ),
        (
            "printf: extern function (format: ref @bit8, ...) result: @bit32\n",
            "ref printf",
            "E028",
        ),
        (
            "id[T]: function (value: T) out: T {\n    out = value\n}\n",
            "ref id",
            "E044",
        ),
    ] {
        let src = format!("{items}main: function () nothing {{\n    f: ref unknown = {value} as ref unknown\n}}\n");
        assert_eq!(codes_of(&check_src(&src)), [code], "{src}");
    }
}
