//! Conversions and operator desugar (SPEC §8).

use lilac_ast::BinOp;
use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_span::Span;
use lilac_ty::{FuncSig, TypeId, TypeRef, Types};

use super::Sigs;

/// Tries a single directed `@conversion(from → to)` step.
///
/// Callers check exact matches first (conversions never steal one) and keep
/// `ref` mismatches as their own `E010`: only non-`ref` mismatches reach here.
/// No transitive chains in v1. Success is type-level acceptance (M1 emits no
/// code); the target type is returned for uniformity.
pub(crate) fn convert(
    types: &Types,
    diags: &mut Diags,
    sigs: &Sigs,
    from: TypeRef,
    to: TypeId,
    expected_span: Span,
    actual_span: Span,
) -> Result<TypeId, ErrorGuaranteed> {
    debug_assert!(!from.is_ref);
    if sigs
        .conversions
        .iter()
        .any(|c| c.from == from.id && c.to == to)
    {
        return Ok(to);
    }
    let target = types.display(to);
    Err(diags.emit(
        Diagnostic::error(
            "E043",
            format!("no conversion from {} to {target}", types.display(from.id)),
        )
        .label(expected_span, false, format!("expected `{target}` here"))
        .label(actual_span, true, "conversion attempted here".to_owned()),
    ))
}

/// Hook name for a binary operator; `==` has none (strict bit-equality).
pub(crate) fn binop_hook_name(op: BinOp) -> Option<&'static str> {
    match op {
        BinOp::Add => Some("@add"),
        BinOp::Sub => Some("@sub"),
        BinOp::Mul => Some("@mul"),
        BinOp::Div => Some("@div"),
        BinOp::Mod => Some("@mod"),
        BinOp::Lt => Some("@less_than"),
        BinOp::Gt => Some("@greater_than"),
        BinOp::LtEq => Some("@less_than_or_equal"),
        BinOp::GtEq => Some("@greater_than_or_equal"),
        BinOp::EqEq => None,
    }
}

/// Failed hook resolution: the exact pair tried.
pub(crate) struct BinopMiss {
    /// Hook that was resolved, e.g. `@add`.
    pub hook: String,
    /// Left operand base type.
    pub lhs: TypeId,
    /// Right operand base type.
    pub rhs: TypeId,
}

/// Resolves a binary-operator call: exact `(lhs, rhs)` overload first, then
/// one directed conversion of the right side toward the left. Never converts
/// the left side, never chains. Hook signatures with `ref` params or arity
/// other than 2-in/1-out do not participate.
pub(crate) fn resolve_binop(
    sigs: &Sigs,
    hook: &str,
    lhs: TypeId,
    rhs: TypeId,
) -> Result<FuncSig, BinopMiss> {
    let miss = || BinopMiss {
        hook: hook.to_owned(),
        lhs,
        rhs,
    };
    let overloads = sigs.hooks.get(hook).ok_or_else(&miss)?;
    let shaped = |sig: &FuncSig| {
        sig.params.len() == 2
            && sig.returns.len() == 1
            && !sig.params[0].is_ref
            && !sig.params[1].is_ref
    };
    if let Some((_, sig)) = overloads
        .iter()
        .find(|(_, sig)| shaped(sig) && sig.params[0].id == lhs && sig.params[1].id == rhs)
    {
        return Ok(sig.clone());
    }
    if sigs
        .conversions
        .iter()
        .any(|c| c.from == rhs && c.to == lhs)
    {
        if let Some((_, sig)) = overloads
            .iter()
            .find(|(_, sig)| shaped(sig) && sig.params[0].id == lhs && sig.params[1].id == lhs)
        {
            return Ok(sig.clone());
        }
    }
    Err(miss())
}

#[cfg(test)]
mod tests {
    use lilac_error::Diags;
    use lilac_lex::Lexer;
    use lilac_parse::Parser;
    use lilac_span::SourceMap;
    use lilac_ty::Types;

    use super::super::{check_bodies, collect_sigs};

    fn check_src(src: &str) -> Diags {
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
            .expect("test source parses");
        let module = lilac_resolve::collect(&program, &mut diags);
        let mut types = Types::new();
        let sigs = collect_sigs(&module, &program, &mut types, &mut diags);
        check_bodies(&program, &module, &sigs, &mut types, &mut diags);
        diags
    }

    fn has_code(diags: &Diags, code: &str) -> bool {
        diags.diagnostics().iter().any(|d| d.code == code)
    }

    fn conversion_count(src: &str) -> usize {
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
            .expect("test source parses");
        let module = lilac_resolve::collect(&program, &mut diags);
        let mut types = Types::new();
        let sigs = collect_sigs(&module, &program, &mut types, &mut diags);
        sigs.conversions.len()
    }

    fn conversion_hook() -> &'static str {
        "@conversion: function (from: @bit32) to: @bit8 {\n    to = @truncate(from)\n}\n"
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
    fn conversion_does_not_chain() {
        // Both steps exist as overloads, but v1 takes exactly one step: a
        // chaining compiler would accept this, ours reports E043.
        let src = "@conversion: function (from: @bit32) to: @bit16 {\n    to = @truncate(from)\n}\n@conversion: function (from: @bit16) to: @bit8 {\n    to = @truncate(from)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    c: @bit8 = a\n}\n";
        assert_eq!(conversion_count(src), 2);
        let diags = check_src(src);
        assert!(has_code(&diags, "E043"), "{diags:?}");
    }

    #[test]
    fn binop_anchors_left() {
        let diags = check_src(
            "@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 {\n    result = @iadd(lhs, rhs)\n}\n@conversion: function (from: @bit8) to: @bit32 {\n    to = @extend_zero(from)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit8 = 0x2\n    z: @bit32 = a + b\n    w: @bit32 = b + a\n}\n",
        );
        assert_eq!(
            diags
                .diagnostics()
                .iter()
                .filter(|d| d.code == "E043")
                .count(),
            1,
            "{diags:?}"
        );
    }

    #[test]
    fn exact_match_beats_conversion() {
        let diags = check_src(
            "@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 {\n    result = @iadd(lhs, rhs)\n}\n@add: function (lhs: @bit8, rhs: @bit32) result: @bit32 {\n    result = @iadd(@truncate(lhs), rhs)\n}\n@conversion: function (from: @bit8) to: @bit32 {\n    to = @extend_zero(from)\n}\nmain: function () nothing {\n    x: @bit8 = 0x1\n    y: @bit8 = 0x2\n    z: @bit8 = x + y\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn binop_literal_takes_sibling_width() {
        let diags = check_src(
            "@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 {\n    result = @iadd(lhs, rhs)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    z: @bit32 = a + 0x2\n    w: @bit32 = 0x3 + a\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn equality_literal_takes_sibling_width() {
        let diags = check_src(
            "main: function () nothing {\n    a: @bit8 = 0x1\n    c: @bit8 = a == 0x1\n}\n",
        );
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
            "@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 {\n    result = @iadd(lhs, rhs)\n}\n@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 {\n    result = @iadd(lhs, rhs)\n}\nmain: function () nothing {\n    a: @bit32 = 0x1\n    b: @bit32 = 0x2\n    z: @bit32 = @add(a, b)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }
}
