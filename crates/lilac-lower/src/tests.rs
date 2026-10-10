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
    let options = lilac_typeck::Options {
        native_bits: 64,
        require_entry: false,
    };
    let checked = lilac_typeck::check(&res, &mut types, &mut diags, options);
    assert!(!diags.has_errors(), "{diags:?}");
    let lir = lower(
        &checked.hir,
        &res,
        &types,
        &sources,
        "x86_64-unknown-none",
        8,
    )
    .module;
    let text = lir.to_text();
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
        "main: function () nothing {\n    conditional: @bit32 = 0x0\n    flag: @bit8 = 0x0\n    if flag {\n        conditional = 0x1\n    } else {\n        conditional = 0x2\n    }\n}\n",
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
        text.contains("call @.root.get_multi (in) (out %condition, %result)"),
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
    assert!(text.contains("#bit8 = const 0x5"), "{text}");
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
    assert!(text.contains("inline function @.root.twice"), "{text}");
    assert!(text.contains("call @.root.twice"), "{text}");
}

#[test]
fn arguments_are_passed_as_fresh_copies() {
    let (text, _) = lower_src(
        "f: function (x: @bit32) nothing {\n    x = 0x5\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    f(a)\n}\n",
    );
    assert!(!text.contains("call @.root.f (in %a)"), "{text}");
}

/// Lowers `prelude` plus a `main` running `if <cond> { live() } else {
/// dead() }` (or the reverse when `taken` is false) and asserts which arm
/// reached LIR. `None` means the condition is not constant: both arms and the
/// branch stay.
fn assert_arm(prelude: &str, cond: &str, taken: Option<bool>) {
    let src = format!(
        "{prelude}\nthen_arm: function () nothing {{\n}}\nelse_arm: function () nothing {{\n}}\nmain: function () nothing {{\n    if {cond} {{\n        then_arm()\n    }} else {{\n        else_arm()\n    }}\n}}\n"
    );
    let (text, _) = lower_src(&src);
    let main = text.split("function @main").nth(1).expect("main");
    let has = |name: &str| main.contains(&format!("call @.root.{name}"));
    if let Some(taken) = taken {
        assert!(!main.contains("branch.if"), "{cond}:\n{main}");
        assert_eq!(has("then_arm"), taken, "{cond}:\n{main}");
        assert_eq!(has("else_arm"), !taken, "{cond}:\n{main}");
    } else {
        assert!(main.contains("branch.if"), "{cond}:\n{main}");
        assert!(has("then_arm") && has("else_arm"), "{cond}:\n{main}");
    }
}

const BOOL: &str = "Bool: object {\n    data: @bit8\n    from_int8: function (from: @bit8) Bool => Bool { data = from }\n    @conversion: function (from: Bool) to: @bit8 => from.data\n}\ntrue: Bool => Bool.from_int8(0x1)\nfalse: Bool => Bool.from_int8(0x0)\n";

#[test]
fn constant_literals_and_arithmetic() {
    assert_arm("", "0x0", Some(false));
    assert_arm("", "0x2", Some(true));
    assert_arm("", "@i_less_than_32(0x1, 0x2)", Some(true));
    assert_arm("", "@i_less_than_8(0xff, 0x0)", Some(true));
    assert_arm("", "@i_less_than_u_8(0xff, 0x0)", Some(false));
    assert_arm("", "@i_add_32(0x1, 0x2) == 0x3", Some(true));
    assert_arm("", "@i_add_8(0xff, 0x2) == 0x1", Some(true));
    assert_arm("", "@size_of(@bit32) == 0x4", Some(true));
}

#[test]
fn division_by_zero_is_not_constant() {
    assert_arm("", "@i_div_32(0x1, 0x0) == 0x0", None);
}

#[test]
fn inline_constants_fold_through_inline_calls() {
    assert_arm(BOOL, "true", Some(true));
    assert_arm(BOOL, "false", Some(false));
    assert_arm(BOOL, "Bool.from_int8(0x1)", Some(true));
}

#[test]
fn nested_inline_calls_bind_parameters() {
    let prelude = "add: function (a: @bit32, b: @bit32) r: @bit32 => @i_add_32(a, b)\ntwice: function (v: @bit32) r: @bit32 => add(v, v)\nlimit: @bit32 => twice(0x3)\n";
    assert_arm(prelude, "limit == 0x6", Some(true));
    assert_arm(prelude, "limit == 0x7", Some(false));
}

#[test]
fn locals_are_never_constant() {
    let (text, _) = lower_src(&format!(
        "{BOOL}\nboom: function () nothing {{\n}}\nmain: function () nothing {{\n    t: Bool = false\n    if t {{\n        boom()\n    }}\n}}\n"
    ));
    assert!(text.contains("branch.if"), "{text}");
    assert!(text.contains("call @.root.boom"), "{text}");
}

#[test]
fn recursive_and_block_bodied_calls_are_not_constant() {
    let recursive = "spin: function (v: @bit8) r: @bit8 => spin(v)\n";
    assert_arm(recursive, "spin(0x1)", None);
    let block = "one: function () r: @bit8 {\n    r = 0x1\n}\n";
    assert_arm(block, "one()", None);
}

#[test]
fn hooked_types_are_not_constant() {
    let drop = "Flag: object {\n    data: @bit8\n    @on_drop: function (self) nothing {\n    }\n    @conversion: function (from: Flag) to: @bit8 => from.data\n}\non: Flag => Flag { data = 0x1 }\n";
    assert_arm(drop, "on", None);
    let copy = "Flag: object {\n    data: @bit8\n    @on_copy: function (from: ref Flag) to: Flag {\n        to = Flag { data = from.data }\n    }\n    @conversion: function (from: Flag) to: @bit8 => from.data\n}\non: Flag => Flag { data = 0x1 }\n";
    assert_arm(copy, "on", None);
}

#[test]
fn value_if_and_logic_skip_the_dead_side() {
    let prelude = "one: function () r: @bit8 {\n    r = 0x1\n}\n";
    // The unevaluated side is not constant, yet the whole is.
    assert_arm(prelude, "if 0x1 then 0x0 else one()", Some(false));
    assert_arm(prelude, "0x0 and one()", Some(false));
    assert_arm(prelude, "0x1 or one()", Some(true));
    assert_arm(prelude, "0x1 and 0x0", Some(false));
    assert_arm(prelude, "0x1 and one()", None);
}

#[test]
fn value_match_in_an_inline_body() {
    let prelude = "Opt: enum {\n    None\n    Some: @bit8\n}\nget: function (o: Opt) r: @bit8 => match o {\n    .Some -> v: @bit8 => v\n} else 0x0\nkind: function (b: @bit8) r: @bit8 => match b {\n    0x1 => 0x0\n} else 0x1\none: function () r: @bit8 {\n    r = 0x1\n}\npick: function (b: @bit8) r: @bit8 => match b {\n    0x0 => one()\n} else 0x1\n";
    assert_arm(prelude, "get(Opt.Some(0x1))", Some(true));
    assert_arm(prelude, "get(Opt.None)", Some(false));
    assert_arm(prelude, "kind(0x7)", Some(true));
    assert_arm(prelude, "kind(0x1)", Some(false));
    assert_arm(prelude, "pick(0x5)", Some(true));
}

#[test]
fn constant_match_statement_lowers_one_arm() {
    let (text, _) = lower_src(
        "Opt: enum {\n    None\n    Some: @bit8\n}\nboom: function () nothing {\n}\nkeep: function (v: @bit8) nothing {\n}\nmain: function () nothing {\n    match Opt.Some(0x4) {\n        .None => boom()\n        .Some -> v: @bit8 => keep(v)\n    }\n}\n",
    );
    let main = text.split("function @main").nth(1).expect("main");
    assert!(!main.contains("call @.root.boom"), "{main}");
    assert!(main.contains("call @.root.keep"), "{main}");
    assert!(!main.contains("branch.if"), "{main}");
}

#[test]
fn constant_condition_drops_dead_compile_error() {
    let (text, _) = lower_src(
        "main: function () nothing {\n    if 0x0 {\n        @compile_error(\"dead\")\n    }\n}\n",
    );
    assert!(!text.contains("@compile_error"), "{text}");
}
