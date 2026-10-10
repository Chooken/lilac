//! Analysis index derived from the retained session (editor queries).

use std::collections::HashMap;

use lilac_driver::{Analysis, Session};
use lilac_hir::{AliasSite, LocalSite, NameSite, SemanticSite};
use lilac_ir::LirModule;
use lilac_resolve::{DefId, DefKind};
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
    /// The declaration name's token span (the whole span's leading token).
    pub name_span: Span,
    /// Owning object or enum, for members and variants.
    pub parent: Option<DefId>,
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
    /// Non-item semantic-token sites (locals, parameters, type names,
    /// builtins), deduped by `(span, kind)` and sorted by `(file, lo)`.
    pub semantics: Vec<SemanticSite>,
    /// Local and parameter names, each linked to its declaration; deduped
    /// and sorted by `(file, lo)`.
    pub locals: Vec<LocalSite>,
    /// `using … as` alias names, each linked to its declaration: every
    /// declaration (as its own site) and every path head naming one;
    /// deduped and sorted by `(file, lo)`.
    pub aliases: Vec<AliasSite>,
}

/// The IR modules captured at each pipeline boundary during a compile.
#[derive(Clone, Debug, Default)]
pub struct LirStages {
    /// Stage name → module as it stood at that boundary, in pipeline order.
    pub modules: Vec<(&'static str, LirModule)>,
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
    /// IR modules captured at each pipeline boundary.
    pub stages: LirStages,
}

impl Retained {
    /// Builds a retained compile from a finished session and its URI maps.
    #[must_use]
    pub fn build(
        session: Session,
        file_uris: HashMap<FileId, Url>,
        uri_files: HashMap<Url, FileId>,
    ) -> Self {
        Self::build_with_stages(session, file_uris, uri_files, LirStages::default())
    }

    /// Builds a retained compile that carries the captured IR stages.
    #[must_use]
    pub fn build_with_stages(
        session: Session,
        file_uris: HashMap<FileId, Url>,
        uri_files: HashMap<Url, FileId>,
        stages: LirStages,
    ) -> Self {
        let index = session
            .analysis()
            .map_or_else(AnalysisIndex::default, build_index);
        Self {
            session,
            index,
            file_uris,
            uri_files,
            stages,
        }
    }
}

/// Builds the index from a session's retained analysis.
fn build_index(analysis: &Analysis) -> AnalysisIndex {
    let resolved = &analysis.resolved;
    let mut defs = HashMap::new();
    for id in resolved.ids() {
        let def = resolved.def(id);
        let kind = match &def.kind {
            DefKind::Func(_) => "function",
            DefKind::Hook(_) => "hook",
            DefKind::Object => "object",
            DefKind::Enum => "enum",
            DefKind::Const { .. } => "const",
            DefKind::Global { .. } => "global",
            DefKind::Field(_) => "field",
            DefKind::Variant(_) => "variant",
        };
        defs.insert(
            id,
            DefInfo {
                name: def.name.clone(),
                qualified: resolved.qualified_name(id),
                kind,
                span: def.span,
                name_span: def.name_span,
                parent: def.parent,
            },
        );
    }

    let mut uses = analysis.hir.uses.clone();
    uses.sort_by_key(|s| (s.span.file.0, s.span.lo.0, s.span.hi.0, s.def.0));
    uses.dedup_by_key(|s| (s.span.file.0, s.span.lo.0, s.span.hi.0, s.def.0));

    let mut locals = analysis.hir.locals.clone();
    locals.sort_by_key(|site| (site.span.file.0, site.span.lo.0, site.span.hi.0));
    locals.dedup();

    let mut aliases = analysis.hir.alias_uses.clone();
    aliases.extend(resolved.files.values().flat_map(|scope| {
        scope.aliases.values().map(|(_, decl)| AliasSite {
            span: *decl,
            decl: *decl,
        })
    }));
    aliases.retain(|site| !site.span.is_dummy());
    aliases.sort_by_key(|site| (site.span.file.0, site.span.lo.0, site.span.hi.0));
    aliases.dedup();

    let mut exprs = Vec::new();
    let mut record = |expr: &lilac_hir::HirExpr| exprs.push((expr.span, expr.ty));
    for func in &analysis.hir.funcs {
        if let Some(body) = &func.body {
            body.walk_exprs(&mut record);
        }
    }
    for value in analysis.hir.consts.values() {
        value.walk(&mut record);
    }

    AnalysisIndex {
        defs,
        uses,
        exprs,
        semantics: analysis.semantics.clone(),
        locals,
        aliases,
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
    fn retained_carries_captured_stages() {
        let mut session = lilac_driver::Session::new();
        let mut stages = LirStages::default();
        let _ = session.compile_lir_with_stages(
            "t.lilac",
            "main: function () nothing {\n}\n",
            &mut |name, lir| {
                stages.modules.push((name, lir.clone()));
            },
        );
        let uri = Url::parse("file:///c:/t.lilac").expect("uri");
        let retained = Retained::build_with_stages(
            session,
            HashMap::from([(FileId(0), uri.clone())]),
            HashMap::from([(uri, FileId(0))]),
            stages,
        );
        let names: Vec<&str> = retained.stages.modules.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec!["lower", "runtime", "final"]);
    }

    #[test]
    fn index_has_defs_uses_and_exprs() {
        let retained =
            retained("pi: @bit32 = 0x2A\nmain: function () nothing {\n    x: @bit32 = pi\n}\n");
        assert!(retained
            .index
            .defs
            .values()
            .any(|d| d.name == "pi" && d.kind == "global"));
        assert!(retained
            .index
            .defs
            .values()
            .any(|d| d.name == "main" && d.kind == "function"));
        assert!(!retained.index.uses.is_empty());
        assert!(!retained.index.exprs.is_empty());
    }

    #[test]
    fn index_qualifies_member_hooks_by_owner() {
        let retained = retained(
            "Int64: object {\n    data: @bit64\n\
             @add: function (lhs: Int64, rhs: Int64) Int64 => lhs\n}\n\
             main: function () nothing {\n}\n",
        );
        assert!(
            retained
                .index
                .defs
                .values()
                .any(|d| d.name == "@add" && d.qualified == "Int64.@add" && d.kind == "hook"),
            "{:?}",
            retained
                .index
                .defs
                .values()
                .map(|d| &d.qualified)
                .collect::<Vec<_>>()
        );
    }
}
