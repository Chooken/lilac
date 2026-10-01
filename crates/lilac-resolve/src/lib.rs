//! Name resolution: `DefId`, scopes, folder-modules (real pass lands in M1).

use std::collections::HashMap;

use lilac_ast::{Item, ItemKind, Program};
use lilac_error::{Diags, Diagnostic};
use lilac_span::Span;

/// HIR-level definition identity, stable across passes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DefId(pub u32);

impl DefId {
    /// Placeholder for error-recovery poison.
    pub const ERROR: Self = Self(u32::MAX);
}

/// One collected definition.
#[derive(Clone, Debug)]
pub struct ResItem {
    /// Stable identity.
    pub id: DefId,
    /// What was defined.
    pub kind: ResKind,
    /// Definition span.
    pub span: Span,
}

/// Definition kinds tracked for typeck.
#[derive(Clone, Debug)]
pub enum ResKind {
    /// `name: function ...` (or `extern`, which has no body).
    Func { /// Foreign declaration without a body.
        is_extern: bool,
        /// Whether it declares generic parameters (bodies check in M4).
        generic: bool,
    },
    /// `@`-named hook (`@add`, `@conversion`, ...).
    Hook {
        /// Whether it declares generic parameters (rejected with `E052`).
        generic: bool,
    },
    /// `name: object { ... }` with collected members.
    Object {
        /// Member name → definition.
        members: HashMap<String, ResItem>,
    },
    /// `name: enum { ... }` with variant names in order.
    Enum {
        /// Variant names.
        variants: Vec<String>,
    },
    /// `name: union { ... }` (members collect in M2).
    Union,
    /// `name: Type = expr` module constant.
    Const,
    /// `member: Type` bare object field.
    Field,
}

/// One namespace unit: top-level names of a file.
#[derive(Clone, Debug, Default)]
pub struct Module {
    /// Top-level name → definition.
    pub items: HashMap<String, ResItem>,
}

struct Collector<'a> {
    diags: &'a mut Diags,
    next: u32,
}

impl Collector<'_> {
    fn fresh(&mut self) -> DefId {
        let id = DefId(self.next);
        self.next += 1;
        id
    }

    fn insert(&mut self, scope: &mut HashMap<String, ResItem>, name: &str, item: ResItem) {
        if let Some(previous) = scope.get(name) {
            let previous_span = previous.span;
            self.diags.emit(
                Diagnostic::error("E020", format!("duplicate definition of `{name}`"))
                    .label(item.span, true, "redefined here")
                    .label(previous_span, false, "first defined here"),
            );
            return;
        }
        scope.insert(name.to_owned(), item);
    }

    fn item(&mut self, item: &Item) -> Option<ResItem> {
        let id = self.fresh();
        let kind = match &item.kind {
            ItemKind::Func(decl) => {
                let generic = !decl.generic_params.is_empty();
                if item.name.starts_with('@') {
                    ResKind::Hook { generic }
                } else {
                    ResKind::Func {
                        is_extern: decl.is_extern,
                        generic,
                    }
                }
            }
            ItemKind::Object { members, .. } => {
                let mut table = HashMap::new();
                for member in members {
                    if let Some(resolved) = self.item(member) {
                        self.insert(&mut table, &member.name, resolved);
                    }
                }
                ResKind::Object { members: table }
            }
            ItemKind::Enum { variants, .. } => ResKind::Enum {
                variants: variants.iter().map(|v| v.name.clone()).collect(),
            },
            ItemKind::Union { .. } => ResKind::Union,
            ItemKind::Using { .. } => return None,
            ItemKind::Const { .. } => ResKind::Const,
            ItemKind::Field { .. } => ResKind::Field,
        };
        Some(ResItem {
            id,
            kind,
            span: item.span,
        })
    }
}

/// Collects every top-level (and object-member) definition.
///
/// `using` directives are not definitions and are skipped. Duplicates keep
/// the first binding and report `E020` at each redefinition.
pub fn collect(program: &Program, diags: &mut Diags) -> Module {
    let mut collector = Collector { diags, next: 0 };
    let mut module = Module::default();
    for item in &program.items {
        if let Some(resolved) = collector.item(item) {
            collector.insert(&mut module.items, &item.name, resolved);
        }
    }
    module
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_error::Diags;
    use lilac_lex::Lexer;
    use lilac_parse::Parser;
    use lilac_span::SourceMap;

    fn resolve_src(src: &str) -> (Module, Diags) {
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
        let module = collect(&program, &mut diags);
        (module, diags)
    }

    fn e020_count(diags: &Diags) -> usize {
        diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E020")
            .count()
    }

    #[test]
    fn duplicate_top_level_names_emit_e020() {
        let (_, diags) = resolve_src("get: function () nothing {}\nget: function () nothing {}\n");
        assert_eq!(e020_count(&diags), 1);
    }

    #[test]
    fn object_members_resolve() {
        let (module, diags) = resolve_src(
            "Object: object {\n    member: @bit8\n    get: function () nothing {}\n}\n",
        );
        assert!(!diags.has_errors());
        let object = module.items.get("Object").expect("has Object");
        let ResKind::Object { members } = &object.kind else {
            panic!("expected object, got {:?}", object.kind);
        };
        assert!(matches!(members.get("member").map(|m| &m.kind), Some(ResKind::Field)));
        assert!(matches!(
            members.get("get").map(|m| &m.kind),
            Some(ResKind::Func { .. })
        ));
    }

    #[test]
    fn hooks_marked_hook() {
        let (module, diags) = resolve_src("@myhook: function () nothing {}\n");
        assert!(!diags.has_errors());
        let hook = module.items.get("@myhook").expect("has hook");
        assert!(matches!(hook.kind, ResKind::Hook { .. }));
    }

    #[test]
    fn enum_variants_recorded() {
        let (module, diags) = resolve_src("Number: enum {\n    One\n    Two\n}\n");
        assert!(!diags.has_errors());
        let number = module.items.get("Number").expect("has Number");
        let ResKind::Enum { variants } = &number.kind else {
            panic!("expected enum, got {:?}", number.kind);
        };
        assert_eq!(variants, &["One".to_owned(), "Two".to_owned()]);
    }
}
