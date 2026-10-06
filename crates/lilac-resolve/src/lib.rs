//! Name resolution: `DefId`, scopes, folder-modules (real pass lands in M1).

use std::collections::{HashMap, HashSet};

use lilac_ast::{Item, ItemKind, Param, Program, Type as AstType};
use lilac_error::{Diagnostic, Diags};
use lilac_module::Project;
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
            index_defs(item, &mut def_ns);
        }
        for item in module.hooks.values().flatten() {
            index_defs(item, &mut def_ns);
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

/// Records `item` and, recursively, its object members under the namespace
/// they carry in [`ResItem::ns`].
fn index_defs(item: &ResItem, index: &mut HashMap<DefId, NsId>) {
    index.insert(item.id, item.ns);
    if let ResKind::Object { members, .. } = &item.kind {
        for member in members.values() {
            index_defs(member, index);
        }
    }
}

/// The dotted path of the enclosing folder (`""` for a top-level namespace),
/// or `None` for the root itself.
fn parent_path(path: &str) -> Option<&str> {
    if path.is_empty() {
        return None;
    }
    Some(match path.rfind('.') {
        Some(index) => &path[..index],
        None => "",
    })
}

/// The nearest ancestor folder present in the compilation unit; `None` only
/// for the root. Missing intermediate folders are skipped, so a lone `a.b`
/// still reaches the root.
fn nearest_ancestor(path: &str, by_path: &HashMap<String, NsId>) -> Option<NsId> {
    let mut candidate = parent_path(path);
    while let Some(candidate_path) = candidate {
        if let Some(&ns) = by_path.get(candidate_path) {
            return Some(ns);
        }
        candidate = parent_path(candidate_path);
    }
    None
}

/// Signature equality for overload uniqueness (names excluded, types included).
fn same_sig(a: &ResItem, b: &ResItem) -> bool {
    match (&a.kind, &b.kind) {
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
    ns: NsId,
}

impl Collector<'_> {
    fn fresh(&mut self) -> DefId {
        let id = DefId(self.next);
        self.next += 1;
        id
    }

    /// Collects one file's top-level definitions. Flattens top-level `private`
    /// blocks, marking their items [`Visibility::Private`] (top-level hooks
    /// stay public: `private` does not gate them, spec §7.2).
    fn collect_program(&mut self, program: &Program, ns: NsId) -> Module {
        self.ns = ns;
        let mut module = Module::default();
        self.collect_top_level(&program.items, Visibility::Public, &mut module);
        module
    }

    fn collect_top_level(&mut self, items: &[Item], vis: Visibility, module: &mut Module) {
        for item in items {
            if let ItemKind::Private { items } = &item.kind {
                self.collect_top_level(items, Visibility::Private, module);
                continue;
            }
            let Some(mut resolved) = self.item(item) else {
                continue;
            };
            resolved.vis = if matches!(resolved.kind, ResKind::Hook { .. }) {
                Visibility::Public
            } else {
                vis
            };
            let name = item.name.clone();
            self.insert_overload(&mut module.items, Some(&mut module.hooks), &name, resolved);
        }
    }

    /// Collects one object's members, flattening nested `private` blocks and
    /// marking their members [`Visibility::Private`] (spec §5). Object-member
    /// hooks follow member visibility like any other member (spec §7.2).
    fn collect_members(
        &mut self,
        items: &[Item],
        vis: Visibility,
        table: &mut HashMap<String, ResItem>,
    ) {
        for member in items {
            if let ItemKind::Private { items } = &member.kind {
                self.collect_members(items, Visibility::Private, table);
                continue;
            }
            if let Some(mut resolved) = self.item(member) {
                resolved.vis = vis;
                // Member scope tracks no overload sets: same-name members
                // always report E020, hooks included.
                self.insert_overload(table, None, &member.name, resolved);
            }
        }
    }

    /// Adds one hook overload to the unit-global set, rejecting an identical
    /// signature anywhere in the compilation unit (`E020`, spec §7.2).
    fn add_hook(&mut self, hooks: &mut HashMap<String, Vec<ResItem>>, name: &str, item: ResItem) {
        let overloads = hooks.entry(name.to_owned()).or_default();
        if overloads.iter().any(|previous| same_sig(previous, &item)) {
            let previous_span = overloads
                .first()
                .map_or(item.span, |previous| previous.span);
            self.diags.emit(
                Diagnostic::error("E020", format!("duplicate definition of `{name}`"))
                    .label(item.span, true, "redefined here")
                    .label(previous_span, false, "first defined here"),
            );
            return;
        }
        overloads.push(item);
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
                self.collect_members(members, Visibility::Public, &mut table);
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
            ns: self.ns,
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
        if matches!(item.kind, ResKind::Hook { .. }) {
            if let Some(hooks) = hooks {
                let overloads = hooks.entry(name.to_owned()).or_default();
                if overloads.iter().any(|previous| same_sig(previous, &item)) {
                    let previous_span = overloads
                        .first()
                        .map_or(item.span, |previous| previous.span);
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
    let mut collector = Collector {
        diags,
        next: 0,
        ns: NsId(0),
    };
    collector.collect_program(program, NsId(0))
}

/// Collects a whole loaded [`Project`] into a [`ResolvedProject`].
///
/// `programs` pairs each file's [`FileId`] with its parsed [`Program`]. By the
/// loader's contract (`lilac-module`) a file's `FileId` equals its index in
/// `Project::files`, so this maps each program to its declared namespace.
///
/// Each namespace merges its files' top-level items (a cross-file duplicate is
/// `E020`, first binding wins); top-level hooks go into one unit-global overload
/// set (`E020` on an identical signature anywhere). Every file then gets a
/// `visible` scope: its own namespace's items (public and private), its
/// ancestors' items (public by ancestor reach, private by descendant reach),
/// then each `using`d namespace's **public** items — local names shadowing
/// imports, and an import-vs-import collision recorded as [`Lookup::Ambiguous`].
pub fn collect_project(
    project: &Project,
    programs: &[(FileId, Program)],
    diags: &mut Diags,
) -> ResolvedProject {
    let mut by_file: HashMap<usize, &Program> = HashMap::new();
    for (file, program) in programs {
        by_file.insert(file.0 as usize, program);
    }

    let mut namespaces: Vec<NamespaceData> = project
        .namespaces
        .iter()
        .map(|ns| NamespaceData {
            path: ns.path.clone(),
            parent: None,
            items: HashMap::new(),
        })
        .collect();
    let by_path: HashMap<String, NsId> = project
        .namespaces
        .iter()
        .enumerate()
        .map(|(index, ns)| {
            (
                ns.path.clone(),
                NsId(u32::try_from(index).unwrap_or(u32::MAX)),
            )
        })
        .collect();
    for data in &mut namespaces {
        data.parent = nearest_ancestor(&data.path, &by_path);
    }

    let mut collector = Collector {
        diags,
        next: 0,
        ns: NsId(0),
    };
    let mut hooks: HashMap<String, Vec<ResItem>> = HashMap::new();
    for (index, file) in project.files.iter().enumerate() {
        let Some(program) = by_file.get(&index).copied() else {
            continue;
        };
        let ns = NsId(u32::try_from(file.ns).unwrap_or(u32::MAX));
        let module = collector.collect_program(program, ns);
        if let Some(data) = namespaces.get_mut(usize::try_from(ns.0).unwrap_or(usize::MAX)) {
            for (name, item) in module.items {
                if matches!(item.kind, ResKind::Hook { .. }) {
                    // Hook names are overload sets merged unit-globally by
                    // `add_hook` below. The namespace keeps only the first
                    // overload so `namespace_of`/lookup can still find it; a
                    // same-name hook in another file must not be `E020` just
                    // for sharing the namespace.
                    data.items.entry(name).or_insert(item);
                } else {
                    collector.insert(&mut data.items, &name, item);
                }
            }
        }
        for (name, overloads) in module.hooks {
            for item in overloads {
                collector.add_hook(&mut hooks, &name, item);
            }
        }
    }

    let mut def_ns: HashMap<DefId, NsId> = HashMap::new();
    for data in &namespaces {
        for item in data.items.values() {
            index_defs(item, &mut def_ns);
        }
    }
    for item in hooks.values().flatten() {
        index_defs(item, &mut def_ns);
    }

    let mut files = Vec::with_capacity(project.files.len());
    for (index, file) in project.files.iter().enumerate() {
        let ns = NsId(u32::try_from(file.ns).unwrap_or(u32::MAX));
        let visible = compute_visible(&namespaces, ns, &file.usings, &by_path);
        files.push(FileScope {
            file: FileId(u32::try_from(index).unwrap_or(u32::MAX)),
            ns,
            visible,
        });
    }

    ResolvedProject {
        namespaces,
        hooks,
        files,
        def_ns,
    }
}

/// The namespace at `ns`, or `None` for an out-of-range id.
fn namespace_at(namespaces: &[NamespaceData], ns: NsId) -> Option<&NamespaceData> {
    namespaces.get(usize::try_from(ns.0).unwrap_or(usize::MAX))
}

/// Inserts `ns`'s items into `visible`, marking each name as locally bound.
fn add_local(
    namespaces: &[NamespaceData],
    ns: NsId,
    visible: &mut HashMap<String, Lookup>,
    local: &mut HashSet<String>,
) {
    let Some(data) = namespace_at(namespaces, ns) else {
        return;
    };
    for (name, item) in &data.items {
        visible.insert(name.clone(), Lookup::One(item.clone()));
        local.insert(name.clone());
    }
}

/// Builds the per-file `visible` map for a file in `ns` with `usings`.
fn compute_visible(
    namespaces: &[NamespaceData],
    ns: NsId,
    usings: &[String],
    by_path: &HashMap<String, NsId>,
) -> HashMap<String, Lookup> {
    let mut visible: HashMap<String, Lookup> = HashMap::new();
    let mut local: HashSet<String> = HashSet::new();

    let mut ancestors: Vec<NsId> = Vec::new();
    let mut current = namespace_at(namespaces, ns).and_then(|data| data.parent);
    while let Some(ancestor) = current {
        if ancestors.contains(&ancestor) {
            break;
        }
        ancestors.push(ancestor);
        current = namespace_at(namespaces, ancestor).and_then(|data| data.parent);
    }
    ancestors.reverse();
    for ancestor in ancestors {
        add_local(namespaces, ancestor, &mut visible, &mut local);
    }
    add_local(namespaces, ns, &mut visible, &mut local);

    let mut imported: HashMap<String, Vec<ResItem>> = HashMap::new();
    let mut seen: HashSet<NsId> = HashSet::new();
    for using in usings {
        let Some(&target) = by_path.get(using.as_str()) else {
            continue;
        };
        if !seen.insert(target) {
            continue;
        }
        let Some(data) = namespace_at(namespaces, target) else {
            continue;
        };
        for (name, item) in &data.items {
            if item.vis != Visibility::Public || local.contains(name) {
                continue;
            }
            imported.entry(name.clone()).or_default().push(item.clone());
        }
    }
    for items in imported.values_mut() {
        items.sort_by_key(|item| (item.span.file.0, item.span.lo.0));
    }
    for (name, items) in imported {
        if let [item] = items.as_slice() {
            visible.insert(name, Lookup::One(item.clone()));
        } else {
            let spans = items.iter().map(|item| item.span).collect();
            visible.insert(name, Lookup::Ambiguous(spans));
        }
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_error::Diags;
    use lilac_lex::Lexer;
    use lilac_module::{FileUnit, Namespace};
    use lilac_parse::Parser;
    use lilac_span::SourceMap;
    use std::path::PathBuf;

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
    fn object_private_members_recorded() {
        let (module, diags) = resolve_src(
            "Object: object {\n    public_member: @bit8\n    private {\n        secret: @bit8\n        hidden: function () nothing {}\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let object = module.items.get("Object").expect("has Object");
        let ResKind::Object { members, .. } = &object.kind else {
            panic!("expected object, got {:?}", object.kind);
        };
        assert_eq!(
            members.get("public_member").map(|m| m.vis),
            Some(Visibility::Public)
        );
        assert_eq!(
            members.get("secret").map(|m| m.vis),
            Some(Visibility::Private)
        );
        assert_eq!(
            members.get("hidden").map(|m| m.vis),
            Some(Visibility::Private)
        );
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

    struct Spec {
        ns: usize,
        name: &'static str,
        src: &'static str,
        usings: &'static [&'static str],
    }

    fn build_project(ns_paths: &[&str], specs: &[Spec]) -> (Project, Vec<(FileId, Program)>) {
        let mut programs = Vec::new();
        let mut files = Vec::new();
        let mut ns_files: Vec<Vec<usize>> = vec![Vec::new(); ns_paths.len()];
        for (index, spec) in specs.iter().enumerate() {
            let file = FileId(u32::try_from(index).expect("index fits in u32"));
            let (tokens, lex_errors) = Lexer::lex_all(spec.src, file);
            assert!(lex_errors.is_empty(), "{lex_errors:?}");
            let mut parse_diags = Diags::new();
            let mut parser = Parser::new(tokens, file);
            let program = parser
                .parse_program(&mut parse_diags)
                .expect("source parses");
            programs.push((file, program));
            files.push(FileUnit {
                name: spec.name.to_owned(),
                ns: spec.ns,
                src: spec.src.to_owned(),
                usings: spec
                    .usings
                    .iter()
                    .map(|using| (*using).to_owned())
                    .collect(),
            });
            ns_files[spec.ns].push(index);
        }
        let namespaces = ns_paths
            .iter()
            .enumerate()
            .map(|(index, path)| Namespace {
                path: (*path).to_owned(),
                dir: PathBuf::from(path),
                files: ns_files[index].clone(),
            })
            .collect();
        let project = Project {
            root: PathBuf::from("proj"),
            namespaces,
            files,
        };
        (project, programs)
    }

    fn collect_project_src(ns_paths: &[&str], specs: &[Spec]) -> (ResolvedProject, Diags) {
        let (project, programs) = build_project(ns_paths, specs);
        let mut diags = Diags::new();
        let resolved = collect_project(&project, &programs, &mut diags);
        (resolved, diags)
    }

    fn one<'a>(project: &'a ResolvedProject, file: FileId, name: &str) -> &'a ResItem {
        match project.lookup(file, name) {
            Some(Lookup::One(item)) => item,
            other => panic!("expected one binding for `{name}`, got {other:?}"),
        }
    }

    #[test]
    fn namespace_local_names_are_distinct() {
        let (resolved, diags) = collect_project_src(
            &["", "util"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "foo: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 1,
                    name: "util.lilac",
                    src: "foo: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let root_foo = one(&resolved, FileId(0), "foo");
        let util_foo = one(&resolved, FileId(1), "foo");
        assert_eq!(resolved.namespace_of(root_foo.id), Some(NsId(0)));
        assert_eq!(resolved.namespace_of(util_foo.id), Some(NsId(1)));
        assert_eq!(resolved.namespace_path(NsId(0)), "");
        assert_eq!(resolved.namespace_path(NsId(1)), "util");
    }

    #[test]
    fn descendant_sees_ancestor_without_import() {
        let (resolved, diags) = collect_project_src(
            &["", "util"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "foo: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 1,
                    name: "util.lilac",
                    src: "bar: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(
            resolved.namespace_of(one(&resolved, FileId(1), "foo").id),
            Some(NsId(0))
        );
    }

    #[test]
    fn ancestor_does_not_see_descendant() {
        let (resolved, diags) = collect_project_src(
            &["", "util"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "main: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 1,
                    name: "util.lilac",
                    src: "foo: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(resolved.lookup(FileId(0), "foo").is_none());
    }

    #[test]
    fn namespace_private_is_not_importable() {
        let (resolved, diags) = collect_project_src(
            &["", "util"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "using util\n",
                    usings: &["util"],
                },
                Spec {
                    ns: 1,
                    name: "util.lilac",
                    src: "private {\n    secret: function () nothing {}\n}\npublic_fn: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(resolved.lookup(FileId(0), "secret").is_none());
        assert!(resolved.lookup(FileId(0), "public_fn").is_some());
        assert!(resolved.lookup(FileId(1), "secret").is_some());
    }

    #[test]
    fn colliding_imports_are_ambiguous() {
        let (resolved, diags) = collect_project_src(
            &["", "util1", "util2"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "using util1\nusing util2\n",
                    usings: &["util1", "util2"],
                },
                Spec {
                    ns: 1,
                    name: "one.lilac",
                    src: "foo: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 2,
                    name: "two.lilac",
                    src: "foo: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        match resolved.lookup(FileId(0), "foo") {
            Some(Lookup::Ambiguous(spans)) => assert_eq!(spans.len(), 2),
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    #[test]
    fn cross_file_duplicate_emits_e020() {
        let (_, diags) = collect_project_src(
            &[""],
            &[
                Spec {
                    ns: 0,
                    name: "a.lilac",
                    src: "dup: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 0,
                    name: "b.lilac",
                    src: "dup: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert_eq!(e020_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn hooks_are_unit_global_across_namespaces() {
        let (resolved, diags) = collect_project_src(
            &["", "util"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "@h: function (x: @bit8) result: @bit8 => @i_add(x, x)\n",
                    usings: &[],
                },
                Spec {
                    ns: 1,
                    name: "util.lilac",
                    src: "@h: function (x: @bit32) result: @bit32 => @i_add(x, x)\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let overloads = resolved.hooks.get("@h").expect("has @h");
        assert_eq!(overloads.len(), 2);
        let namespaces: HashSet<_> = overloads.iter().map(|item| item.ns).collect();
        assert_eq!(namespaces, HashSet::from([NsId(0), NsId(1)]));
    }

    #[test]
    fn identical_hooks_across_namespaces_emit_e020() {
        let (_, diags) = collect_project_src(
            &["", "util"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "@h: function (x: @bit8) result: @bit8 => @i_add(x, x)\n",
                    usings: &[],
                },
                Spec {
                    ns: 1,
                    name: "util.lilac",
                    src: "@h: function (x: @bit8) result: @bit8 => @i_add(x, x)\n",
                    usings: &[],
                },
            ],
        );
        assert_eq!(e020_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn qualification_covers_members_and_hook_overloads() {
        let (resolved, diags) = collect_project_src(
            &["", "util"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "main: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 1,
                    name: "util.lilac",
                    src: "Obj: object {\n    member: @bit8\n}\n@h: function (x: @bit8) result: @bit8 => @i_add(x, x)\n@h: function (x: @bit32) result: @bit32 => @i_add(x, x)\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let util = NsId(1);
        let object = resolved
            .all_items()
            .find(|(_, item)| item.name == "Obj")
            .map(|(_, item)| item)
            .expect("has Obj");
        assert_eq!(resolved.namespace_of(object.id), Some(util));
        let ResKind::Object { members, .. } = &object.kind else {
            panic!("expected object, got {:?}", object.kind);
        };
        let member = members.get("member").expect("has member");
        assert_eq!(resolved.namespace_of(member.id), Some(util));
        let overloads = resolved.hooks.get("@h").expect("has @h");
        assert_eq!(overloads.len(), 2);
        for overload in overloads {
            assert_eq!(resolved.namespace_of(overload.id), Some(util));
        }
    }

    #[test]
    fn cross_file_distinct_hook_overloads_do_not_collide() {
        let (resolved, diags) = collect_project_src(
            &[""],
            &[
                Spec {
                    ns: 0,
                    name: "a.lilac",
                    src: "@h: function (x: @bit8) result: @bit8 => @i_add(x, x)\n",
                    usings: &[],
                },
                Spec {
                    ns: 0,
                    name: "b.lilac",
                    src: "@h: function (x: @bit32) result: @bit32 => @i_add(x, x)\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(resolved.hooks.get("@h").map(Vec::len), Some(2));
        // The namespace still records the first overload for `namespace_of`.
        let first = one(&resolved, FileId(0), "@h");
        assert_eq!(resolved.namespace_of(first.id), Some(NsId(0)));
    }

    #[test]
    fn cross_file_identical_hooks_emit_e020() {
        let (_, diags) = collect_project_src(
            &[""],
            &[
                Spec {
                    ns: 0,
                    name: "a.lilac",
                    src: "@h: function (x: @bit8) result: @bit8 => @i_add(x, x)\n",
                    usings: &[],
                },
                Spec {
                    ns: 0,
                    name: "b.lilac",
                    src: "@h: function (x: @bit8) result: @bit8 => @i_add(x, x)\n",
                    usings: &[],
                },
            ],
        );
        assert_eq!(e020_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn local_name_shadows_import() {
        let (resolved, diags) = collect_project_src(
            &["", "util"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "using util\nfoo: function () nothing {}\n",
                    usings: &["util"],
                },
                Spec {
                    ns: 1,
                    name: "util.lilac",
                    src: "foo: function () nothing {}\nbar: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let foo = one(&resolved, FileId(0), "foo");
        assert_eq!(resolved.namespace_of(foo.id), Some(NsId(0)));
        let bar = one(&resolved, FileId(0), "bar");
        assert_eq!(resolved.namespace_of(bar.id), Some(NsId(1)));
    }

    #[test]
    fn using_is_not_transitive() {
        let (resolved, diags) = collect_project_src(
            &["", "a", "b", "c"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "main: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 1,
                    name: "a.lilac",
                    src: "using b\n",
                    usings: &["b"],
                },
                Spec {
                    ns: 2,
                    name: "b.lilac",
                    src: "using c\nb_item: function () nothing {}\n",
                    usings: &["c"],
                },
                Spec {
                    ns: 3,
                    name: "c.lilac",
                    src: "c_item: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(resolved.lookup(FileId(1), "b_item").is_some());
        assert!(resolved.lookup(FileId(1), "c_item").is_none());
        assert!(resolved.lookup(FileId(2), "c_item").is_some());
    }

    #[test]
    fn using_is_per_file() {
        let (resolved, diags) = collect_project_src(
            &["", "util1", "util2"],
            &[
                Spec {
                    ns: 0,
                    name: "a.lilac",
                    src: "using util1\n",
                    usings: &["util1"],
                },
                Spec {
                    ns: 0,
                    name: "b.lilac",
                    src: "using util2\n",
                    usings: &["util2"],
                },
                Spec {
                    ns: 1,
                    name: "one.lilac",
                    src: "one: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 2,
                    name: "two.lilac",
                    src: "two: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(resolved.lookup(FileId(0), "one").is_some());
        assert!(resolved.lookup(FileId(0), "two").is_none());
        assert!(resolved.lookup(FileId(1), "two").is_some());
        assert!(resolved.lookup(FileId(1), "one").is_none());
    }

    #[test]
    fn nested_namespace_parent_chain_and_path() {
        let (resolved, diags) = collect_project_src(
            &["", "a", "a.b"],
            &[
                Spec {
                    ns: 0,
                    name: "main.lilac",
                    src: "root_item: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 1,
                    name: "a.lilac",
                    src: "a_item: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 2,
                    name: "deep.lilac",
                    src: "deep_item: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(resolved.namespace_path(NsId(0)), "");
        assert_eq!(resolved.namespace_path(NsId(1)), "a");
        assert_eq!(resolved.namespace_path(NsId(2)), "a.b");
        assert_eq!(resolved.namespaces[0].parent, None);
        assert_eq!(resolved.namespaces[1].parent, Some(NsId(0)));
        assert_eq!(resolved.namespaces[2].parent, Some(NsId(1)));
        // `a.b` reaches both its ancestors (root and `a`) with no import.
        assert!(resolved.lookup(FileId(2), "root_item").is_some());
        assert!(resolved.lookup(FileId(2), "a_item").is_some());
        assert_eq!(
            resolved.namespace_of(one(&resolved, FileId(2), "deep_item").id),
            Some(NsId(2))
        );
    }
}
