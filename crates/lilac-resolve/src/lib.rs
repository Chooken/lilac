//! Name resolution: `DefId`, scopes, folder-modules (real pass lands in M1).

use std::collections::HashMap;

use lilac_ast::{Item, ItemKind, Param, Program};
use lilac_error::{Diagnostic, Diags};
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
    Func {
        /// Foreign declaration without a body.
        is_extern: bool,
        /// Whether it declares generic parameters (bodies check in M4).
        generic: bool,
        /// Declared parameters (for signature collection).
        params: Vec<Param>,
        /// Declared return slots (for signature collection).
        returns: Vec<Param>,
    },
    /// `@`-named hook (`@add`, `@conversion`, ...).
    Hook {
        /// Whether it declares generic parameters (rejected with `E052`).
        generic: bool,
        /// Declared parameters (for signature collection).
        params: Vec<Param>,
        /// Declared return slots (for signature collection).
        returns: Vec<Param>,
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
    /// Top-level name → definition (first binding wins on duplicates).
    pub items: HashMap<String, ResItem>,
    /// Hook name → every overload in definition order.
    pub hooks: HashMap<String, Vec<ResItem>>,
}

/// Signature key for overload uniqueness (names excluded, types included).
fn sig_key(params: &[Param], returns: &[Param]) -> String {
    let one = |p: &Param| format!("{}:{}:{:?}", p.ty.name, p.ty.is_ref, p.ty.generic_args);
    let ps: Vec<String> = params.iter().map(one).collect();
    let rs: Vec<String> = returns.iter().map(one).collect();
    format!("({})->({})", ps.join(","), rs.join(","))
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
                let params = decl.params.clone();
                let returns = decl.returns.clone();
                if item.name.starts_with('@') {
                    ResKind::Hook {
                        generic,
                        params,
                        returns,
                    }
                } else {
                    ResKind::Func {
                        is_extern: decl.is_extern,
                        generic,
                        params,
                        returns,
                    }
                }
            }
            ItemKind::Object { members, .. } => {
                let mut table = HashMap::new();
                for member in members {
                    if let Some(resolved) = self.item(member) {
                        // Member scope tracks no overload sets: same-name
                        // members always report E020, hooks included.
                        self.insert_overload(&mut table, None, &member.name, resolved);
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

    /// Inserts into a scope, allowing hook overloads with distinct signatures.
    /// Identical hook signatures (and any non-hook redefinition) report `E020`.
    fn insert_overload(
        &mut self,
        scope: &mut HashMap<String, ResItem>,
        hooks: Option<&mut HashMap<String, Vec<ResItem>>>,
        name: &str,
        item: ResItem,
    ) {
        let same_sig = |a: &ResItem, b: &ResItem| match (&a.kind, &b.kind) {
            (
                ResKind::Hook {
                    params: ap,
                    returns: ar,
                    ..
                },
                ResKind::Hook {
                    params: bp,
                    returns: br,
                    ..
                },
            ) => sig_key(ap, ar) == sig_key(bp, br),
            _ => true,
        };
        if matches!(item.kind, ResKind::Hook { .. }) {
            if let Some(hooks) = hooks {
                let overloads = hooks.entry(name.to_owned()).or_default();
                if overloads.iter().any(|prev| same_sig(prev, &item)) {
                    let previous_span = overloads.first().map_or(item.span, |prev| prev.span);
                    self.diags.emit(
                        Diagnostic::error("E020", format!("duplicate definition of `{name}`"))
                            .label(item.span, true, "redefined here")
                            .label(previous_span, false, "first defined here"),
                    );
                    return;
                }
                let first = overloads.is_empty();
                overloads.push(item.clone());
                if !first {
                    return;
                }
            }
        }
        self.insert(scope, name, item);
    }
}

/// Collects every top-level (and object-member) definition.
///
/// `using` directives are not definitions and are skipped. Non-hook
/// duplicates keep the first binding and report `E020`; hooks allow overloads
/// with distinct signatures (identical ones still report `E020`).
pub fn collect(program: &Program, diags: &mut Diags) -> Module {
    let mut collector = Collector { diags, next: 0 };
    let mut module = Module::default();
    for item in &program.items {
        if let Some(resolved) = collector.item(item) {
            collector.insert_overload(
                &mut module.items,
                Some(&mut module.hooks),
                &item.name,
                resolved,
            );
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
        assert!(matches!(
            members.get("member").map(|m| &m.kind),
            Some(ResKind::Field)
        ));
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

    #[test]
    fn hook_overloads_with_distinct_sigs_pass() {
        let (module, diags) = resolve_src(
            "@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 => @iadd(lhs, rhs)\n@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 => @iadd(lhs, rhs)\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(module.hooks.get("@add").map(Vec::len), Some(2));
    }

    #[test]
    fn identical_hook_sigs_emit_e020() {
        let (_, diags) = resolve_src(
            "@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 => @iadd(lhs, rhs)\n@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 => @iadd(lhs, rhs)\n",
        );
        assert_eq!(e020_count(&diags), 1);
    }
}
