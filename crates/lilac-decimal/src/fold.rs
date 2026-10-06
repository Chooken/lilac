//! Post-order AST fold: rewrite `a <op> b` to a single decimal `Number` when
//! both operands are decimal literals and `<op>` is `+ - * / %`.
use crate::{eval, is_decimal_literal, DecimalError, Op, Policy};
use lilac_ast::{walk_expr_mut, BinOp, Expr, MutVisitor, Program};
use lilac_error::{Diagnostic, Diags};

/// The mutable visitor state: evaluation policy and the diagnostic sink.
struct Folder<'a> {
    policy: Policy,
    diags: &'a mut Diags,
}

impl MutVisitor for Folder<'_> {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        walk_expr_mut(self, expr); // post-order: inner folds are `Number`s already
        let Expr::BinOp { lhs, op, rhs, span } = expr else {
            return;
        };
        let Some(op) = op_of(*op) else {
            return;
        };
        let (Expr::Number(l, _), Expr::Number(r, _)) = (&**lhs, &**rhs) else {
            return;
        };
        if !is_decimal_literal(l) || !is_decimal_literal(r) {
            return;
        }
        match eval(op, l, r, self.policy) {
            Ok(s) => *expr = Expr::Number(s, *span),
            Err(DecimalError::DivideByZero) => {
                self.diags
                    .emit(Diagnostic::error("E060", "division by zero").label(
                        *span,
                        true,
                        "in decimal literal",
                    ));
            }
            Err(DecimalError::ModuloByZero) => {
                self.diags
                    .emit(Diagnostic::error("E060", "modulo by zero").label(
                        *span,
                        true,
                        "in decimal literal",
                    ));
            }
            // Unreachable: `is_decimal_literal` re-checks the grammar `eval`
            // parses, so a valid-looking literal cannot be malformed.
            Err(DecimalError::Malformed) => {}
        }
    }
}

/// Maps a foldable arithmetic operator to its [`Op`]; comparisons and logical
/// operators are not folded.
fn op_of(op: BinOp) -> Option<Op> {
    match op {
        BinOp::Add => Some(Op::Add),
        BinOp::Sub => Some(Op::Sub),
        BinOp::Mul => Some(Op::Mul),
        BinOp::Div => Some(Op::Div),
        BinOp::Mod => Some(Op::Mod),
        BinOp::Lt
        | BinOp::Gt
        | BinOp::LtEq
        | BinOp::GtEq
        | BinOp::EqEq
        | BinOp::And
        | BinOp::Or => None,
    }
}

/// Folds every foldable decimal-literal binop in `program` in place, emitting
/// `E060` for a decimal division or modulo by zero.
pub fn fold(program: &mut Program, policy: Policy, diags: &mut Diags) {
    let mut folder = Folder { policy, diags };
    folder.visit_program_mut(program);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Round;
    use lilac_ast::{walk_expr, walk_program, Expr, FuncBody, ItemKind, Program, Stmt, Visitor};
    use lilac_error::Diags;
    use lilac_lex::Lexer;
    use lilac_parse::Parser;
    use lilac_span::SourceMap;

    fn fold_src(src: &str) -> (Program, Diags) {
        let mut sm = SourceMap::new();
        let file = sm.add_file("t.lilac", src);
        let (tokens, _) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        let mut parser = Parser::new(tokens, file);
        let mut program = parser.parse_program(&mut diags).expect("parses");
        fold(
            &mut program,
            Policy {
                scale: 10,
                round: Round::Trunc,
            },
            &mut diags,
        );
        (program, diags)
    }

    struct Numbers(Vec<String>);
    impl Visitor for Numbers {
        fn visit_expr(&mut self, expr: &Expr) {
            if let Expr::Number(t, _) = expr {
                self.0.push(t.clone());
            }
            walk_expr(self, expr);
        }
    }
    fn numbers(p: &Program) -> Vec<String> {
        let mut n = Numbers(Vec::new());
        walk_program(&mut n, p);
        n.0
    }

    /// The initializer of the first `let` in the first function of `program`.
    fn let_value(program: &Program) -> &Expr {
        let ItemKind::Func(decl) = &program.items[0].kind else {
            panic!("expected a function item");
        };
        let Some(FuncBody::Block(block)) = &decl.body else {
            panic!("expected a block body");
        };
        let Stmt::Let { value, .. } = &block.stmts[0] else {
            panic!("expected a let statement");
        };
        value
    }

    #[test]
    fn folds_nested_literal_expression() {
        let (p, d) = fold_src("main: function () nothing {\n    x: @byte_buffer = 2 * 3 + 4\n}\n");
        assert!(!d.has_errors());
        assert_eq!(numbers(&p), vec!["10".to_owned()]);
    }

    #[test]
    fn leaves_non_decimal_operands() {
        let (p, _) =
            fold_src("main: function () nothing {\n    a: @byte_buffer = 2\n    x: @byte_buffer = a + 2\n}\n");
        assert_eq!(numbers(&p), vec!["2".to_owned(), "2".to_owned()]); // no folded "4"
        let (p, _) = fold_src("main: function () nothing {\n    x: @byte_buffer = 2 + 0x3\n}\n");
        assert!(!numbers(&p).contains(&"5".to_owned()));
    }

    #[test]
    fn leaves_comparisons() {
        let (p, _) = fold_src("main: function () nothing {\n    x: @bit8 = 2 < 3\n}\n");
        assert_eq!(numbers(&p), vec!["2".to_owned(), "3".to_owned()]);
    }

    #[test]
    fn folds_inside_control_flow_and_fstring() {
        let (p, _) =
            fold_src("main: function () nothing {\n    x: @byte_buffer = $\"{2 * 3}\"\n}\n");
        assert_eq!(numbers(&p), vec!["6".to_owned()]);
    }

    /// Review Focus #4: a `Number op Number` pair nested in control flow and
    /// data-literal positions still folds, not just one inside an f-string.
    #[test]
    fn folds_inside_if_and_loop_bodies() {
        let (p, d) = fold_src(
            "main: function () nothing {\n    if flag {\n        x: @byte_buffer = 2 * 3\n    }\n    loop {\n        y: @byte_buffer = 4 + 5\n        break\n    }\n}\n",
        );
        assert!(!d.has_errors());
        assert_eq!(numbers(&p), vec!["6".to_owned(), "9".to_owned()]);
    }

    #[test]
    fn folds_inside_match_arm() {
        let (p, d) = fold_src(
            "main: function () nothing {\n    match n {\n        0x1 => {\n            x: @byte_buffer = 2 * 3\n        }\n    }\n}\n",
        );
        assert!(!d.has_errors());
        assert_eq!(numbers(&p), vec!["6".to_owned()]);
    }

    #[test]
    fn folds_inside_struct_literal_field() {
        let (p, d) = fold_src(
            "Object: object {\n    member: @byte_buffer\n}\nmain: function () nothing {\n    v: Object = Object {\n        member = 2 * 3\n    }\n}\n",
        );
        assert!(!d.has_errors());
        assert_eq!(numbers(&p), vec!["6".to_owned()]);
    }

    /// The folded `Number` keeps the original binop's span, so a later pass
    /// (and diagnostics) still points at the source expression.
    #[test]
    fn folded_number_preserves_binop_span() {
        let src = "main: function () nothing {\n    x: @byte_buffer = 2 * 3\n}\n";
        let mut sm = SourceMap::new();
        let file = sm.add_file("t.lilac", src);
        let (tokens, _) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        let mut parser = Parser::new(tokens, file);
        let mut program = parser.parse_program(&mut diags).expect("parses");
        let before = match let_value(&program) {
            Expr::BinOp { span, .. } => *span,
            other => panic!("expected a binop before folding, got {other:?}"),
        };
        fold(
            &mut program,
            Policy {
                scale: 10,
                round: Round::Trunc,
            },
            &mut diags,
        );
        let after = match let_value(&program) {
            Expr::Number(_, span) => *span,
            other => panic!("expected a folded number, got {other:?}"),
        };
        assert_eq!(before, after);
    }

    #[test]
    fn division_by_zero_reports_e060() {
        let (_, d) = fold_src("main: function () nothing {\n    x: @byte_buffer = 1 / 0\n}\n");
        assert!(d.diagnostics().iter().any(|e| e.code == "E060"));
    }

    #[test]
    fn modulo_by_zero_reports_e060() {
        let (_, d) = fold_src("main: function () nothing {\n    x: @byte_buffer = 1 % 0\n}\n");
        assert!(
            d.diagnostics()
                .iter()
                .any(|e| e.code == "E060" && e.msg.contains("modulo by zero")),
            "{:?}",
            d.diagnostics()
        );
    }
}
