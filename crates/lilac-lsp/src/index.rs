//! Analysis index derived from the retained session (editor queries).

use std::collections::HashMap;

use lilac_driver::{Analysis, Session};
use lilac_hir::{HirBlock, HirExpr, HirExprKind, HirStmt, NameSite};
use lilac_resolve::{DefId, ResItem, ResKind, ResolvedProject};
use lilac_span::{FileId, Span};
use lilac_ty::TypeRef;
use lsp_types::Url;

/// Display info for one definition (hover / go-to-definition).
#[derive(Clone, Debug)]
pub struct DefInfo {
    /// Local (unqualified) name.
    pub name: String,
    /// Namespace- (and owner-) qualified name.
    pub qualified: String,
    /// `function`/`hook`/`object`/`enum`/`const`/`field`.
    pub kind: &'static str,
    /// Whole-declaration span.
    pub span: Span,
}

/// Derived index: definitions, resolved uses, and expression types.
#[derive(Clone, Debug, Default)]
pub struct AnalysisIndex {
    /// `DefId` → display info.
    pub defs: HashMap<DefId, DefInfo>,
    /// Resolved uses, deduped and sorted by `(file, lo)`.
    pub uses: Vec<NameSite>,
    /// Every checked expression's `(span, type)`.
    pub exprs: Vec<(Span, TypeRef)>,
}

/// A retained compile: the session plus its derived index and URI maps.
pub struct Retained {
    /// The last compile's session (owns the source map and analysis).
    pub session: Session,
    /// Derived index for editor queries.
    pub index: AnalysisIndex,
    /// `FileId` → document URI.
    pub file_uris: HashMap<FileId, Url>,
    /// Document URI → `FileId`.
    pub uri_files: HashMap<Url, FileId>,
}

impl Retained {
    /// Builds a retained compile from a finished session and its URI maps.
    #[must_use]
    pub fn build(
        session: Session,
        file_uris: HashMap<FileId, Url>,
        uri_files: HashMap<Url, FileId>,
    ) -> Self {
        let index = session
            .analysis()
            .map_or_else(AnalysisIndex::default, build_index);
        Self {
            session,
            index,
            file_uris,
            uri_files,
        }
    }
}

/// Builds the index from a session's retained analysis.
fn build_index(analysis: &Analysis) -> AnalysisIndex {
    let mut defs = HashMap::new();
    for (_, item) in analysis.project.all_items() {
        add_def(&analysis.project, item, None, &mut defs);
    }
    for overloads in analysis.project.hooks.values() {
        for item in overloads {
            add_def(&analysis.project, item, None, &mut defs);
        }
    }

    let mut uses = analysis.hir.uses.clone();
    uses.sort_by_key(|s| (s.span.file.0, s.span.lo.0, s.span.hi.0, s.def.0));
    uses.dedup_by_key(|s| (s.span.file.0, s.span.lo.0, s.span.hi.0, s.def.0));

    let mut exprs = Vec::new();
    for func in &analysis.hir.funcs {
        walk_block(&func.body, &mut exprs);
    }
    for konst in &analysis.hir.consts {
        walk_expr(&konst.value, &mut exprs);
    }

    AnalysisIndex { defs, uses, exprs }
}

/// Records `item` and, recursively, its object members.
fn add_def(
    project: &ResolvedProject,
    item: &ResItem,
    owner: Option<&str>,
    defs: &mut HashMap<DefId, DefInfo>,
) {
    let qualified = if let Some(owner) = owner {
        format!("{owner}.{}", item.name)
    } else {
        let path = project.namespace_path(item.ns);
        if path.is_empty() {
            item.name.clone()
        } else {
            format!("{path}.{}", item.name)
        }
    };
    defs.insert(
        item.id,
        DefInfo {
            name: item.name.clone(),
            qualified: qualified.clone(),
            kind: kind_of(&item.kind),
            span: item.span,
        },
    );
    if let ResKind::Object { members, .. } = &item.kind {
        for member in members.values() {
            add_def(project, member, Some(&qualified), defs);
        }
    }
}

/// The hover label for a definition kind.
fn kind_of(kind: &ResKind) -> &'static str {
    match kind {
        ResKind::Func { .. } => "function",
        ResKind::Hook { .. } => "hook",
        ResKind::Object { .. } => "object",
        ResKind::Enum { .. } => "enum",
        ResKind::Const => "const",
        ResKind::Field => "field",
    }
}

/// Collects every expression in a block, depth-first.
fn walk_block(block: &HirBlock, out: &mut Vec<(Span, TypeRef)>) {
    for stmt in &block.stmts {
        walk_stmt(stmt, out);
    }
}

/// Collects every expression in a statement, depth-first.
fn walk_stmt(stmt: &HirStmt, out: &mut Vec<(Span, TypeRef)>) {
    match stmt {
        HirStmt::Let { value, .. } | HirStmt::MultiLet { value, .. } | HirStmt::ExprStmt(value) => {
            walk_expr(value, out);
        }
        HirStmt::Assign { target, value, .. } => {
            walk_expr(target, out);
            walk_expr(value, out);
        }
        HirStmt::Return { .. } | HirStmt::Break { .. } => {}
        HirStmt::If {
            cond, then, els, ..
        } => {
            walk_expr(cond, out);
            walk_block(then, out);
            walk_block(els, out);
        }
        HirStmt::Loop { body, .. } => walk_block(body, out),
        HirStmt::Match {
            scrutinee,
            arms,
            els,
            ..
        } => {
            walk_expr(scrutinee, out);
            for arm in arms {
                walk_block(&arm.body, out);
            }
            walk_block(els, out);
        }
    }
}

/// Collects an expression and its sub-expressions, depth-first.
fn walk_expr(expr: &HirExpr, out: &mut Vec<(Span, TypeRef)>) {
    out.push((expr.span, expr.ty));
    match &expr.kind {
        HirExprKind::Ident { .. }
        | HirExprKind::Tag { .. }
        | HirExprKind::Number { .. }
        | HirExprKind::Str { .. }
        | HirExprKind::Char { .. }
        | HirExprKind::Temp(_)
        | HirExprKind::CompileError { .. }
        | HirExprKind::Poison => {}
        HirExprKind::Variant { payload, .. } => {
            if let Some(payload) = payload {
                walk_expr(payload, out);
            }
        }
        HirExprKind::Call { args, .. } => {
            for arg in args {
                walk_expr(arg, out);
            }
        }
        HirExprKind::EqEq { lhs, rhs } => {
            walk_expr(lhs, out);
            walk_expr(rhs, out);
        }
        HirExprKind::Member { obj, .. } => walk_expr(obj, out),
        HirExprKind::RefOf { value } => walk_expr(value, out),
        HirExprKind::StructLit { fields } => {
            for field in fields {
                walk_expr(&field.value, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use lilac_span::FileId;
    use lsp_types::Url;

    fn retained(src: &str) -> Retained {
        let mut session = lilac_driver::Session::new();
        let _ = session.compile_lir("test.lilac", src);
        let uri = Url::parse("file:///c:/test.lilac").expect("uri");
        Retained::build(
            session,
            HashMap::from([(FileId(0), uri.clone())]),
            HashMap::from([(uri, FileId(0))]),
        )
    }

    #[test]
    fn index_has_defs_uses_and_exprs() {
        let retained =
            retained("pi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n");
        assert!(retained
            .index
            .defs
            .values()
            .any(|d| d.name == "pi" && d.kind == "const"));
        assert!(retained
            .index
            .defs
            .values()
            .any(|d| d.name == "main" && d.kind == "function"));
        assert!(!retained.index.uses.is_empty());
        assert!(!retained.index.exprs.is_empty());
    }
}
