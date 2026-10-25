use super::*;
use lilac_error::Diags;
use lilac_span::SourceMap;
use lilac_ty::Types;

fn lower_src(src: &str) -> (String, Diags) {
    lower_src_on(src, lilac_target::Target::default())
}

/// Lowers `src` for `target`.
fn lower_src_on(src: &str, target: lilac_target::Target) -> (String, Diags) {
    let (lowered, diags) = lowered_on(src, target);
    (lowered.module.to_text(), diags)
}

/// Lowers `src` for `target`, keeping everything lowering collected. The
/// module verifies once the panic handler the driver injects is added.
fn lowered_on(src: &str, target: lilac_target::Target) -> (Lowered, Diags) {
    let mut sources = SourceMap::new();
    let mut diags = Diags::new();
    let project = lilac_module::single_file("test.lilac", src, &mut sources);
    let (programs, _) = lilac_module::parse_all(&project, &mut diags, false);
    let res = lilac_resolve::resolve(&project, &programs, &mut diags);
    let mut types = Types::new();
    let options = lilac_typeck::Options {
        target,
        require_entry: false,
        cancel: None,
    };
    let checked = lilac_typeck::check(&res, &mut types, &mut diags, options);
    assert!(!diags.has_errors(), "{diags:?}");
    let lowered = lower(&checked.hir, &res, &types, &sources, target, None);
    let text = lowered.module.to_text();
    let mut lir = lowered.module.clone();
    lir.funcs.extend(lilac_ir::build_runtime(&target));
    lir.funcs.extend(lilac_ir::hook_externs());
    lilac_ir::panic_runtime(lilac_ir::PanicMessage::of(&lowered.module)).add_to(&mut lir);
    let mut verify_diags = Diags::new();
    assert!(
        lilac_ir::verify(&lir, &mut verify_diags).is_ok() || text.contains("@compile_error"),
        "{verify_diags:?}\n{text}"
    );
    (lowered, diags)
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
        "GenObject[T]: structure {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\nmain: function () nothing {\n    variable: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    value: @bit32 = variable.get()\n}\n",
    );
    assert!(text.contains("#GenObject[m]"), "{text}");
    assert!(text.contains("call @GenObject[m].get"), "{text}");
}

#[test]
fn method_own_generics_are_distinct() {
    let (text, _) = lower_src(
        "GenObject[T]: structure {\n    value: T\n    get[U]: function (x: U) result: @bit32 {\n        result = 0x2\n    }\n}\n\ngen[T]: function () out: @bit32 {\n    out = T.get[@bit32](0x0)\n}\n\ngen2[T]: function () out: @bit32 {\n    out = T.get[@bit64](0x0)\n}\n\nmain: function () nothing {\n    a: @bit32 = gen[GenObject[@bit32]]()\n    b: @bit32 = gen2[GenObject[@bit32]]()\n}\n",
    );
    assert!(text.contains("call @GenObject[m].get[m]"), "{text}");
    assert!(text.contains("call @GenObject[m].get[y]"), "{text}");
}

#[test]
fn drop_hook_runs_before_marker() {
    let (text, _) = lower_src(
        "Object: structure {\n    member: @bit32\n    @on_drop: function (from: ref Object) nothing {\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n}\n",
    );
    let call = text.find("call @Object.@on_drop").expect("on_drop call");
    let drop = text.find("drop %variable").expect("drop marker");
    assert!(call < drop, "{text}");
}

#[test]
fn no_drop_without_hook() {
    let (text, _) = lower_src(
        "Object: structure {\n    member: @bit32\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n}\n",
    );
    assert!(!text.contains("drop %"), "{text}");
}

#[test]
fn field_drop_glue_runs_parent_then_fields() {
    let (text, _) = lower_src(
        "Inner: structure {\n    v: @bit8\n    @on_drop: function (self) nothing {\n    }\n}\nOuter: structure {\n    a: Inner\n    @on_drop: function (self) nothing {\n    }\n}\nmain: function () nothing {\n    o: Outer = Outer { a = Inner { v = 0x1 } }\n}\n",
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
        "Mixed: structure {\n    a: @bit8\n    b: @bit32\n    c: @bit16\n}\nmain: function () nothing {\n    n: @bit_native = @size_of(Mixed)\n    m: @bit_native = @size_of(ref Mixed)\n}\n",
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

const BOOL: &str = "Bool: structure {\n    data: @bit8\n    from_int8: function (from: @bit8) Bool => Bool { data = from }\n    @conversion: function (from: Bool) to: @bit8 => from.data\n}\ntrue: Bool => Bool.from_int8(0x1)\nfalse: Bool => Bool.from_int8(0x0)\n";

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
    assert_arm("", "@i_div_8(0x80, 0xff) == 0x80", None);
    assert_arm("", "@i_div_8(0x81, 0xff) == 0x7f", Some(true));
}

/// The function (by source name) and panic of each collected `E060`.
fn bad_panics(src: &str) -> Vec<(String, ConstantPanic)> {
    let (lowered, _) = lowered_on(src, lilac_target::Target::default());
    lowered
        .bad_panics
        .iter()
        .map(|bad| {
            let name = bad
                .func
                .as_ref()
                .map_or_else(|| "<global>".to_owned(), |func| lowered.names[func].clone());
            (name, bad.panic.clone())
        })
        .collect()
}

#[test]
fn constant_divisions_that_panic_are_collected() {
    let found = bad_panics(
        "div: function (a: @bit32, b: @bit32) r: @bit32 => @i_div_32(a, b)
twice: function (a: @bit32) r: @bit32 => div(a, 0x0)
pass: function (a: @bit32, b: @bit32) r: @bit32 => div(a, b)
zero: @bit32 => 0x0
by_zero: function () r: @bit32 {
    r = @i_div_u_32(0x1, 0x0)
}
runtime_dividend: function (a: @bit8) r: @bit8 {
    r = @i_mod_8(a, 0x0)
}
overflow: function () r: @bit64 {
    r = @i_mod_64(0x8000000000000000, 0xFFFFFFFFFFFFFFFF)
}
through_hook: function (a: @bit32) r: @bit32 {
    r = div(a, zero)
}
through_nested: function (a: @bit32) r: @bit32 {
    r = twice(a)
}
through_pass: function (a: @bit32) r: @bit32 {
    r = pass(a, 0x0)
}
fine: function (a: @bit32, b: @bit32) r: @bit32 {
    r = @i_div_32(a, 0xFFFFFFFF)
    r = @i_div_32(0x80000000, b)
    r = @i_div_32(0x80000000, 0x2)
    r = div(a, 0x3)
    if 0x0 {
        r = @i_div_u_32(a, 0x0)
    }
}
main: function () nothing {
}
",
    );
    let found: Vec<(&str, ConstantPanic)> = found
        .iter()
        .map(|(name, panic)| (name.as_str(), panic.clone()))
        .collect();
    assert_eq!(
        found,
        [
            // `twice`'s own body divides by zero; its callers are not
            // reported again (`through_nested`), but one whose constant
            // argument reaches a division is (`through_pass`).
            ("twice", ConstantPanic::ByZero),
            ("by_zero", ConstantPanic::ByZero),
            ("runtime_dividend", ConstantPanic::ByZero),
            ("overflow", ConstantPanic::Overflow),
            ("through_hook", ConstantPanic::ByZero),
            ("through_pass", ConstantPanic::ByZero),
        ]
    );
}

#[test]
fn a_global_initializer_that_divides_by_zero_is_e060() {
    let found = bad_panics("g: @bit32 = @i_div_u_32(0x1, 0x0)\nmain: function () nothing {\n}\n");
    assert_eq!(found, [("<global>".to_owned(), ConstantPanic::ByZero)]);
}

#[test]
fn a_constant_call_that_reaches_a_panic_is_e060_with_its_message() {
    let found = bad_panics(
        "pick: function (a: @bit32) r: @bit32 => if @i_less_than_u_32(a, 0x2) then @panic(\"too small\") else a
g: @bit32 = pick(0x1)
small: function () r: @bit32 {
    r = pick(0x0)
}
fine: function (a: @bit32) r: @bit32 {
    r = pick(0x5)
    r = pick(a)
}
main: function () nothing {
}
",
    );
    let message = ConstantPanic::Message("too small".to_owned());
    assert_eq!(
        found,
        [
            ("<global>".to_owned(), message.clone()),
            ("small".to_owned(), message),
        ]
    );
}

/// Each division gets only the tests its constant operands leave open.
#[test]
fn division_checks_skip_what_constants_rule_out() {
    let (text, _) = lower_src(
        "f: function (a: @bit32, b: @bit32) r: @bit32 {
    r = @i_div_32(a, b)
}
g: function (a: @bit32, b: @bit32) r: @bit32 {
    r = @i_div_u_32(a, b)
}
h: function (a: @bit32) r: @bit32 {
    r = @i_div_32(a, 0x7)
}
k: function (a: @bit32) r: @bit32 {
    r = @i_mod_32(a, 0xFFFFFFFF)
}
main: function () nothing {
}
",
    );
    let body = |name: &str| {
        let start = text.find(&format!("function @.root.{name}\n")).expect(name);
        let rest = &text[start..];
        rest[..rest.find("\n}").expect("end")].to_owned()
    };
    let panics = |name: &str| body(name).matches("call @.lilac.panic").count();
    // Signed: zero and `MIN / -1`; unsigned: zero only.
    assert_eq!(panics("f"), 2, "{}", body("f"));
    assert_eq!(panics("g"), 1, "{}", body("g"));
    // A nonzero constant divisor other than -1 needs no test.
    assert_eq!(panics("h"), 0, "{}", body("h"));
    // A constant -1 leaves only the overflow test.
    assert_eq!(panics("k"), 1, "{}", body("k"));
    assert!(body("k").contains("const 0x80000000"), "{}", body("k"));
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
    let drop = "Flag: structure {\n    data: @bit8\n    @on_drop: function (self) nothing {\n    }\n    @conversion: function (from: Flag) to: @bit8 => from.data\n}\non: Flag => Flag { data = 0x1 }\n";
    assert_arm(drop, "on", None);
    let copy = "Flag: structure {\n    data: @bit8\n    @on_copy: function (from: ref Flag) to: Flag {\n        to = Flag { data = from.data }\n    }\n    @conversion: function (from: Flag) to: @bit8 => from.data\n}\non: Flag => Flag { data = 0x1 }\n";
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
    let prelude = "Opt: either {\n    None\n    Some: @bit8\n}\nget: function (o: Opt) r: @bit8 => match o {\n    .Some -> v: @bit8 => v\n} else 0x0\nkind: function (b: @bit8) r: @bit8 => match b {\n    0x1 => 0x0\n} else 0x1\none: function () r: @bit8 {\n    r = 0x1\n}\npick: function (b: @bit8) r: @bit8 => match b {\n    0x0 => one()\n} else 0x1\n";
    assert_arm(prelude, "get(Opt.Some(0x1))", Some(true));
    assert_arm(prelude, "get(Opt.None)", Some(false));
    assert_arm(prelude, "kind(0x7)", Some(true));
    assert_arm(prelude, "kind(0x1)", Some(false));
    assert_arm(prelude, "pick(0x5)", Some(true));
}

#[test]
fn constant_match_statement_lowers_one_arm() {
    let (text, _) = lower_src(
        "Opt: either {\n    None\n    Some: @bit8\n}\nboom: function () nothing {\n}\nkeep: function (v: @bit8) nothing {\n}\nmain: function () nothing {\n    match Opt.Some(0x4) {\n        .None => boom()\n        .Some -> v: @bit8 => keep(v)\n    }\n}\n",
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

/// Layouts and `@size_of` follow the target's C ABI: on i386 System V an
/// 8-byte integer is aligned to 4, so `{ @bit8, @bit64 }` is 12 bytes.
#[test]
fn layouts_follow_the_target() {
    let src = "Pair: structure {\n    a: @bit8\n    b: @bit64\n}\n\nmain: function () nothing {\n    size: @bit_native = @size_of(Pair)\n    align: @bit_native = @align_of(Pair)\n}\n";
    let i686 = lilac_target::Target::parse("x86-unknown-linux-gnu").expect("triple");
    let (text, _) = lower_src_on(src, i686);
    assert!(
        text.contains("target = \"i686-unknown-linux-gnu\""),
        "{text}"
    );
    assert!(text.contains("type #Pair size 12 align 4 {"), "{text}");
    assert!(text.contains("#bit_native = const 0xc"), "{text}");
    assert!(text.contains("#bit_native = const 0x4"), "{text}");
    let (text, _) = lower_src(src);
    assert!(text.contains("type #Pair size 16 align 8 {"), "{text}");
    assert!(text.contains("#bit_native = const 0x10"), "{text}");
}

#[test]
fn not_and_not_equal_are_constant() {
    let prelude = "one: function () r: @bit8 {\n    r = 0x1\n}\nPair: structure {\n    a: @bit8\n    b: @bit8\n}\nOpt: either {\n    None\n    Some: @bit8\n}\np: Pair => Pair { a = 0x1, b = 0x2 }\nq: Pair => Pair { a = 0x1, b = 0x3 }\n";
    assert_arm(prelude, "not 0x0", Some(true));
    assert_arm(prelude, "not 0x1", Some(false));
    // A condition is true when it is nonzero.
    assert_arm(prelude, "not 0x2", Some(false));
    assert_arm(prelude, "not not 0x2", Some(true));
    assert_arm(prelude, "0x1 != 0x2", Some(true));
    assert_arm(prelude, "@i_add_32(0x1, 0x2) != 0x3", Some(false));
    assert_arm(prelude, "p != q", Some(true));
    assert_arm(prelude, "p != p", Some(false));
    assert_arm(prelude, "Opt.Some(0x1) != Opt.Some(0x1)", Some(false));
    assert_arm(prelude, "Opt.Some(0x1) != Opt.None", Some(true));
    assert_arm(prelude, "not one()", None);
    assert_arm(prelude, "one() != 0x1", None);
    assert_arm(prelude, "not 0x1 and one()", Some(false));
}

#[test]
fn a_guard_with_not_equal_rules_out_an_inline_division() {
    let found = bad_panics(
        "safe: function (a: @bit32, b: @bit32) r: @bit32 => if b != 0x0 then @i_div_32(a, b) else 0x0
unsafe: function (a: @bit32, b: @bit32) r: @bit32 => if not (b == 0x1) then @i_div_32(a, b) else 0x0
fine: function (a: @bit32) r: @bit32 {
    r = safe(a, 0x0)
}
bad: function (a: @bit32) r: @bit32 {
    r = unsafe(a, 0x0)
}
main: function () nothing {
}
",
    );
    assert_eq!(found, [("bad".to_owned(), ConstantPanic::ByZero)]);
}
