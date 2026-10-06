//! Name resolution: `DefId`, scopes, folder-modules (real pass lands in M1).

use std::collections::HashMap;

use lilac_ast::{Item, ItemKind, Param, Program, Type as AstType};
use lilac_error::{Diagnostic, Diags};
use lilac_span::{FileId, Span};

/// Identity of one namespace in a [`ResolvedProject`] (index into its
/// `namespaces` list; `NsId(0)` is the root).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NsId(pub u32);

/// Declared visibility of a definition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    /// Visible to importers (and, by namespace reach, to descendants).
    Public,
    /// Visible only within the declaring namespace and its descendants.
    Private,
}

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
    /// Local (unqualified) declared name.
    pub name: String,
    /// What was defined.
    pub kind: ResKind,
    /// Definition span.
    pub span: Span,
    /// Declared visibility.
    pub vis: Visibility,
    /// Owning namespace.
    pub ns: NsId,
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
        /// Declared generic parameter names, in declaration order.
        generic_params: Vec<String>,
        /// Declared parameters (for signature collection).
        params: Vec<Param>,
        /// Declared return slots (for signature collection).
        returns: Vec<Param>,
    },
    /// `@`-named hook (`@add`, `@conversion`, ...).
    Hook {
        /// Whether it declares generic parameters (rejected with `E052`).
        generic: bool,
        /// Declared generic parameter names, in declaration order.
        generic_params: Vec<String>,
        /// Declared parameters (for signature collection).
        params: Vec<Param>,
        /// Declared return slots (for signature collection).
        returns: Vec<Param>,
    },
    /// `name: object { ... }` with collected members.
    Object {
        /// Member name → definition.
        members: HashMap<String, ResItem>,
        /// Declared generic parameter names, in declaration order.
        generic_params: Vec<String>,
    },
    /// `name: enum { ... }` with variant names and payloads in order.
    Enum {
        /// Variants in declaration order.
        variants: Vec<EnumMember>,
    },
    /// `name: Type = expr` module constant.
    Const,
    /// `member: Type` bare object field.
    Field,
}

/// One enum variant: `One` or `Some: @bit32` (payload).
#[derive(Clone, Debug)]
pub struct EnumMember {
    /// Variant name.
    pub name: String,
    /// Payload type (`None` for payload-less variants).
    pub payload: Option<AstType>,
}

/// One namespace unit: top-level names of a file.
#[derive(Clone, Debug, Default)]
pub struct Module {
    /// Top-level name → definition (first binding wins on duplicates).
    pub items: HashMap<String, ResItem>,
    /// Hook name → every overload in definition order.
    pub hooks: HashMap<String, Vec<ResItem>>,
}

/// The result of resolving one name: a single binding, or several equally
/// visible ones (reported as `E029` on use).
#[derive(Clone, Debug)]
pub enum Lookup {
    /// Exactly one visible binding.
    One(ResItem),
    /// Import-vs-import collision; the spans are the competing declarations.
    Ambiguous(Vec<Span>),
}

/// One namespace's own definitions.
#[derive(Clone, Debug)]
pub struct NamespaceData {
    /// Dotted path from the root (`""` for the root).
    pub path: String,
    /// Enclosing namespace (`None` for the root).
    pub parent: Option<NsId>,
    /// Name → definition (first binding wins on duplicates).
    pub items: HashMap<String, ResItem>,
}

/// One source file's namespace and the bindings it can see.
#[derive(Clone, Debug)]
pub struct FileScope {
    /// The file this scope belongs to.
    pub file: FileId,
    /// The namespace the file is declared in.
    pub ns: NsId,
    /// Name → binding, already filtered by visibility and reach.
    pub visible: HashMap<String, Lookup>,
}

/// A whole compilation unit's resolved namespaces and per-file scopes.
#[derive(Clone, Debug)]
pub struct ResolvedProject {
    /// All namespaces; index 0 is the root.
    pub namespaces: Vec<NamespaceData>,
    /// Unit-global top-level hook overload sets.
    pub hooks: HashMap<String, Vec<ResItem>>,
    /// One visible scope per source file.
    pub files: Vec<FileScope>,
    /// Owning namespace of every collected definition (top-level items,
    /// object members, and every hook overload).
    def_ns: HashMap<DefId, NsId>,
}

impl ResolvedProject {
    /// Wraps a single-file [`Module`] as a one-namespace project.
    pub fn single(module: Module, file: FileId) -> Self {
        let visible = module
            .items
            .iter()
            .map(|(name, item)| (name.clone(), Lookup::One(item.clone())))
            .collect();
        let root = NamespaceData {
            path: String::new(),
            parent: None,
            items: module.items,
        };
        let mut def_ns = HashMap::new();
        for item in root.items.values() {
            index_defs(NsId(0), item, &mut def_ns);
        }
        for item in module.hooks.values().flatten() {
            index_defs(NsId(0), item, &mut def_ns);
        }
        Self {
            namespaces: vec![root],
            hooks: module.hooks,
            files: vec![FileScope {
                file,
                ns: NsId(0),
                visible,
            }],
            def_ns,
        }
    }

    /// The binding `name` denotes in `file`, if any.
    pub fn lookup(&self, file: FileId, name: &str) -> Option<&Lookup> {
        self.files
            .iter()
            .find(|scope| scope.file == file)
            .and_then(|scope| scope.visible.get(name))
    }

    /// Every top-level item across the project as `(local name, item)`.
    pub fn all_items(&self) -> impl Iterator<Item = (&str, &ResItem)> {
        self.namespaces
            .iter()
            .flat_map(|ns| ns.items.iter().map(|(name, item)| (name.as_str(), item)))
    }

    /// The namespace `def` was declared in, if the project collected it.
    pub fn namespace_of(&self, def: DefId) -> Option<NsId> {
        self.def_ns.get(&def).copied()
    }

    /// The dotted path of `ns` from the root (`""` for the root namespace).
    pub fn namespace_path(&self, ns: NsId) -> &str {
        let index = usize::try_from(ns.0).unwrap_or(usize::MAX);
        self.namespaces
            .get(index)
            .map_or("", |data| data.path.as_str())
    }
}

/// Records `item` and, recursively, its object members as owned by `ns`.
fn index_defs(ns: NsId, item: &ResItem, index: &mut HashMap<DefId, NsId>) {
    index.insert(item.id, ns);
    if let ResKind::Object { members, .. } = &item.kind {
        for member in members.values() {
            index_defs(ns, member, index);
        }
    }
}

/// Signature key for overload uniqueness (names excluded, types included).
fn ty_key(ty: &AstType) -> String {
    match &ty.func {
        Some(func) => {
            let ps: Vec<String> = func.params.iter().map(|p| ty_key(&p.ty)).collect();
            let rs: Vec<String> = func.returns.iter().map(|r| ty_key(&r.ty)).collect();
            format!("function({})->({})", ps.join(","), rs.join(","))
        }
        None => format!("{}:{}:{:?}", ty.name, ty.is_ref, ty.generic_args),
    }
}

fn sig_key(params: &[Param], returns: &[Param]) -> String {
    let ps: Vec<String> = params.iter().map(|p| ty_key(&p.ty)).collect();
    let rs: Vec<String> = returns.iter().map(|r| ty_key(&r.ty)).collect();
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
                let generic_params = decl.generic_params.clone();
                let generic = !generic_params.is_empty();
                let params = decl.params.clone();
                let returns = decl.returns.clone();
                if item.name.starts_with('@') {
                    ResKind::Hook {
                        generic,
                        generic_params,
                        params,
                        returns,
                    }
                } else {
                    ResKind::Func {
                        is_extern: decl.is_extern,
                        generic,
                        generic_params,
                        params,
                        returns,
                    }
                }
            }
            ItemKind::Object {
                members,
                generic_params,
            } => {
                let mut table = HashMap::new();
                for member in members {
                    if let Some(resolved) = self.item(member) {
                        // Member scope tracks no overload sets: same-name
                        // members always report E020, hooks included.
                        self.insert_overload(&mut table, None, &member.name, resolved);
                    }
                }
                ResKind::Object {
                    members: table,
                    generic_params: generic_params.clone(),
                }
            }
            ItemKind::Enum { variants, .. } => ResKind::Enum {
                variants: variants
                    .iter()
                    .map(|v| EnumMember {
                        name: v.name.clone(),
                        payload: v.payload.clone(),
                    })
                    .collect(),
            },
            ItemKind::Using { .. } | ItemKind::Private { .. } => return None,
            ItemKind::Const { .. } => ResKind::Const,
            ItemKind::Field { .. } => ResKind::Field,
        };
        Some(ResItem {
            id,
            name: item.name.clone(),
            kind,
            span: item.span,
            vis: Visibility::Public,
            ns: NsId(0),
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

    fn resolve_with_file(src: &str) -> (Module, FileId, Diags) {
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
        (module, file, diags)
    }

    fn resolve_src(src: &str) -> (Module, Diags) {
        let (module, _file, diags) = resolve_with_file(src);
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
    fn single_project_lookup() {
        let src = "main: function () nothing {}\n";
        let (module, file, diags) = resolve_with_file(src);
        assert!(!diags.has_errors(), "{diags:?}");
        let def = module.items.get("main").expect("has main").id;
        let project = ResolvedProject::single(module, file);
        let Lookup::One(item) = project.lookup(file, "main").expect("main is visible") else {
            panic!("expected exactly one binding");
        };
        assert_eq!(item.id, def);
        assert_eq!(project.namespace_of(def), Some(NsId(0)));
        assert_eq!(project.namespace_path(NsId(0)), "");
    }

    #[test]
    fn hook_overload_defs_share_root_namespace() {
        let (module, file, diags) = resolve_with_file(
            "@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 => @i_add(lhs, rhs)\n@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 => @i_add(lhs, rhs)\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let overloads = module.hooks.get("@add").expect("has @add");
        assert_eq!(overloads.len(), 2);
        let second = overloads[1].id;
        assert!(
            !module.items.values().any(|item| item.id == second),
            "the non-first overload must not live in `items`"
        );
        let project = ResolvedProject::single(module, file);
        assert_eq!(project.namespace_of(second), Some(NsId(0)));
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
        let ResKind::Object { members, .. } = &object.kind else {
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
        let names: Vec<_> = variants.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["One", "Two"]);
        assert!(variants.iter().all(|v| v.payload.is_none()));
    }

    #[test]
    fn hook_overloads_with_distinct_sigs_pass() {
        let (module, diags) = resolve_src(
            "@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 => @i_add(lhs, rhs)\n@add: function (lhs: @bit32, rhs: @bit32) result: @bit32 => @i_add(lhs, rhs)\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(module.hooks.get("@add").map(Vec::len), Some(2));
    }

    #[test]
    fn identical_hook_sigs_emit_e020() {
        let (_, diags) = resolve_src(
            "@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 => @i_add(lhs, rhs)\n@add: function (lhs: @bit8, rhs: @bit8) result: @bit8 => @i_add(lhs, rhs)\n",
        );
        assert_eq!(e020_count(&diags), 1);
    }

    #[test]
    fn enum_payloads_recorded() {
        let (module, diags) = resolve_src("OptionalBit32: enum {\n    Some: @bit32\n    None\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let number = module.items.get("OptionalBit32").expect("has enum");
        let ResKind::Enum { variants } = &number.kind else {
            panic!("expected enum, got {:?}", number.kind);
        };
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[0].name, "Some");
        assert!(variants[0].payload.is_some());
        assert!(variants[1].payload.is_none());
    }

    #[test]
    fn func_records_generic_params() {
        let (module, diags) = resolve_src("gen[T, U]: function (a: T) r: U {\n    r = a\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let func = module.items.get("gen").expect("has gen");
        let ResKind::Func {
            generic,
            generic_params,
            ..
        } = &func.kind
        else {
            panic!("expected func, got {:?}", func.kind);
        };
        assert!(generic);
        assert_eq!(generic_params, &["T".to_owned(), "U".to_owned()]);
    }

    #[test]
    fn object_records_generic_params() {
        let (module, diags) = resolve_src("GenObj[T]: object {\n    value: T\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let object = module.items.get("GenObj").expect("has GenObj");
        let ResKind::Object { generic_params, .. } = &object.kind else {
            panic!("expected object, got {:?}", object.kind);
        };
        assert_eq!(generic_params, &["T".to_owned()]);
    }
}
