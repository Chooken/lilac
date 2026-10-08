use super::*;
use lilac_error::Diags;
use lilac_span::SourceMap;
use lilac_ty::Types;

fn lower_src(src: &str) -> (String, Diags) {
    let mut sources = SourceMap::new();
    let mut diags = Diags::new();
    let project = lilac_module::single_file("test.lilac", src, &mut sources);
    let (programs, _) = lilac_module::parse_all(&project, &mut diags, false);
    let res = lilac_resolve::resolve(&project, &programs, &mut diags);
    let mut types = Types::new();
    let checked = lilac_typeck::check(&res, &mut types, &mut diags, 64);
    assert!(!diags.has_errors(), "{diags:?}");
    let mut lir = lower(&checked.hir, &res, &types, "x86_64-unknown-none", 8);
    let text = lir.to_text();
    lilac_const::fold(&mut lir);
    let mut verify_diags = Diags::new();
    assert!(
        lilac_ir::verify(&lir, &mut verify_diags).is_ok() || text.contains("@compile_error"),
        "{verify_diags:?}\n{text}"
    );
    (text, diags)
}

#[test]
fn if_produces_branches() {
    let (text, _) = lower_src(
        "main: function () nothing {\n    conditional: @bit32 = 0x0\n    if 0x0 {\n        conditional = 0x1\n    } else {\n        conditional = 0x2\n    }\n}\n",
    );
    assert!(text.contains("branch.if"), "{text}");
}

#[test]
fn unsigned_intrinsics_map_to_unsigned_ops() {
    let (text, _) = lower_src(
        "main: function () nothing {\n    a: @bit32 = 0x8\n    b: @bit32 = 0x2\n    q: @bit32 = @i_div_u_32(a, b)\n    r: @bit32 = @i_mod_u_32(a, b)\n    if @i_less_than_u_32(a, b) {\n        q = 0x1\n    }\n    if @i_greater_than_or_equal_u_32(a, b) {\n        r = 0x1\n    }\n}\n",
    );
    for op in ["i.div.u", "i.mod.u", "i.cmp.lt.u", "i.cmp.ge.u"] {
        assert!(text.contains(op), "missing `{op}` in:\n{text}");
    }
}

#[test]
fn multi_return_writes_both_outs() {
    let (text, _) = lower_src(
        "get_multi: function () condition: @bit32, result: @bit32 {\n    condition = 0x1\n    result = 0x2\n}\nmain: function () nothing {\n    condition: @bit32, result: @bit32 = get_multi()\n}\n",
    );
    assert!(
        text.contains("call @get_multi (in) (out %condition, %result)"),
        "{text}"
    );
    assert!(text.contains("zero %condition"), "{text}");
}

#[test]
fn generic_object_instance_names() {
    let (text, _) = lower_src(
        "GenObject[T]: object {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\nmain: function () nothing {\n    variable: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    value: @bit32 = variable.get()\n}\n",
    );
    assert!(text.contains("#GenObject.1..bit32"), "{text}");
    assert!(text.contains("call @GenObject.1..bit32.get"), "{text}");
}

#[test]
fn method_own_generics_are_distinct() {
    let (text, _) = lower_src(
        "GenObject[T]: object {\n    value: T\n    get[U]: function (x: U) result: @bit32 {\n        result = 0x2\n    }\n}\n\ngen[T]: function () out: @bit32 {\n    out = T.get[@bit32](0x0)\n}\n\ngen2[T]: function () out: @bit32 {\n    out = T.get[@bit64](0x0)\n}\n\nmain: function () nothing {\n    a: @bit32 = gen[GenObject[@bit32]]()\n    b: @bit32 = gen2[GenObject[@bit32]]()\n}\n",
    );
    assert!(
        text.contains("call @GenObject.1..bit32.get.1..bit32"),
        "{text}"
    );
    assert!(
        text.contains("call @GenObject.1..bit32.get.1..bit64"),
        "{text}"
    );
}

#[test]
fn drop_hook_runs_before_marker() {
    let (text, _) = lower_src(
        "Object: object {\n    member: @bit32\n    @on_drop: function (from: ref Object) nothing {\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n}\n",
    );
    let call = text.find("call @Object.@on_drop").expect("on_drop call");
    let drop = text.find("drop %variable").expect("drop marker");
    assert!(call < drop, "{text}");
}

#[test]
fn no_drop_without_hook() {
    let (text, _) = lower_src(
        "Object: object {\n    member: @bit32\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n}\n",
    );
    assert!(!text.contains("drop %"), "{text}");
}

#[test]
fn field_drop_glue_runs_parent_then_fields() {
    let (text, _) = lower_src(
        "Inner: object {\n    v: @bit8\n    @on_drop: function (self) nothing {\n    }\n}\nOuter: object {\n    a: Inner\n    @on_drop: function (self) nothing {\n    }\n}\nmain: function () nothing {\n    o: Outer = Outer { a = Inner { v = 0x1 } }\n}\n",
    );
    let glue = text
        .split("function @.lilac.drop.Outer")
        .nth(1)
        .expect("drop glue for Outer");
    let parent = glue.find("call @Outer.@on_drop").expect("parent hook");
    let field = glue.find("call @Inner.@on_drop").expect("field hook");
    assert!(parent < field, "{text}");
}

#[test]
fn strings_share_one_const() {
    let (text, _) = lower_src(
        "main: function () nothing {\n    a: @byte_buffer = \"hi\"\n    b: @byte_buffer = \"hi\"\n}\n",
    );
    assert_eq!(text.matches("str = \"hi\"").count(), 1, "{text}");
}

#[test]
fn hex_byte_string_is_one_byte() {
    let (text, _) = lower_src("main: function () nothing {\n    a: @byte_buffer = \"\\xFF\"\n}\n");
    assert!(text.contains("str = \"\\xFF\""), "{text}");
}

#[test]
fn literals_lower_to_consts() {
    let (text, _) = lower_src(
        "main: function () nothing {\n    x: @bit8 = 5\n    y: @fbit64 = 3.14\n    z: @bit8 = 0x1\n}\n",
    );
    assert!(text.contains("#bit8 = const 5"), "{text}");
    assert!(text.contains("#fbit64 = const 3.14"), "{text}");
    assert!(text.contains("#bit8 = const 0x1"), "{text}");
}

#[test]
fn size_of_is_a_constant() {
    let (text, _) = lower_src(
        "Mixed: object {\n    a: @bit8\n    b: @bit32\n    c: @bit16\n}\nmain: function () nothing {\n    n: @bit_native = @size_of(Mixed)\n    m: @bit_native = @size_of(ref Mixed)\n}\n",
    );
    assert!(text.contains("#bit_native = const 0xc"), "{text}");
    assert!(text.contains("#bit_native = const 0x8"), "{text}");
}

#[test]
fn inline_functions_are_marked() {
    let (text, _) = lower_src(
        "twice: function (v: @bit32) r: @bit32 => @i_add_32(v, v)\nmain: function () nothing {\n    x: @bit32 = twice(0x2)\n}\n",
    );
    assert!(text.contains("inline function @twice"), "{text}");
    assert!(text.contains("call @twice"), "{text}");
}

#[test]
fn arguments_are_passed_as_fresh_copies() {
    let (text, _) = lower_src(
        "f: function (x: @bit32) nothing {\n    x = 0x5\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    f(a)\n}\n",
    );
    assert!(!text.contains("call @f (in %a)"), "{text}");
}
