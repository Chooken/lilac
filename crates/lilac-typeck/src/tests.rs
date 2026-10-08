//! Checker tests: each runs the real pipeline front half (module, parse,
//! resolve, check) on a single in-memory file.

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
    let checked = crate::check(&res, &mut types, &mut diags, native_bits);
    (diags, checked.hir, types, sm, res)
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
    diags.diagnostics().iter().map(|d| d.msg.clone()).collect()
}

fn render_all(diags: &Diags, sm: &SourceMap) -> String {
    use lilac_error::{Emitter, EmitterHuman};
    diags
        .diagnostics()
        .iter()
        .map(|diagnostic| EmitterHuman::new().render(diagnostic, sm))
        .collect()
}

/// The checked type and source bytes of the literal in `main`'s first
/// statement: a bare literal statement takes no expected type, while
/// `x: T = lit` takes `T`.
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
        HirExprKind::Number(bytes) => bytes.clone(),
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
    "Int8: object { private { data: @bit8 } @conversion: function (from: @bit8) to: Int8 { to = Int8 { data = from } } }\n\
     Int16: object { private { data: @bit16 } @conversion: function (from: @bit16) to: Int16 { to = Int16 { data = from } } }\n\
     Int32: object { private { data: @bit32 } @conversion: function (from: @bit32) to: Int32 { to = Int32 { data = from } } }\n\
     Int64: object { private { data: @bit64 } @conversion: function (from: @bit64) to: Int64 { to = Int64 { data = from } } }\n\
     Int: object { private { data: @bit_native } @conversion: function (from: @bit_native) to: Int { to = Int { data = from } } }\n\
     Float32: object { private { data: @fbit32 } @conversion: function (from: @fbit32) to: Float32 { to = Float32 { data = from } } }\n\
     Float64: object { private { data: @fbit64 } @conversion: function (from: @fbit64) to: Float64 { to = Float64 { data = from } } }\n"
}

fn method_src() -> &'static str {
    "Object: object {\n    member: @bit32\n    get: function (instance: ref Object) result: @bit32 {\n        result = instance.member\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    member: @bit32 = variable.get()\n}\n"
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
fn nested_literal_fold_mixed_kind_is_e010() {
    let diags = check_src("main: function () nothing {\n    2 * 3 + 4.0\n}\n");
    assert!(has_code(&diags, "E010"), "{diags:?}");
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
fn literal_fold_mixed_kind_is_e010() {
    let diags = check_src("main: function () nothing {\n    0x2 + 3.0\n}\n");
    assert!(has_code(&diags, "E010"), "{diags:?}");
    let diags = check_src("main: function () nothing {\n    3.0 - 0x2\n}\n");
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
fn fbit128_literal_is_out_of_scope() {
    let diags = check_src("main: function () nothing {\n    x: @fbit128 = 3.14\n}\n");
    assert!(has_code(&diags, "E010"), "{diags:?}");
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
        "Maybe: enum {\n    Some: @bit8\n    None\n}\nmain: function () nothing {\n    x: Maybe = .None\n}\n",
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
        "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    variable_2: @bit8 = variable.data\n}\n",
    );
    assert!(has_code(&diags, "E021"), "{diags:?}");
    assert!(!has_code(&diags, "E040"), "{diags:?}");
}

#[test]
fn calling_a_field_is_e022() {
    let diags = check_src(
        "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    variable.member()\n}\n",
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

#[test]
fn single_let_against_multi_return_is_e022() {
    let diags = check_src(
        "Object1: object {\n    value: @bit8\n}\nmain: function () nothing {\n    result: Object1 = get()\n}\nget: function () result: Object1, result_2: @bit32 {\n    result = Object1 {\n        value = 0x0\n    }\n    result_2 = 0x0\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn structlit_missing_field_is_e022() {
    let diags = check_src(
        "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n    }\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
    let msgs = messages(&diags);
    assert!(msgs.iter().any(|m| m.contains("member")), "{msgs:?}");
}

#[test]
fn structlit_unknown_field_is_e021() {
    let diags = check_src(
        "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object {\n        data = 0x1\n    }\n}\n",
    );
    assert!(has_code(&diags, "E021"), "{diags:?}");
}

#[test]
fn func_value_call_checks() {
    let diags = check_src(
        "main: function () nothing {\n    add: function (@bit32) @bit32 = addOne\n    value: @bit32 = 0x0\n    value = add(value)\n    add = addTwo\n    value = add(value)\n}\naddOne: function (value: @bit32) result: @bit32 {\n    result = @i_add_32(value, 0x1)\n}\naddTwo: function (value: @bit32) result: @bit32 {\n    result = @i_add_32(value, 0x2)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
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
        "Object: object {\n    member: @bit32\n    add: function (num: @bit32) result: @bit32 {\n        result = @i_add_32(num, 0x1)\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    member: @bit32 = Object.add(0x2)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn method_without_self_is_e022() {
    let diags = check_src(
        "Object: object {\n    member: @bit8\n    add: function (num: @bit32) result: @bit32 {\n        result = num\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n    member: @bit32 = variable.add(0x2)\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn factory_call_checks() {
    // `Type(args)` routes to the `@init` factory hook (SPEC §8 RAII).
    let diags = check_src(
        "Object: object {\n    value: @bit8\n    @init: function (value: @bit8) result: Object {\n        result = Object {\n            value = value\n        }\n    }\n}\nmain: function () nothing {\n    variable: Object = Object(0x1)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn factory_missing_init_is_e022() {
    let diags = check_src(
        "Object: object {\n    value: @bit8\n}\nmain: function () nothing {\n    variable: Object = Object(0x1)\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn factory_self_param_is_e022() {
    // Factories take plain parameters (Zig rejects `self` on `@init`).
    let diags = check_src(
        "Object: object {\n    value: @bit8\n    @init: function (self) result: Object {\n        result = Object {\n            value = 0x1\n        }\n    }\n}\nmain: function () nothing {\n    variable: Object = Object(0x1)\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn factory_wrong_return_is_e022() {
    let diags = check_src(
        "Object: object {\n    value: @bit8\n    @init: function (value: @bit8) result: @bit8 {\n        result = value\n    }\n}\nmain: function () nothing {\n    variable: Object = Object(0x1)\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
}

#[test]
fn factory_selects_init_overload_by_arity() {
    // Member hooks overload: `Object(0x1)` must pick the 1-arg `@init`,
    // not the 0-arg one, and `Object()` must pick the 0-arg one.
    let diags = check_src(
        "Object: object {\n    value: @bit8\n    @init: function () result: Object {\n        result = Object { value = 0x1 }\n    }\n    @init: function (value: @bit8) result: Object {\n        result = Object { value = value }\n    }\n}\nmain: function () nothing {\n    a: Object = Object()\n    b: Object = Object(0x1)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn factory_does_not_convert_arguments() {
    // A factory is a hook, so its arguments match exactly: `Object(b)` with
    // `b: @bit8` and an `@bit8 -> @bit32` conversion is `E010`, not a
    // conversion.
    let diags = check_src(
        "Object: object {\n    value: @bit32\n    @init: function (value: @bit32) result: Object {\n        result = Object { value = value }\n    }\n}\n@conversion: function (from: @bit8) to: @bit32 { to = @extend_zero(from) }\nmain: function () nothing {\n    b: @bit8 = 0x2\n    c: Object = Object(b)\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn on_drop_bad_shape_is_e022() {
    let diags = check_src(
        "Object: object {\n    member: @bit8\n    @on_drop: function (member: @bit8) nothing {\n    }\n}\nmain: function () nothing {\n}\n",
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
        "Flag: object {\n    v: @bit8\n    @conversion: function (from: Flag) to: @bit8 {\n        to = from.v\n    }\n}\nmain: function () nothing {\n    f: Flag = Flag { v = 0x1 }\n    if f {\n    }\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn capture_binds_by_name() {
    let diags = check_src(
        "get_multi: function () condition: @bit8, result: @bit32 {\n    condition = 0x1\n    result = 0x2\n}\nmain: function () nothing {\n    if get_multi() -> result: @bit32 {\n    }\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn unknown_capture_is_e022() {
    let diags = check_src(
        "get_multi: function () condition: @bit8, result: @bit32 {\n    condition = 0x1\n    result = 0x2\n}\nmain: function () nothing {\n    if get_multi() -> nope: @bit32 {\n    }\n}\n",
    );
    assert!(has_code(&diags, "E022"), "{diags:?}");
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
        "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    number: OptionalBit32 = .Some(0x1)\n    match number {\n        .Some -> some: @bit32 {\n        }\n        .None => return\n    }\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn match_capture_mismatch_is_e010() {
    let diags = check_src(
        "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    number: OptionalBit32 = .Some(0x1)\n    match number {\n        .Some -> some: @bit16 {\n        }\n        .None => return\n    }\n}\n",
    );
    assert!(has_code(&diags, "E010"), "{diags:?}");
}

#[test]
fn capture_on_bare_tag_is_e022() {
    let diags = check_src(
        "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\nmain: function () nothing {\n    number: OptionalBit32 = .None\n    match number {\n        .Some -> some: @bit32 {\n        }\n        .None -> x: @bit8 {\n        }\n    }\n}\n",
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

#[test]
fn fstring_desugars_to_format_args() {
    let diags = check_src("main: function () nothing {\n    x: @format_args = $\"a\"\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn fstring_hole_missing_conversion_is_e043() {
    let diags =
        check_src("main: function () nothing {\n    name: @bit32 = 0x1\n    x: @format_args = $\"{name}\"\n}\n");
    assert!(has_code(&diags, "E043"), "{diags:?}");
}

#[test]
fn fstring_piece_cap_is_e061() {
    let mut holes = String::new();
    for _ in 0..17 {
        holes.push_str("{name}");
    }
    let src = format!(
        "main: function () nothing {{\n    name: @byte_buffer = \"x\"\n    x: @format_args = $\"{holes}\"\n}}\n"
    );
    let diags = check_src(&src);
    assert!(has_code(&diags, "E061"), "{diags:?}");
}

#[test]
fn nested_private_object_field_resolves() {
    // Regression: a field wrapped in nested `private` blocks must flatten
    // into the object's member fields, not be routed as a top-level item
    // (which silently dropped it and produced a spurious `E021`).
    let src = "Object: object {\n    private {\n        private {\n            f: @bit8\n        }\n    }\n\n    @init: function () result: Object {\n        result = Object {\n            f = 0x1\n        }\n    }\n\n    get: function (instance: ref Object) result: @bit8 {\n        result = instance.f\n    }\n}\n\nmain: function () nothing {\n    variable: Object = Object()\n    value: @bit8 = variable.get()\n}\n";
    let diags = check_src(src);
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn nested_private_object_field_is_e026_outside() {
    let src = "Object: object {\n    private {\n        private {\n            f: @bit8\n        }\n    }\n\n    @init: function () result: Object {\n        result = Object {\n            f = 0x1\n        }\n    }\n}\n\nmain: function () nothing {\n    variable: Object = Object()\n    value: @bit8 = variable.f\n}\n";
    let diags = check_src(src);
    assert!(has_code(&diags, "E026"), "{diags:?}");
    assert!(!has_code(&diags, "E021"), "{diags:?}");
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
        rendered.contains("fn other() -> T"),
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
        !rendered.contains(".1.."),
        "mangled symbol leaked: {rendered}"
    );
}

#[test]
fn value_receiver_shows_required_signature() {
    // A value receiver of generic-param type renders `self: T`; the argument
    // is probe-checked (a decimal literal is `@bit8`) and the return
    // comes from the generic parameter.
    let src = "gen[T]: function (value: T) out: T {\n    out = value.other(5)\n}\n\nmain: function () nothing {\n    x: @bit32 = gen[@bit32](0x1)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("fn other(self: ref T, @bit8) -> T"),
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
        rendered.contains("fn other(self: ref T, U) -> @bit8"),
        "missing generic-param signature: {rendered}"
    );
}

#[test]
fn concrete_return_stays_concrete_in_required_signature() {
    // A return type that is not one of the instantiation arguments (a user
    // object) must render concretely, never as a generic parameter.
    let src = "Widget: object {\n    value: @bit32\n}\n\ngen[T]: function (value: T) out: Widget {\n    out = value.other(5)\n}\n\nmain: function () nothing {\n    x: Widget = gen[@bit32](0x1)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("fn other(self: ref T, @bit8) -> Widget"),
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
        rendered.contains(
            "based on this usage, the generic body requires a field matching:\n      data: T"
        ),
        "missing field note: {rendered}"
    );
}

#[test]
fn concrete_field_type_stays_concrete() {
    // The field note's type comes from the use-site expected type; a user
    // object stays concrete rather than becoming a generic parameter.
    let src = "Widget: object {\n    value: @bit32\n}\n\ngen[T]: function (value: T) out: Widget {\n    out = value.data\n}\n\nmain: function () nothing {\n    x: Widget = gen[@bit32](0x1)\n}\n";
    let (diags, _, _, sm) = run(src);
    let rendered = render_all(&diags, &sm);
    assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    assert!(
        rendered.contains("requires a field matching:\n      data: Widget"),
        "concrete field type not rendered concretely: {rendered}"
    );
}

#[test]
fn concrete_field_miss_has_no_inferred_note() {
    // An ordinary field miss outside a generic instantiation keeps the
    // plain `E021` shape with no inferred-requirement note.
    let src = "Object: object {\n    value: @bit32\n}\n\nmain: function () nothing {\n    o: Object = Object {\n        value = 0x1\n    }\n    x: @bit32 = o.data\n}\n";
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
    let src = "GenObject[T]: object {\n    value: T\n    get: function (self) result: T {\n        result = self.other()\n    }\n}\n\nmain: function () nothing {\n    variable: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    r: @bit32 = variable.get()\n}\n";
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
    assert!(!rendered.contains(".1.."), "{rendered}");
}

#[test]
fn field_miss_has_clean_tone() {
    // A field lookup miss inside a generic body must not leak the internal
    // milestone label; it reads as a requirement phrase.
    let src = "Object: object {\n    value: @bit32\n}\n\ngen[T]: function (v: T) out: @bit8 {\n    out = v.data\n}\n\nmain: function () nothing {\n    o: Object = Object {\n        value = 0x1\n    }\n    x: @bit8 = gen[Object](o)\n}\n";
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
    let src = "GenObject[T]: object {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\n\ngen[T]: function (value: T) out: @bit32 {\n    out = value.geet()\n}\n\nmain: function () nothing {\n    v: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    x: @bit32 = gen[GenObject[@bit32]](v)\n}\n";
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
        "Box[T]: object {\n    value: T\n}\n\ngrow[T]: function () nothing {\n    grow[Box[T]]()\n}\n\nmain: function () nothing {\n    grow[@bit32]()\n}\n",
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
    assert!(!note.contains(".1.."), "mangled symbol leaked: {note}");
}

#[test]
fn cyclic_growing_object_layout_is_e051() {
    let (diags, _, _) = check(
        "GenObj[T]: object {\n    next: GenObj[GenObj[T]]\n}\n\nconsume[T]: function () nothing {\n}\n\nmain: function () nothing {\n    consume[GenObj[@bit32]]()\n}\n",
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
    assert!(!note.contains(".1.."), "mangled symbol leaked: {note}");
}

#[test]
fn bare_generic_object_static_is_e044() {
    let (diags, _, _) = check(
        "GenObj[T]: object {\n    value: T\n    ping: function () nothing {\n    }\n}\n\nmain: function () nothing {\n    GenObj.ping()\n}\n",
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
        "GenObject[T]: object {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\n\ngen[T]: function (value: T) out: @bit32 {\n    out = value.get()\n}\n\nmain: function () nothing {\n    v: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    x: @bit32 = gen[GenObject[@bit32]](v)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn static_method_on_object_ok() {
    let (diags, _, _) = check(
        "Object: object {\n    get: function () result: @bit32 {\n        result = 0x2\n    }\n}\n\ngen[T]: function () out: @bit32 {\n    out = T.get()\n}\n\nmain: function () nothing {\n    x: @bit32 = gen[Object]()\n}\n",
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
    let msgs: Vec<String> = diags.diagnostics().iter().map(|d| d.msg.clone()).collect();
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
        "Int64: object {\n    data: @bit64\n\
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
        "Int64: object {\n    data: @bit64\n\
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
        "Int64: object {\n    data: @bit64\n\
         @conversion: function (from: @bit32) to: @bit8 { to = @truncate(from) }\n}\n\
         @conversion: function (from: @bit32) to: @bit8 { to = @truncate(from) }\n",
    );
    assert!(has_code(&diags, "E020"), "{diags:?}");
}

#[test]
fn private_member_conversion_resolves() {
    let diags = check_src(
        "Int64: object {\n    data: @bit64\n\
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
        "Int64: object {\n    data: @bit64\n\
         add: function (self, x: @bit64) result: @bit64 { result = @i_add_64(x, self.data) }\n\
         @add: function (lhs: Int64, rhs: Int64) Int64 => Int64 { data = @i_add_64(lhs.data, rhs.data) }\n}\n\
         main: function () nothing {\n    a: Int64 = Int64 { data = 0x1 }\n    b: Int64 = a + a\n    c: @bit64 = a.add(0x2)\n}\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
}

#[test]
fn generic_object_hook_not_e052() {
    let diags = check_src(
        "GenObj[T]: object {\n    value: T\n    @on_drop: function (from: ref GenObj[T]) nothing {\n    }\n}\nmain: function () nothing {\n    g: GenObj[@bit8] = GenObj[@bit8] { value = 0x1 }\n}\n",
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
        "Box[T]: object {\n    value: T\n}\nmain: function () nothing {\n    b: Box[@bit8, @bit8] = Box[@bit8] { value = 0x1 }\n}\n",
    );
    assert!(has_code(&diags, "E044"), "{diags:?}");
}

#[test]
fn self_containing_type_is_e053() {
    let diags = check_src(
        "Node: object {\n    next: Node\n}\nmain: function () nothing {\n    n: Node\n}\n",
    );
    assert!(has_code(&diags, "E053"), "{diags:?}");
    // Through a reference is fine.
    let diags =
        check_src("Node: object {\n    next: ref Node\n}\nmain: function () nothing {\n}\n");
    assert!(!diags.has_errors(), "{diags:?}");
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
