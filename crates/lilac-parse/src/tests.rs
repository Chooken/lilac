use super::*;
use lilac_ast::{BinOp, FuncBody, MatchPattern, PatternLiteral, Stmt, TypeShape};
use lilac_error::Diags;
use lilac_lex::Lexer;
use lilac_span::SourceMap;

fn parse(src: &str) -> (Program, Diags, SourceMap) {
    let mut sm = SourceMap::new();
    let file = sm.add_file("test.lilac", src);
    let (tokens, lex_errors) = Lexer::lex_all(src, file);
    let mut diags = Diags::new();
    for error in lex_errors {
        diags.emit(error);
    }
    let mut parser = Parser::new(tokens, file);
    let program = parser.parse_program(&mut diags).unwrap_or(Program {
        items: Vec::new(),
        span: Span::dummy(),
    });
    (program, diags, sm)
}

#[test]
fn hello_parses_cleanly() {
    let src = "main: function () nothing {\n    value: @bit32 = 0x1\n}\n";
    let (program, diags, _) = parse(src);
    assert!(!diags.has_errors());
    assert_eq!(program.items.len(), 1);
}

#[test]
fn missing_value_is_e001() {
    let (_, diags, _) = parse("main: function () nothing {\n    value: @bit32 =\n}\n");
    assert!(diags.has_errors());
    assert!(diags.diagnostics().iter().any(|d| d.code == "E001"));
}

#[test]
fn generics_and_calls_parse() {
    let src = "gen_function[T]: function (in: T) out: T {\n    out = in\n}\n\nmain: function () nothing {\n    out: @bit32 = gen_function[@bit32](0x1)\n}\n";
    let (program, diags, _) = parse(src);
    assert!(!diags.has_errors());
    assert_eq!(program.items.len(), 2);
}

#[test]
fn array_types_and_values_parse() {
    let src = "Grid: object {
rows: @array(@array(@bit16, 3), 0x2)
}

sum: function (values: ref @array(@bit32, 4)) nothing {
copy: @array(@bit32, 4) = @array(@bit32, 4) {}
}
";
    let (program, diags, _) = parse(src);
    assert!(!diags.has_errors(), "{diags:?}");
    let ItemKind::Object { members, .. } = &program.items[0].kind else {
        panic!("expected an object");
    };
    let ItemKind::Field { ty } = &members[0].kind else {
        panic!("expected a field");
    };
    let TypeShape::Array { element, len, .. } = &ty.shape else {
        panic!("expected an array, got {ty:?}");
    };
    assert_eq!(len, "0x2");
    let TypeShape::Array { element, len, .. } = &element.shape else {
        panic!("expected an inner array, got {element:?}");
    };
    assert_eq!(len, "3");
    assert_eq!(element.name(), Some("@bit16"));
    let ItemKind::Func(func) = &program.items[1].kind else {
        panic!("expected a function");
    };
    assert!(func.params[0].ty.is_ref);
    assert!(matches!(func.params[0].ty.shape, TypeShape::Array { .. }));
}

#[test]
fn array_value_needs_braces() {
    let (_, diags, _) = parse(
        "main: function () nothing {
a: @bit8 = @array(@bit8, 2)
}
",
    );
    assert!(diags.has_errors());
    let (_, diags, _) = parse(
        "main: function () nothing {
a: @array(@bit8) {}
}
",
    );
    assert!(diags.has_errors());
}

#[test]
fn recovery_reports_each_broken_item() {
    let src = "broken1: function ( nothing {}\nbroken2: object {\n";
    let (_, diags, _) = parse(src);
    assert!(diags.diagnostics().len() >= 2);
}

#[test]
fn item_error_recovery() {
    let src = "ok: function () nothing {\n}\nbad: function (\n";
    let mut sm = SourceMap::new();
    let file = sm.add_file("test.lilac", src);
    let (tokens, lex_errors) = Lexer::lex_all(src, file);
    let mut diags = Diags::new();
    for error in lex_errors {
        diags.emit(error);
    }
    let mut parser = Parser::new(tokens, file);
    let program = parser
        .parse_program(&mut diags)
        .expect("parse_program must recover rather than return Err");
    assert_eq!(program.items.len(), 2, "one good item plus one error item");
    assert!(
        matches!(program.items[0].kind, ItemKind::Func(_)),
        "the good item survives: {:?}",
        program.items[0].kind
    );
    assert!(
        matches!(program.items[1].kind, ItemKind::Error),
        "the broken item recovers into `ItemKind::Error`: {:?}",
        program.items[1].kind
    );
    assert!(parser.had_errors(), "a failed construct is recorded");
    let parse_diag = diags
        .diagnostics()
        .iter()
        .find(|d| d.code == "E001")
        .expect("the broken item still reports E001");
    assert_eq!(
        &*parse_diag.msg, "unterminated parameter list; add a closing `)`",
        "same message as today"
    );
}

#[test]
fn object_field_parses() {
    let src = "Object: object {\n    member: @bit8\n}\n";
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    assert_eq!(program.items.len(), 1);
    let ItemKind::Object { members, .. } = &program.items[0].kind else {
        panic!("expected object, got {:?}", program.items[0].kind);
    };
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].name, "member");
    let ItemKind::Field { ty } = &members[0].kind else {
        panic!("expected field, got {:?}", members[0].kind);
    };
    assert_eq!(ty.head(), "@bit8");
    assert!(!ty.is_ref);
}

fn sole_let_type(src: &str) -> (Type, SourceMap) {
    let (program, diags, sm) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let Stmt::Let { ty, .. } = &block.stmts[0] else {
        panic!("expected let, got {:?}", block.stmts[0]);
    };
    ((**ty).clone(), sm)
}

#[test]
fn type_name_span() {
    let cases = [
        (
            "main: function () nothing {\n    x: @bit32 = 0x1\n}\n",
            "@bit32",
        ),
        (
            "main: function () nothing {\n    x: ref @bit32 = y\n}\n",
            "@bit32",
        ),
        (
            "main: function () nothing {\n    x: GenObj[@bit32] = y\n}\n",
            "GenObj",
        ),
    ];
    for (src, name) in cases {
        let (ty, sm) = sole_let_type(src);
        assert_eq!(ty.head(), name, "type name in {src:?}");
        let lo = u32::try_from(src.find(name).expect("name present in source"))
            .expect("offset fits in u32");
        let hi = lo + u32::try_from(name.len()).expect("name length fits in u32");
        assert_eq!(ty.name_span.lo.0, lo, "lo for {name}");
        assert_eq!(ty.name_span.hi.0, hi, "hi for {name}");
        assert_eq!(sm.span_text(ty.name_span), Some(name), "text for {name}");
    }
}

#[test]
fn tag_literal_parses() {
    let src = "main: function () nothing {\n    number: Number = .One\n}\n";
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    assert_eq!(program.items.len(), 1);
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let Stmt::Let { value, .. } = &block.stmts[0] else {
        panic!("expected let, got {:?}", block.stmts[0]);
    };
    let Expr::Tag(tag, _) = value else {
        panic!("expected tag literal, got {value:?}");
    };
    assert_eq!(tag, "One");
}

#[test]
fn bare_top_level_decl_is_a_zeroed_global() {
    let (file, diags, _) = parse("x: @bit8\n");
    assert!(!diags.has_errors(), "{diags:?}");
    assert!(matches!(
        file.items[0].kind,
        ItemKind::Global { value: None, .. }
    ));
}

#[test]
fn and_or_parse_as_binop() {
    let src =
        "main: function () nothing {\n    x: @bit8 = 0x0 and 0x1\n    y: @bit8 = 0x1 or x\n}\n";
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let Stmt::Let {
        value: and_value, ..
    } = &block.stmts[0]
    else {
        panic!("expected let, got {:?}", block.stmts[0]);
    };
    let Expr::BinOp { op: and_op, .. } = and_value else {
        panic!("expected binop, got {and_value:?}");
    };
    assert_eq!(*and_op, BinOp::And);
    let Stmt::Let {
        value: or_value, ..
    } = &block.stmts[1]
    else {
        panic!("expected let, got {:?}", block.stmts[1]);
    };
    let Expr::BinOp { op: or_op, .. } = or_value else {
        panic!("expected binop, got {or_value:?}");
    };
    assert_eq!(*or_op, BinOp::Or);
}

#[test]
fn match_byte_arms_parse() {
    let src = "main: function () nothing {\n    match 0x0 {\n        0x0 => {}\n        0x1 => {}\n    }\n}\n";
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let Stmt::Match { arms, .. } = &block.stmts[0] else {
        panic!("expected match, got {:?}", block.stmts[0]);
    };
    assert_eq!(arms.len(), 2);
    let MatchPattern::Literal {
        literal: PatternLiteral::Number(first),
        ..
    } = &arms[0].pattern
    else {
        panic!("expected const pattern, got {:?}", arms[0].pattern);
    };
    assert_eq!(first, "0x0");
}

#[test]
fn multilet_parses() {
    let src =
        "main: function () nothing {\n    condition: @bit32, result: @bit32 = get_multi()\n}\n";
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let Stmt::MultiLet { bindings, .. } = &block.stmts[0] else {
        panic!("expected multilet, got {:?}", block.stmts[0]);
    };
    assert_eq!(bindings.len(), 2);
    assert_eq!(bindings[0].name, "condition");
    assert_eq!(bindings[1].name, "result");
}

#[test]
fn declaration_shorthands_desugar() {
    let stmts = main_stmts(
        "main: function () nothing {\n    a: Object {\n        member = 0x1\n    }\n    b: Box[@bit8] { value = 0x1 }\n    c: Object.create(0x1, 0x2)\n    d: Box[@bit8].create(0x1)\n    e: A.Pair.make[@bit8](0x1)\n    f: Object(0x1)\n}\n",
    );
    let Stmt::Let { value, .. } = &stmts[0] else {
        panic!("{:?}", stmts[0]);
    };
    assert!(
        matches!(value, Expr::StructLit { ty, fields, .. } if ty.head() == "Object" && fields.len() == 1)
    );
    let Stmt::Let { value, .. } = &stmts[1] else {
        panic!("{:?}", stmts[1]);
    };
    assert!(matches!(value, Expr::StructLit { ty, .. } if ty.args().len() == 1));
    // The segment before `(` is the function; the rest is the type.
    let Stmt::Let { ty, value, .. } = &stmts[2] else {
        panic!("{:?}", stmts[2]);
    };
    assert_eq!(ty.head(), "Object");
    assert!(
        matches!(value, Expr::Construct { function, args, .. } if function == "create" && args.len() == 2),
        "{value:?}"
    );
    let Stmt::Let { ty, value, .. } = &stmts[3] else {
        panic!("{:?}", stmts[3]);
    };
    assert_eq!((ty.head(), ty.args().len()), ("Box", 1));
    assert!(
        matches!(value, Expr::Construct { function, generic_args, .. } if function == "create" && generic_args.is_empty()),
        "{value:?}"
    );
    let Stmt::Let { ty, value, .. } = &stmts[4] else {
        panic!("{:?}", stmts[4]);
    };
    assert_eq!((ty.head(), ty.args().len()), ("A.Pair", 0));
    assert!(
        matches!(value, Expr::Construct { function, generic_args, .. } if function == "make" && generic_args.len() == 1),
        "{value:?}"
    );
    // `Type(args)` stays a call of the type, which typeck rejects.
    let Stmt::Let { value, .. } = &stmts[5] else {
        panic!("{:?}", stmts[5]);
    };
    assert!(
        matches!(value, Expr::Call { callee, .. } if matches!(callee.as_ref(), Expr::Ident(name, _) if name == "Object"))
    );
}

#[test]
fn generic_type_in_value_position() {
    let stmts = main_stmts(
        "main: function () nothing {\n    a: Box[@bit8] = Box[@bit8] { value = 0x1 }\n    b: Box[@bit8] = Box[@bit8].make(0x1)\n    c: ref @bit8 = CAllocator.create[@bit8]()\n}\n",
    );
    let Stmt::Let { value, .. } = &stmts[0] else {
        panic!("{:?}", stmts[0]);
    };
    assert!(
        matches!(value, Expr::StructLit { ty, .. } if ty.head() == "Box" && ty.args().len() == 1)
    );
    let Stmt::Let { value, .. } = &stmts[1] else {
        panic!("{:?}", stmts[1]);
    };
    let Expr::Call { callee, .. } = value else {
        panic!("{value:?}");
    };
    let Expr::Member { obj, field, .. } = callee.as_ref() else {
        panic!("{callee:?}");
    };
    assert_eq!(field, "make");
    assert!(matches!(obj.as_ref(), Expr::TypeApp(ty) if ty.head() == "Box"));
    let Stmt::Let { value, .. } = &stmts[2] else {
        panic!("{:?}", stmts[2]);
    };
    assert!(matches!(value, Expr::Call { generic_args, .. } if generic_args.len() == 1));
}

#[test]
fn continue_is_a_statement() {
    let stmts =
        main_stmts("main: function () nothing {\n    loop if x { continue } else continue\n}\n");
    let Stmt::Loop { body, .. } = &stmts[0] else {
        panic!("{:?}", stmts[0]);
    };
    let Stmt::If {
        then,
        els: Some(els),
        ..
    } = body.as_ref()
    else {
        panic!("{body:?}");
    };
    assert!(matches!(then.stmts[..], [Stmt::Continue { .. }]));
    assert!(matches!(els.as_ref(), Stmt::Continue { .. }));
}

#[test]
fn extern_link_name_and_variadic() {
    let (program, diags, _) = parse(
        "open: extern \"open\" function (path: ref @bit8, flags: @bit32, ...) fd: @bit32\nwrite: extern function (fd: @bit32) nothing\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
    let ItemKind::Func(open) = &program.items[0].kind else {
        panic!("{:?}", program.items[0].kind);
    };
    assert_eq!(open.link_name.as_deref(), Some("open"));
    assert!(open.variadic);
    assert_eq!(open.params.len(), 2);
    let ItemKind::Func(write) = &program.items[1].kind else {
        panic!("{:?}", program.items[1].kind);
    };
    assert_eq!(write.link_name, None);
    assert!(!write.variadic);
}

#[test]
fn ellipsis_needs_an_extern_and_the_last_slot() {
    let (_, diags, _) = parse("f: function (a: @bit32, ...) nothing {}\n");
    assert!(diags.has_errors());
    let (_, diags, _) = parse("f: extern function (..., a: @bit32) nothing\n");
    assert!(diags.has_errors());
}

#[test]
fn builtin_type_names_take_dotted_segments() {
    let stmts =
        main_stmts("main: function () nothing {\n    os: @builtin.Os = @builtin.Target.os\n}\n");
    let Stmt::Let { ty, value, .. } = &stmts[0] else {
        panic!("{:?}", stmts[0]);
    };
    assert_eq!(ty.head(), "@builtin.Os");
    assert!(matches!(value, Expr::Member { field, .. } if field == "os"));
}

#[test]
fn type_paths_and_path_literals() {
    let stmts = main_stmts(
        "main: function () nothing {
a: A.B.Box[@bit8] = A.B.Box[@bit8] {}
b: Outer.Inner = Outer.Inner { x = 0x1 }
c: @bit8 = A.Box[@bit8].make()
}
",
    );
    let Stmt::Let { ty, value, .. } = &stmts[0] else {
        panic!("{:?}", stmts[0]);
    };
    assert_eq!(ty.head(), "A.B.Box");
    assert_eq!(ty.args().len(), 1);
    assert!(
        matches!(value, Expr::StructLit { ty, .. } if ty.head() == "A.B.Box" && ty.args().len() == 1),
        "{value:?}"
    );
    let Stmt::Let { ty, value, .. } = &stmts[1] else {
        panic!("{:?}", stmts[1]);
    };
    assert_eq!(ty.head(), "Outer.Inner");
    assert!(
        matches!(value, Expr::StructLit { ty, .. } if ty.head() == "Outer.Inner"),
        "{value:?}"
    );
    let Stmt::Let { value, .. } = &stmts[2] else {
        panic!("{:?}", stmts[2]);
    };
    let Expr::Call { callee, .. } = value else {
        panic!("{value:?}");
    };
    assert!(
        matches!(callee.as_ref(), Expr::Member { obj, .. } if matches!(obj.as_ref(), Expr::TypeApp(ty) if ty.head() == "A.Box")),
        "{callee:?}"
    );
}

#[test]
fn path_patterns_split_off_the_tag() {
    let stmts = main_stmts(
        "main: function () nothing {
match m {
    A.Mode.On -> v: Outer.Inner => {}
    .Off => {}
    Opt[@bit8].Some => {}
}
}
",
    );
    let Stmt::Match { arms, .. } = &stmts[0] else {
        panic!("{:?}", stmts[0]);
    };
    assert!(
        matches!(&arms[0].pattern, MatchPattern::Tag { qualifier: Some(q), name , .. } if q.head() == "A.Mode" && name == "On"),
        "{:?}",
        arms[0].pattern
    );
    assert!(
        matches!(&arms[1].pattern, MatchPattern::Tag { qualifier: None, name , .. } if name == "Off"),
        "{:?}",
        arms[1].pattern
    );
    assert!(
        matches!(&arms[2].pattern, MatchPattern::Tag { qualifier: Some(q), name , .. } if q.head() == "Opt" && name == "Some"),
        "{:?}",
        arms[2].pattern
    );
}

#[test]
fn enum_bodies_parse_nested_types() {
    // Parsed so the resolver can reject them (`E036`).
    let (program, diags, _) = parse(
        "Status: enum {
Idle
Busy: Job
Job: object {
    id: @bit32
}
Kind: enum { A, B }
}
",
    );
    assert!(!diags.has_errors(), "{diags:?}");
    let ItemKind::Enum {
        variants, members, ..
    } = &program.items[0].kind
    else {
        panic!("{:?}", program.items[0]);
    };
    assert_eq!(variants.len(), 2);
    let names: Vec<&str> = members.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, ["Job", "Kind"]);
}

#[test]
fn loop_and_else_take_statements() {
    let stmts = main_stmts(
        "main: function () nothing {\n    loop if x { x = 0x0 } else break\n    if a { } else if b { } else { }\n    loop { }\n    { y: @bit8 = 0x1 }\n}\n",
    );
    let Stmt::Loop { body, .. } = &stmts[0] else {
        panic!("{:?}", stmts[0]);
    };
    let Stmt::If { els: Some(els), .. } = body.as_ref() else {
        panic!("{body:?}");
    };
    assert!(matches!(els.as_ref(), Stmt::Break { .. }));
    let Stmt::If { els: Some(els), .. } = &stmts[1] else {
        panic!("{:?}", stmts[1]);
    };
    assert!(matches!(els.as_ref(), Stmt::If { els: Some(_), .. }));
    assert!(
        matches!(&stmts[2], Stmt::Loop { body, .. } if matches!(body.as_ref(), Stmt::Block(_)))
    );
    assert!(matches!(&stmts[3], Stmt::Block(block) if block.stmts.len() == 1));
}

#[test]
fn if_value_uses_then() {
    let stmts = main_stmts("main: function () nothing {\n    v: @bit8 = if c then -x else .A\n}\n");
    let Stmt::Let { value, .. } = &stmts[0] else {
        panic!("{:?}", stmts[0]);
    };
    let Expr::IfValue { then, els, .. } = value else {
        panic!("{value:?}");
    };
    assert!(matches!(then.as_ref(), Expr::Call { .. }));
    assert!(matches!(els.as_ref(), Expr::Tag(name, _) if name == "A"));
}

#[test]
fn qualified_match_patterns() {
    let stmts = main_stmts(
        "main: function () nothing {\n    match v {\n        .A => return\n        Opt.Some -> x: @bit8 => return\n        Opt[@bit8].None => return\n    }\n}\n",
    );
    let Stmt::Match { arms, .. } = &stmts[0] else {
        panic!("{:?}", stmts[0]);
    };
    assert!(
        matches!(&arms[0].pattern, MatchPattern::Tag { qualifier: None, name , .. } if name == "A")
    );
    assert!(
        matches!(&arms[1].pattern, MatchPattern::Tag { qualifier: Some(q), name , .. } if q.head() == "Opt" && name == "Some")
    );
    assert!(
        matches!(&arms[2].pattern, MatchPattern::Tag { qualifier: Some(q), .. } if q.args().len() == 1)
    );
}

#[test]
fn enum_members_parse() {
    let (program, diags, _) = parse(
        "Opt[T]: enum {\n    Some: T\n    None\n    Callback: function (x: @bit8) nothing\n    get: function (self) v: T {\n    }\n    @on_drop: function (self) nothing {}\n    map[U]: function (self) nothing {}\n    private {\n        helper: function () nothing => 0x0\n    }\n}\n",
    );
    assert!(!diags.has_errors(), "{:?}", diags.diagnostics());
    let ItemKind::Enum {
        variants, members, ..
    } = &program.items[0].kind
    else {
        panic!("{:?}", program.items[0].kind);
    };
    let names: Vec<_> = variants.iter().map(|v| v.name.as_str()).collect();
    assert_eq!(names, ["Some", "None", "Callback"]);
    let names: Vec<_> = members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["get", "@on_drop", "map", ""]);
}

#[test]
fn hex_escape_is_one_byte() {
    let stmts = main_stmts("main: function () nothing {\n    s: @byte_buffer = \"\\xFFa\"\n}\n");
    let Stmt::Let { value, .. } = &stmts[0] else {
        panic!("{:?}", stmts[0]);
    };
    assert!(matches!(value, Expr::Str(bytes, _) if bytes == &[0xFF, b'a']));
}

#[test]
fn relative_using_parses() {
    let (program, diags, _) = parse("using *.Heap.Pool\n");
    assert!(!diags.has_errors(), "{:?}", diags.diagnostics());
    assert!(matches!(
        &program.items[0].kind,
        ItemKind::Using { path, relative: true, alias: None, segments }
            if path == "Heap.Pool"
                && segments.iter().map(|s| (s.lo.0, s.hi.0)).eq([(8, 12), (13, 17)])
    ));
}

#[test]
fn using_alias_parses() {
    let (program, diags, _) = parse("using Standard.Math as Math\nusing *.Heap as H\n");
    assert!(!diags.has_errors(), "{:?}", diags.diagnostics());
    assert!(matches!(
        &program.items[0].kind,
        ItemKind::Using { path, relative: false, alias: Some((alias, _)), .. }
            if path == "Standard.Math" && alias == "Math"
    ));
    assert!(matches!(
        &program.items[1].kind,
        ItemKind::Using { path, relative: true, alias: Some((alias, _)), .. }
            if path == "Heap" && alias == "H"
    ));
}

#[test]
fn structlit_equals_form_parses() {
    let src = "main: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n}\n";
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let Stmt::Let { value, .. } = &block.stmts[0] else {
        panic!("expected let, got {:?}", block.stmts[0]);
    };
    let Expr::StructLit { ty, fields, .. } = value else {
        panic!("expected struct literal, got {value:?}");
    };
    assert_eq!(ty.head(), "Object");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "member");
}

#[test]
fn match_value_without_else_is_e001() {
    let src = "main: function () nothing {\n    variable: @bit32 = match 0x0 {\n        0x1 => 0x1\n    }\n}\n";
    let (_, diags, _) = parse(src);
    assert!(diags.has_errors());
    let messages: Vec<_> = diags.diagnostics().iter().map(|d| d.msg.clone()).collect();
    assert!(
        diags.diagnostics().iter().any(|d| d.code == "E001"),
        "expected E001, got {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.contains("else")),
        "expected else-mentioning message, got {messages:?}"
    );
}

#[test]
fn if_value_without_else_is_e001() {
    let src = "main: function () nothing {\n    conditional: @bit32 = if 0x0 0x0\n}\n";
    let (_, diags, _) = parse(src);
    assert!(diags.has_errors());
    assert!(diags.diagnostics().iter().any(|d| d.code == "E001"));
}

#[test]
fn func_type_position_parses() {
    let src = "main: function () nothing {\n    add: function (@bit32) @bit32 = addOne\n}\n";
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let Stmt::Let { ty, .. } = &block.stmts[0] else {
        panic!("expected let, got {:?}", block.stmts[0]);
    };
    let TypeShape::Function(func) = &ty.shape else {
        panic!("expected function type, got {ty:?}");
    };
    assert_eq!(func.params.len(), 1);
    assert_eq!(func.params[0].name, "");
    assert_eq!(func.params[0].ty.head(), "@bit32");
    assert_eq!(func.returns.len(), 1);
}

#[test]
fn enum_payloads_parse() {
    let src = "OptionalBit32: enum {\n    Some: @bit32\n    None\n}\n";
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Enum { variants, .. } = &program.items[0].kind else {
        panic!("expected enum, got {:?}", program.items[0].kind);
    };
    assert_eq!(variants.len(), 2);
    assert!(variants[0].payload.is_some());
    assert!(variants[1].payload.is_none());
}

#[test]
fn lowercase_ident_brace_is_not_structlit() {
    // `if x {` and `match number {` must not parse the trailing block
    // as a struct literal (struct literals name a `Type`).
    let src = "main: function () nothing {\n    x: @bit8 = 0x1\n    if x {\n    }\n}\n";
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    assert!(
        matches!(block.stmts[1], Stmt::If { .. }),
        "expected if statement, got {:?}",
        block.stmts[1]
    );
}

fn first_stmt(src: &str) -> (Option<Stmt>, Diags) {
    let (program, diags, _) = parse(src);
    let stmt = program.items.iter().find_map(|item| match &item.kind {
        ItemKind::Func(decl) => decl.body.as_ref().and_then(|body| match body {
            FuncBody::Block(block) => block.stmts.first().cloned(),
            FuncBody::Inline(_) => None,
        }),
        _ => None,
    });
    (stmt, diags)
}

#[test]
fn if_head_rejects_struct_literal() {
    // `if Foo {` is a condition plus a block, never a struct literal.
    let (stmt, diags) = first_stmt("main: function () nothing {\n    if Foo {\n    }\n}\n");
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let Some(Stmt::If { cond, .. }) = stmt else {
        panic!("expected if statement, got {stmt:?}");
    };
    assert!(
        matches!(&cond, Expr::Ident(name, _) if name == "Foo"),
        "got {cond:?}"
    );
}

#[test]
fn parenthesized_struct_literal_in_head() {
    // Parentheses reset the restriction: a struct literal works in `if`.
    let (stmt, diags) =
        first_stmt("main: function () nothing {\n    if (Foo { member = 0x1 }) {\n    }\n}\n");
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let Some(Stmt::If { cond, .. }) = stmt else {
        panic!("expected if statement, got {stmt:?}");
    };
    assert!(
        matches!(cond, Expr::StructLit { ref ty, .. } if ty.head() == "Foo"),
        "got {cond:?}"
    );
}

#[test]
fn lowercase_struct_literal_in_let() {
    // Case never decides: any identifier opens a struct literal outside
    // head position (the checker rejects unknown type names later).
    let (stmt, diags) = first_stmt(
        "Object: object {\n    member: @bit8\n}\nmain: function () nothing {\n    v: Object = foo { member = 0x1 }\n}\n",
    );
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let Some(Stmt::Let { value, .. }) = stmt else {
        panic!("expected let statement, got {stmt:?}");
    };
    assert!(
        matches!(value, Expr::StructLit { ref ty, .. } if ty.head() == "foo"),
        "got {value:?}"
    );
}

#[test]
fn if_head_brace_always_opens_the_block() {
    // `if x { y = 0x1 }` is a condition and a block holding an assignment;
    // a struct literal in a head needs parentheses.
    let (stmt, diags) = first_stmt("main: function () nothing {\n    if x { y = 0x1 }\n}\n");
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let Some(Stmt::If { cond, then, .. }) = stmt else {
        panic!("expected if statement, got {stmt:?}");
    };
    assert!(matches!(&cond, Expr::Ident(name, _) if name == "x"));
    assert!(matches!(then.stmts[0], Stmt::Assign { .. }));
}

#[test]
fn match_scrutinee_rejects_struct_literal() {
    // `match Foo {` inspects `Foo`; the `{` opens the arm block.
    let (stmt, diags) = first_stmt(
        "main: function () nothing {\n    match Foo {\n        .One {\n        }\n    }\n}\n",
    );
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let Some(Stmt::Match { scrutinee, .. }) = stmt else {
        panic!("expected match statement, got {stmt:?}");
    };
    assert!(
        matches!(&scrutinee, Expr::Ident(name, _) if name == "Foo"),
        "got {scrutinee:?}"
    );
}

#[test]
fn private_block_parses_items() {
    let (program, diags, _) = parse("private {\n    helper: function () nothing {}\n}\n");
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Private { items } = &program.items[0].kind else {
        panic!("expected private block, got {:?}", program.items[0].kind);
    };
    assert_eq!(items.len(), 1);
    assert!(matches!(items[0].kind, ItemKind::Func(_)));
}

#[test]
fn private_block_inside_object_parses() {
    let src = "Object: object {\n    private {\n        helper: function () nothing {}\n        secret: @bit8\n    }\n}\n";
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Object { members, .. } = &program.items[0].kind else {
        panic!("expected object, got {:?}", program.items[0].kind);
    };
    let ItemKind::Private { items } = &members[0].kind else {
        panic!("expected private block, got {:?}", members[0].kind);
    };
    assert_eq!(items.len(), 2);
}

#[test]
fn private_without_brace_is_e001() {
    let (_, diags, _) = parse("private helper: function () nothing {}\n");
    assert!(diags.has_errors());
    assert!(diags.diagnostics().iter().any(|d| d.code == "E001"));
}

#[test]
fn union_keyword_is_gone() {
    // Hard removal: `union` lexes as an identifier, so a union
    // declaration fails with a generic parse error.
    let (_, diags) = first_stmt(
        "Maybe: union {\n    Some: @bit8\n    None\n}\nmain: function () nothing {\n}\n",
    );
    assert!(diags.has_errors());
    assert!(
        diags.diagnostics().iter().any(|d| d.code == "E001"),
        "{:?}",
        diags.diagnostics()
    );
}

/// The statements of `main`'s block body, asserting a clean parse.
fn main_stmts(src: &str) -> Vec<Stmt> {
    let (program, diags, _) = parse(src);
    assert!(
        !diags.has_errors(),
        "unexpected errors: {:?}",
        diags.diagnostics()
    );
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    block.stmts.clone()
}

#[test]
fn as_cast_parses_postfix_left_associative() {
    let src = "main: function () nothing {\n\
        a: @bit8 = x as T\n\
        b: @bit8 = o.f as T\n\
        c: @bit8 = p + q as T\n\
        d: @bit8 = x as A as B\n\
        e: ref @bit8 = x as ref @bit8\n\
        g: @bit8 = f(x) as T\n\
        h: @bit8 = -x as T\n\
    }\n";
    let stmts = main_stmts(src);

    // `x as T` -> `Cast(Ident(x), T)`.
    let Stmt::Let { value, .. } = &stmts[0] else {
        panic!("expected let, got {:?}", stmts[0]);
    };
    let Expr::Cast(inner, ty) = value else {
        panic!("expected cast, got {value:?}");
    };
    assert!(matches!(**inner, Expr::Ident(ref name, _) if name == "x"));
    assert_eq!(ty.head(), "T");
    assert!(!ty.is_ref);

    // `o.f as T` -> `Cast(Member(o, f), T)`.
    let Stmt::Let { value, .. } = &stmts[1] else {
        panic!("expected let, got {:?}", stmts[1]);
    };
    let Expr::Cast(inner, ty) = value else {
        panic!("expected cast, got {value:?}");
    };
    let Expr::Member { obj, field, .. } = &**inner else {
        panic!("expected member, got {inner:?}");
    };
    assert!(matches!(**obj, Expr::Ident(ref name, _) if name == "o"));
    assert_eq!(field, "f");
    assert_eq!(ty.head(), "T");

    // `p + q as T` -> `Add(p, Cast(q, T))`: `as` is postfix and binds
    // tighter than `+`.
    let Stmt::Let { value, .. } = &stmts[2] else {
        panic!("expected let, got {:?}", stmts[2]);
    };
    let Expr::BinOp { lhs, op, rhs, .. } = value else {
        panic!("expected binop, got {value:?}");
    };
    assert_eq!(*op, BinOp::Add);
    assert!(matches!(**lhs, Expr::Ident(ref name, _) if name == "p"));
    let Expr::Cast(inner, ty) = &**rhs else {
        panic!("expected cast on the right, got {rhs:?}");
    };
    assert!(matches!(**inner, Expr::Ident(ref name, _) if name == "q"));
    assert_eq!(ty.head(), "T");

    // `x as A as B` -> `Cast(Cast(x, A), B)` (left-associative).
    let Stmt::Let { value, .. } = &stmts[3] else {
        panic!("expected let, got {:?}", stmts[3]);
    };
    let Expr::Cast(outer_inner, outer_ty) = value else {
        panic!("expected cast, got {value:?}");
    };
    assert_eq!(outer_ty.head(), "B");
    let Expr::Cast(inner_inner, inner_ty) = &**outer_inner else {
        panic!("expected nested cast, got {outer_inner:?}");
    };
    assert_eq!(inner_ty.head(), "A");
    assert!(matches!(**inner_inner, Expr::Ident(ref name, _) if name == "x"));

    // `x as ref @bit8` parses a `ref` target type.
    let Stmt::Let { value, .. } = &stmts[4] else {
        panic!("expected let, got {:?}", stmts[4]);
    };
    let Expr::Cast(inner, ty) = value else {
        panic!("expected cast, got {value:?}");
    };
    assert!(ty.is_ref);
    assert_eq!(ty.head(), "@bit8");
    assert!(matches!(**inner, Expr::Ident(ref name, _) if name == "x"));

    // `f(x) as T` -> `Cast(Call(f, [x]), T)`.
    let Stmt::Let { value, .. } = &stmts[5] else {
        panic!("expected let, got {:?}", stmts[5]);
    };
    let Expr::Cast(inner, ty) = value else {
        panic!("expected cast, got {value:?}");
    };
    let Expr::Call { callee, args, .. } = &**inner else {
        panic!("expected call, got {inner:?}");
    };
    assert!(matches!(**callee, Expr::Ident(ref name, _) if name == "f"));
    assert_eq!(args.len(), 1);
    assert!(matches!(args[0], Expr::Ident(ref name, _) if name == "x"));
    assert_eq!(ty.head(), "T");

    // `-x as T` -> `Cast(@negate(x), T)`: `as` is looser than unary minus,
    // so the cast applies to the negated value.
    let Stmt::Let { value, .. } = &stmts[6] else {
        panic!("expected let, got {:?}", stmts[6]);
    };
    let Expr::Cast(inner, ty) = value else {
        panic!("expected cast, got {value:?}");
    };
    assert_eq!(ty.head(), "T");
    let Expr::Call { callee, args, .. } = &**inner else {
        panic!("expected negate call, got {inner:?}");
    };
    assert!(matches!(**callee, Expr::Ident(ref name, _) if name == "@negate"));
    assert_eq!(args.len(), 1);
    assert!(matches!(args[0], Expr::Ident(ref name, _) if name == "x"));
}

/// Parses `src` with [`Parser::parse_program`] directly, returning the
/// program, the parser's failure signal, and the diagnostics.
fn parse_with_status(src: &str) -> (Program, Diags, bool, SourceMap) {
    let mut sm = SourceMap::new();
    let file = sm.add_file("test.lilac", src);
    let (tokens, lex_errors) = Lexer::lex_all(src, file);
    let mut diags = Diags::new();
    for error in lex_errors {
        diags.emit(error);
    }
    let mut parser = Parser::new(tokens, file);
    let program = parser
        .parse_program(&mut diags)
        .expect("parse_program must recover rather than return Err");
    (program, diags, parser.had_errors(), sm)
}

#[test]
fn stmt_error_recovery() {
    // A function body whose initializer is missing: the enclosing function
    // survives and the broken statement becomes `Stmt::Error`.
    let src = "main: function () nothing {\n    x: @bit8 = \n}\n";
    let (program, diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors, "a failed statement records the failure");
    assert_eq!(program.items.len(), 1, "the function item survives");
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    assert!(
        block
            .stmts
            .iter()
            .any(|stmt| matches!(stmt, Stmt::Error { .. })),
        "the broken statement recovers into `Stmt::Error`: {:?}",
        block.stmts
    );
    assert!(
        diags.diagnostics().iter().any(|d| d.code == "E001"),
        "the syntax diagnostic is reported: {:?}",
        diags.diagnostics()
    );
}

#[test]
fn broken_type_annotation_recovers_to_poison() {
    // A statement with no type annotation: the poison `Type` (empty name)
    // lets the statement survive.
    let src = "main: function () nothing {\n    x: = 0x1\n}\n";
    let (program, diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors, "a failed type records the failure");
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let Some(Stmt::Let { ty, .. }) = block.stmts.first() else {
        panic!("the statement survives, got {:?}", block.stmts);
    };
    assert!(
        ty.is_error(),
        "a failed type is the error shape, got {ty:?}"
    );
    assert!(
        diags.diagnostics().iter().any(|d| d.code == "E001"),
        "the type diagnostic is reported: {:?}",
        diags.diagnostics()
    );
}

#[test]
fn recovery_keeps_sibling_statements() {
    // A good statement before a broken one is preserved.
    let src = "main: function () nothing {\n    y: @bit8 = 0x1\n    x: @bit8 = \n}\n";
    let (program, _diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors);
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    assert_eq!(block.stmts.len(), 2, "{:?}", block.stmts);
    assert!(
        matches!(&block.stmts[0], Stmt::Let { name, .. } if name == "y"),
        "the good sibling is parsed: {:?}",
        block.stmts[0]
    );
    assert!(
        matches!(block.stmts[1], Stmt::Error { .. }),
        "the broken statement is an error: {:?}",
        block.stmts[1]
    );
}

#[test]
fn call_arg_error_recovers_to_expr_error() {
    // A malformed call argument becomes an `Expr::Error` and the statement
    // (and call) survives.
    let src = "main: function () nothing {\n    f(, 0x1)\n}\n";
    let (program, diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors, "a failed expression records the failure");
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let Stmt::ExprStmt(Expr::Call { args, .. }) = &block.stmts[0] else {
        panic!("the call survives, got {:?}", block.stmts[0]);
    };
    assert!(
        args.iter().any(|arg| matches!(arg, Expr::Error { .. })),
        "the bad argument is an `Expr::Error`: {args:?}"
    );
    assert!(diags.diagnostics().iter().any(|d| d.code == "E001"));
}

#[test]
fn recovered_failure_before_missing_close_is_single_diagnostic() {
    // A recovered type failure whose statement *survives* must not be
    // followed by a spurious "unterminated block": strict mode sees exactly
    // one E001. Before the recovery, the failed statement failed the whole
    // item, so there was a single diagnostic.
    let src = "main: function () nothing {\n    x: = 0x1\n";
    let (program, diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors, "the type failure is recorded");
    assert!(
        matches!(
            program.items.first().map(|item| &item.kind),
            Some(ItemKind::Func(_))
        ),
        "the function survives: {:?}",
        program.items
    );
    let errors: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|d| d.code == "E001")
        .collect();
    assert_eq!(errors.len(), 1, "exactly one E001: {errors:?}");
}

#[test]
fn malformed_initializer_recovers_to_expr_error() {
    // A *present-but-malformed* initializer survives as `Expr::Error`
    // (vs. a *missing* one, which stays `Stmt::Error`).
    let src = "main: function () nothing {\n    x: @bit8 = @\n}\n";
    let (program, _diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors, "the expression failure is recorded");
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let Some(Stmt::Let { value, .. }) = block.stmts.first() else {
        panic!("the statement survives, got {:?}", block.stmts);
    };
    assert!(
        matches!(value, Expr::Error { .. }),
        "the malformed initializer is an `Expr::Error`: {value:?}"
    );
}

/// The E001 diagnostics in `diags`, for exact-count assertions.
fn e001(diags: &Diags) -> Vec<&lilac_error::Diagnostic> {
    diags
        .diagnostics()
        .iter()
        .filter(|d| d.code == "E001")
        .collect()
}

#[test]
fn malformed_call_keeps_following_item() {
    // A call argument failure must not consume the enclosing block's `}`
    // as the call's closing paren: exactly one E001 and the following item
    // survives (Critical 1).
    let src = "main: function () nothing { f(ref) }\nfoo: function () nothing {\n}\n";
    let (program, diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors, "the malformed call records the failure");
    assert_eq!(
        program.items.len(),
        2,
        "the item survives: {:?}",
        program.items
    );
    assert!(
        matches!(&program.items[1].kind, ItemKind::Func(_)),
        "the following item is intact: {:?}",
        program.items[1].kind
    );
    let errors = e001(&diags);
    assert_eq!(errors.len(), 1, "exactly one E001: {errors:?}");
}

#[test]
fn poison_type_before_non_eq_is_single_diagnostic() {
    // `x: ]` leaves `]` where the `=` is expected. The recovered type must
    // not cascade a second "expected `=`" diagnostic at the same span
    // (Critical 2).
    let src = "main: function () nothing {\n    x: ]\n}\n";
    let (_, diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors, "the type failure is recorded");
    let errors = e001(&diags);
    assert_eq!(errors.len(), 1, "exactly one E001: {errors:?}");
}

#[test]
fn self_is_its_own_type_shape() {
    let src = "Box: object {
    same: function (ref self, other: ref self) nothing {}
}
";
    let (program, diags, _) = parse(src);
    assert!(!diags.has_errors(), "{diags:?}");
    let ItemKind::Object { members, .. } = &program.items[0].kind else {
        panic!("expected an object");
    };
    let ItemKind::Func(func) = &members[0].kind else {
        panic!("expected a method");
    };
    assert!(func.params.iter().all(|param| param.ty.is_self()));
    assert!(func.params[1].ty.is_ref);
}

#[test]
fn self_takes_no_generic_arguments() {
    let src = "Box: object {
    same: function (ref self[@bit8]) nothing {}
}
";
    let (_, diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors);
    let errors = e001(&diags);
    assert!(
        errors
            .iter()
            .any(|d| &*d.msg == "`self` takes no generic arguments"),
        "{errors:?}"
    );
}

#[test]
fn broken_statement_keeps_parenthesized_sibling() {
    // A broken statement must not skip past a following `(`-led statement:
    // the sibling survives and strict mode sees a single diagnostic
    // (Important 3).
    let src = "main: function () nothing {\n    x: ]\n    (y)\n}\n";
    let (program, diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors, "the broken statement records the failure");
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    assert!(
        matches!(block.stmts.last(), Some(Stmt::ExprStmt(_))),
        "the `(y)` sibling is an expression statement: {:?}",
        block.stmts
    );
    let errors = e001(&diags);
    assert_eq!(errors.len(), 1, "exactly one E001: {errors:?}");
}

/// Parses `src` on a thread with a compile's stack: at the nesting
/// limit a debug build needs more than a test thread's.
fn parse_deep(src: String) -> (Program, Diags, SourceMap) {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || parse(&src))
        .expect("spawns the parse thread")
        .join()
        .expect("parses without overflowing the stack")
}

/// `main` with `body` as its one statement, then an `after` function.
fn in_main(body: &str) -> String {
    format!("main: function () {{\n    {body}\n}}\n\nafter: function () {{}}\n")
}

#[test]
fn nesting_past_the_limit_is_one_e001() {
    let n = 1000;
    let ifs = format!("{}{}", "if 0x1 {\n".repeat(n), "}\n".repeat(n));
    let cases = [
        (
            "parentheses",
            format!("x: @bit32 = {}0x1{}", "(".repeat(n), ")".repeat(n)),
        ),
        (
            "calls",
            format!("x: @bit32 = {}0x1{}", "f(".repeat(n), ")".repeat(n)),
        ),
        ("ifs", ifs),
        (
            "else ifs",
            format!("if 0x1 {{}}{}", " else if 0x1 {}".repeat(n)),
        ),
        ("loops", format!("{}break", "loop ".repeat(n))),
        (
            "operators",
            format!("x: @bit8 = 0x1{}", " and 0x1".repeat(n)),
        ),
        ("casts", format!("x: @bit8 = 0x1{}", " as @bit8".repeat(n))),
        ("negations", format!("x: @bit32 = {}0x1", "-".repeat(n))),
        ("members", format!("x: @bit32 = a{}", ".b".repeat(n))),
        (
            "types",
            format!("x: {}@bit8{} = 0x0", "A[".repeat(n), "]".repeat(n)),
        ),
    ];
    for (what, body) in cases {
        let (program, diags, _) = parse_deep(in_main(&body));
        let errors = e001(&diags);
        assert_eq!(errors.len(), 1, "{what}: exactly one E001: {errors:?}");
        assert!(
            errors[0].msg.contains("nested too deeply"),
            "{what}: {:?}",
            errors[0].msg
        );
        assert!(
            program.items.iter().any(|item| item.name == "after"),
            "{what}: the next item still parses"
        );
    }
}

/// The function's block, the statement and its expression are three
/// levels, so 253 parentheses reach the limit exactly.
#[test]
fn nesting_limit_is_exact() {
    let parens = |n: usize| {
        in_main(&format!(
            "x: @bit32 = {}0x1{}",
            "(".repeat(n),
            ")".repeat(n)
        ))
    };
    let (_, diags, _) = parse_deep(parens(MAX_NESTING - 3));
    assert!(e001(&diags).is_empty(), "{:?}", diags.diagnostics());
    let (_, diags, _) = parse_deep(parens(MAX_NESTING - 2));
    assert_eq!(e001(&diags).len(), 1, "{:?}", diags.diagnostics());
}

/// Writes an expression with its grouping as parentheses.
fn grouping(expr: &Expr) -> String {
    match expr {
        Expr::Ident(name, _) | Expr::Number(name, _) => name.clone(),
        Expr::BinOp { lhs, op, rhs, .. } => {
            format!("({} {op:?} {})", grouping(lhs), grouping(rhs))
        }
        Expr::Cast(value, ty) => format!("({} as {})", grouping(value), ty.head()),
        Expr::RefOf { value, .. } => format!("(ref {})", grouping(value)),
        Expr::Member { obj, field, .. } => format!("{}.{field}", grouping(obj)),
        Expr::Call { callee, args, .. } => {
            let args: Vec<String> = args.iter().map(grouping).collect();
            format!("{}({})", grouping(callee), args.join(", "))
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn operators_bind_by_precedence() {
    for (src, want) in [
        (
            "a or b and c == d < e + f * g as T",
            "(a Or (b And (c EqEq (d Lt (e Add (f Mul (g as T)))))))",
        ),
        ("a - b - c", "((a Sub b) Sub c)"),
        ("a == b == c", "((a EqEq b) EqEq c)"),
        ("a * b + c * d", "((a Mul b) Add (c Mul d))"),
        ("a as T * b", "((a as T) Mul b)"),
        ("-a as T", "(@negate(a) as T)"),
        ("- ref -a", "@negate((ref @negate(a)))"),
        ("(a or b) and c", "((a Or b) And c)"),
        ("ref @move(a)", "(ref @move(a))"),
    ] {
        let (program, diags, _) = parse(&in_main(&format!("x: T = {src}")));
        assert!(!diags.has_errors(), "{src}: {:?}", diags.diagnostics());
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected function");
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected block body");
        };
        let Stmt::Let { value, .. } = &block.stmts[0] else {
            panic!("expected let, got {:?}", block.stmts[0]);
        };
        assert_eq!(grouping(value), want, "{src}");
    }
}

#[test]
fn conditional_items_parse_if_chains_and_matches() {
    let (file, diags, _) = parse(
        "if a { x: @bit8 => 0x1 } else if b { y: @bit8 => 0x2 } else { z: @bit8 => 0x3 }\n\
         match s { .A { using Q as P } .B => using R as P } else { w: @bit8 => 0x4 }\n",
    );
    assert!(!diags.has_errors(), "{diags:?}");
    let ItemKind::Conditional(chain) = &file.items[0].kind else {
        panic!("expected a conditional, got {:?}", file.items[0].kind);
    };
    assert!(chain.scrutinee.is_none());
    assert_eq!(chain.arms.len(), 2);
    assert_eq!(chain.els.as_ref().map(Vec::len), Some(1));
    let ItemKind::Conditional(by_tag) = &file.items[1].kind else {
        panic!("expected a conditional, got {:?}", file.items[1].kind);
    };
    assert!(by_tag.scrutinee.is_some());
    assert_eq!(by_tag.arms.len(), 2);
    assert!(matches!(
        by_tag.arms[1].items[0].kind,
        ItemKind::Using { .. }
    ));
}

#[test]
fn a_bare_dot_is_an_empty_tag() {
    // Mid-edit: a `.` with no name is one E001, and the function and the
    // rest of its `match` survive.
    let src = "main: function () nothing {\n    x: Color = .\n    match x {\n        .Red => {}\n        .\n        .Blue => {}\n    }\n}\n";
    let (program, diags, had_errors, _) = parse_with_status(src);
    assert!(had_errors);
    let errors: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|d| d.code == "E001")
        .collect();
    assert_eq!(errors.len(), 2, "one E001 per bare `.`: {errors:?}");
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let [Stmt::Let { value, .. }, Stmt::Match { arms, .. }] = block.stmts.as_slice() else {
        panic!("both statements survive, got {:?}", block.stmts);
    };
    assert!(matches!(value, Expr::Tag(name, _) if name.is_empty()));
    let names: Vec<&str> = arms
        .iter()
        .map(|arm| match &arm.pattern {
            MatchPattern::Tag { name, .. } => name.as_str(),
            MatchPattern::Literal { .. } => "?",
        })
        .collect();
    assert_eq!(names, ["Red", "", "Blue"]);
}

#[test]
fn a_dot_ending_a_line_takes_no_name() {
    let src = "main: function () nothing {\n    x: Color = .\n    y: Color = . Red\n    p.\n    z: Color = .Red\n}\n";
    let (program, diags, _, _) = parse_with_status(src);
    let errors: Vec<_> = diags
        .diagnostics()
        .iter()
        .filter(|d| d.code == "E001")
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    let ItemKind::Func(decl) = &program.items[0].kind else {
        panic!("expected function, got {:?}", program.items[0].kind);
    };
    let Some(FuncBody::Block(block)) = &decl.body else {
        panic!("expected block body, got {:?}", decl.body);
    };
    let [Stmt::Let { value: x, .. }, Stmt::Let { value: y, .. }, Stmt::ExprStmt(member), Stmt::Let { name, value: z, .. }] =
        block.stmts.as_slice()
    else {
        panic!("every statement survives, got {:?}", block.stmts);
    };
    assert!(matches!(x, Expr::Tag(tag, _) if tag.is_empty()));
    assert!(matches!(y, Expr::Tag(tag, _) if tag == "Red"));
    assert!(matches!(member, Expr::Member { field, .. } if field.is_empty()));
    assert_eq!(name, "z");
    assert!(matches!(z, Expr::Tag(tag, _) if tag == "Red"));
}

#[test]
fn signatures_declare_failure_outs() {
    let (program, diags, _) = parse(
        "a: function () value: @bit8 else nothing {\n}\nb: function () else why: @bit8, code: @bit32 {\n}\nc: function () value: @bit8 {\n}\nE: enum {\n    V\n    get: function (self) value: @bit8 else nothing {\n    }\n}\n",
    );
    assert!(!diags.has_errors(), "{:?}", diags.diagnostics());
    let failures = |index: usize| match &program.items[index].kind {
        ItemKind::Func(decl) => (decl.returns.len(), decl.failures.as_ref().map(Vec::len)),
        other => panic!("{other:?}"),
    };
    assert_eq!(failures(0), (1, Some(0)));
    assert_eq!(failures(1), (0, Some(2)));
    assert_eq!(failures(2), (1, None));
    // An enum body's functions are read as function types.
    let ItemKind::Enum { members, .. } = &program.items[3].kind else {
        panic!("{:?}", program.items[3].kind);
    };
    let ItemKind::Func(decl) = &members[0].kind else {
        panic!("{:?}", members[0].kind);
    };
    assert_eq!(decl.failures.as_ref().map(Vec::len), Some(0));
}

#[test]
fn failure_handling_statements() {
    let stmts = main_stmts(
        "main: function () nothing {\n    a: A, b: B = f() else -> why: E, code: C {\n        return\n    }\n    c: Box[T].create(x) else {\n        fail\n    }\n    f() else {\n        return\n    }\n    if f() -> d: D, e: E {\n    } else -> why: E {\n    }\n    loop if f() -> line: Text {\n    } else break\n    fail\n}\n",
    );
    let Stmt::Guard {
        bindings,
        captures,
        els,
        ..
    } = &stmts[0]
    else {
        panic!("{:?}", stmts[0]);
    };
    assert_eq!((bindings.len(), captures.len()), (2, 2));
    assert!(matches!(els.stmts.as_slice(), [Stmt::Return { .. }]));
    let Stmt::Guard {
        bindings, value, ..
    } = &stmts[1]
    else {
        panic!("{:?}", stmts[1]);
    };
    assert_eq!(bindings.len(), 1);
    assert!(matches!(value, Expr::Construct { function, .. } if function == "create"));
    assert!(
        matches!(&stmts[2], Stmt::Guard { bindings, captures, .. } if bindings.is_empty() && captures.is_empty())
    );
    let Stmt::If {
        captures,
        else_captures,
        els: Some(els),
        ..
    } = &stmts[3]
    else {
        panic!("{:?}", stmts[3]);
    };
    assert_eq!((captures.len(), else_captures.len()), (2, 1));
    assert!(matches!(**els, Stmt::Block(_)));
    let Stmt::Loop { body, .. } = &stmts[4] else {
        panic!("{:?}", stmts[4]);
    };
    assert!(matches!(&**body, Stmt::If { captures, .. } if captures.len() == 1));
    assert!(matches!(stmts[5], Stmt::Fail { .. }));
}

#[test]
fn failure_syntax_errors() {
    for src in [
        // `fail` takes no value.
        "main: function () nothing {\n    fail 0x1\n}\n",
        // `else` in a signature needs outs or `nothing`.
        "main: function () value: @bit8 else {\n}\n",
        // An `else` with captures needs a block.
        "main: function () nothing {\n    if f() {\n    } else -> why: E return\n}\n",
        // A capture is `name: Type`.
        "main: function () nothing {\n    if f() -> @bit8 {\n    }\n}\n",
    ] {
        let (_, diags, _) = parse(src);
        assert!(diags.has_errors(), "{src}");
    }
}

#[test]
fn aliases_parse() {
    let src = "Format: alias @format_args\nBytes: alias List[@bit8]\nCallback: alias function (@bit32) nothing\n";
    let (program, diags, _) = parse(src);
    assert!(!diags.has_errors(), "{diags:?}");
    let names: Vec<_> = program
        .items
        .iter()
        .map(|item| item.name.as_str())
        .collect();
    assert_eq!(names, ["Format", "Bytes", "Callback"]);
    let ItemKind::Alias { ty } = &program.items[1].kind else {
        panic!("expected an alias, got {:?}", program.items[1].kind);
    };
    let TypeShape::Named { name, args } = &ty.shape else {
        panic!("expected a named type, got {ty:?}");
    };
    assert_eq!(name, "List");
    assert_eq!(args.len(), 1);
    let ItemKind::Alias { ty } = &program.items[2].kind else {
        panic!("expected an alias");
    };
    assert!(matches!(ty.shape, TypeShape::Function(_)));
}

#[test]
fn bad_aliases_are_e001_and_parsing_goes_on() {
    for src in [
        "Thing[T]: alias @bit8\nmain: function () nothing {}\n",
        "Thing: alias ref @bit8\nmain: function () nothing {}\n",
        "@thing: alias @bit8\nmain: function () nothing {}\n",
    ] {
        let (program, diags, _) = parse(src);
        let errors: Vec<_> = diags.diagnostics().iter().map(|d| d.code).collect();
        assert_eq!(errors, ["E001"], "{src}: {diags:?}");
        assert!(
            program.items.iter().any(|item| item.name == "main"),
            "{src}: the next item parses"
        );
    }
}

#[test]
fn aliases_in_type_bodies_parse_for_the_resolver() {
    // Rejected by the resolver (`E036`), like nested types.
    let src = "Thing: object {\n    Inner: alias @bit8\n}\nKind: enum {\n    One\n    Inner: alias @bit8\n}\n";
    let (program, diags, _) = parse(src);
    assert!(!diags.has_errors(), "{diags:?}");
    let ItemKind::Object { members, .. } = &program.items[0].kind else {
        panic!("expected an object");
    };
    assert!(matches!(members[0].kind, ItemKind::Alias { .. }));
    let ItemKind::Enum { members, .. } = &program.items[1].kind else {
        panic!("expected an enum");
    };
    assert!(matches!(members[0].kind, ItemKind::Alias { .. }));
}
