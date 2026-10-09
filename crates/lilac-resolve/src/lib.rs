//! Name resolution: the definition table, namespaces, and per-file scopes.
//!
//! Every declaration in the compilation unit becomes one [`Def`] in a single
//! table indexed by [`DefId`]: top-level items, object/enum members (fields,
//! methods, member hooks), and enum variants. Later passes look definitions up
//! by id instead of walking the AST, and each `Def` carries the AST it needs
//! (a function's declaration, a field's type, a constant's initializer).
//!
//! Each file gets a [`FileScope`]: the names it can see, already filtered by
//! visibility and namespace reach (spec §6). Hooks (`@name`) are not scoped:
//! they live in the unit-global [`Resolved::hooks`] list and typeck resolves
//! them by signature. Duplicate *names* are `E020` here; duplicate hook
//! *signatures* are `E020` in typeck, where types are known.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use lilac_ast::{Expr, FuncDecl, Item, ItemKind, Program, Type as AstType};
use lilac_error::{Diagnostic, Diags};
use lilac_module::Project;
use lilac_span::{FileId, Span};

/// Identity of one namespace (`NsId(0)` is the root).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NsId(pub u32);

/// Identity of one definition: an index into [`Resolved::defs`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct DefId(pub u32);

impl DefId {
    /// Placeholder for error-recovery poison.
    pub const ERROR: Self = Self(u32::MAX);
}

/// Declared visibility of a definition.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    /// Visible to importers (and, by namespace reach, to descendants).
    Public,
    /// Top level: excluded from `using`. Member: inaccessible outside the
    /// owning type (`E026`).
    Private,
}

/// What a definition is, with the AST later passes need.
#[derive(Clone, Debug)]
pub enum DefKind {
    /// `name: function ...` (also `extern` and `=>` functions, and methods).
    Func(Rc<FuncDecl>),
    /// `@name: function ...` (operator, conversion, or RAII hook).
    Hook(Rc<FuncDecl>),
    /// `name: object { ... }`.
    Object,
    /// `name: enum { ... }`.
    Enum,
    /// `member: Type` inside an object.
    Field(AstType),
    /// `Variant` / `Variant: Type` inside an enum.
    Variant(Option<AstType>),
    /// `name: Type = expr` / `name: Type => expr` module constant.
    Const {
        /// Declared type.
        ty: AstType,
        /// Initializer (inlined at every use).
        value: Rc<Expr>,
    },
}

/// One definition.
#[derive(Clone, Debug)]
pub struct Def {
    /// Local (unqualified) declared name (`@add` for hooks).
    pub name: String,
    /// What was defined.
    pub kind: DefKind,
    /// Owning object or enum, for members and variants.
    pub parent: Option<DefId>,
    /// Declaring namespace.
    pub ns: NsId,
    /// Declaring file.
    pub file: FileId,
    /// Whole-declaration span.
    pub span: Span,
    /// Span of the declared name token.
    pub name_span: Span,
    /// Declared visibility.
    pub vis: Visibility,
    /// The definition's own generic parameters (`T` in `Box[T]`, `f[T]`).
    pub generic_params: Vec<String>,
    /// Object/enum members by name: fields, methods, and variants (not hooks).
    pub members: HashMap<String, DefId>,
    /// Object/enum member hooks, in declaration order.
    pub hooks: Vec<DefId>,
    /// Object fields, or enum variants, in declaration order.
    pub slots: Vec<DefId>,
}

impl Def {
    /// The function declaration of a function or hook.
    #[must_use]
    pub fn func(&self) -> Option<&Rc<FuncDecl>> {
        match &self.kind {
            DefKind::Func(decl) | DefKind::Hook(decl) => Some(decl),
            _ => None,
        }
    }

    /// Whether this is an object or an enum.
    #[must_use]
    pub fn is_type(&self) -> bool {
        matches!(self.kind, DefKind::Object | DefKind::Enum)
    }
}

/// The result of resolving one name in a file.
#[derive(Clone, Debug)]
pub enum Lookup {
    /// Exactly one visible binding.
    One(DefId),
    /// Import-vs-import collision (`E029` on use); the spans are the competing
    /// `using` directives.
    Ambiguous(Vec<Span>),
}

/// One namespace: a folder.
#[derive(Clone, Debug)]
pub struct Namespace {
    /// Dotted path from the root (`""` for the root).
    pub path: String,
    /// Nearest loaded ancestor (`None` for the root).
    pub parent: Option<NsId>,
    /// Top-level non-hook definitions by name.
    pub items: HashMap<String, DefId>,
}

/// One file's namespace and the names it can see.
#[derive(Clone, Debug)]
pub struct FileScope {
    /// The file.
    pub file: FileId,
    /// Its namespace.
    pub ns: NsId,
    /// Visible names, already filtered by visibility and reach.
    pub visible: HashMap<String, Lookup>,
}

/// A resolved compilation unit.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    /// Every definition, indexed by [`DefId`].
    pub defs: Vec<Def>,
    /// Every namespace, indexed by [`NsId`].
    pub namespaces: Vec<Namespace>,
    /// One scope per file.
    pub files: HashMap<FileId, FileScope>,
    /// Every hook definition (top-level and member), in declaration order.
    pub hooks: Vec<DefId>,
}

impl Resolved {
    /// The definition `id` names.
    #[must_use]
    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.0 as usize]
    }

    /// Every definition id, in declaration order.
    pub fn ids(&self) -> impl Iterator<Item = DefId> {
        (0..self.defs.len()).map(|index| DefId(u32::try_from(index).unwrap_or(u32::MAX)))
    }

    /// The binding `name` denotes in `file`, if any.
    #[must_use]
    pub fn lookup(&self, file: FileId, name: &str) -> Option<&Lookup> {
        self.files.get(&file)?.visible.get(name)
    }

    /// The public top-level item a qualified name (`@builtin.Target.os`)
    /// denotes: the namespace path, then the item name.
    #[must_use]
    pub fn lookup_path(&self, path: &str) -> Option<DefId> {
        let (ns, name) = path.rsplit_once('.')?;
        let ns = self.namespaces.iter().find(|data| data.path == ns)?;
        let id = *ns.items.get(name)?;
        (self.def(id).vis == Visibility::Public).then_some(id)
    }

    /// The dotted path of `ns` (`""` for the root).
    #[must_use]
    pub fn namespace_path(&self, ns: NsId) -> &str {
        self.namespaces
            .get(ns.0 as usize)
            .map_or("", |data| data.path.as_str())
    }

    /// The fully qualified source name: namespace path, owning types, then the
    /// local name (`A.B.Object.method`; root items stay bare).
    #[must_use]
    pub fn qualified_name(&self, id: DefId) -> String {
        let def = self.def(id);
        let prefix = match def.parent {
            Some(parent) => self.qualified_name(parent),
            None => self.namespace_path(def.ns).to_owned(),
        };
        if prefix.is_empty() {
            def.name.clone()
        } else {
            format!("{prefix}.{}", def.name)
        }
    }

    /// A member of type `owner` by name (fields, methods, variants).
    #[must_use]
    pub fn member(&self, owner: DefId, name: &str) -> Option<DefId> {
        self.def(owner).members.get(name).copied()
    }
}

/// Resolves a loaded project. `programs` pairs each file with its AST.
pub fn resolve(project: &Project, programs: &[(FileId, Program)], diags: &mut Diags) -> Resolved {
    let mut resolved = Resolved {
        namespaces: project
            .namespaces
            .iter()
            .map(|ns| Namespace {
                path: ns.path.clone(),
                parent: None,
                items: HashMap::new(),
            })
            .collect(),
        ..Resolved::default()
    };
    let by_path: HashMap<String, NsId> = resolved
        .namespaces
        .iter()
        .enumerate()
        .map(|(index, ns)| (ns.path.clone(), ns_id(index)))
        .collect();
    for ns in &mut resolved.namespaces {
        ns.parent = nearest_ancestor(&ns.path, &by_path);
    }

    let asts: HashMap<FileId, &Program> = programs.iter().map(|(file, p)| (*file, p)).collect();
    for unit in &project.files {
        let Some(program) = asts.get(&unit.file) else {
            continue;
        };
        let mut collector = Collector {
            resolved: &mut resolved,
            diags,
            ns: ns_id(unit.ns),
            file: unit.file,
        };
        collector.top_level(&program.items, Visibility::Public);
    }

    for unit in &project.files {
        let ns = ns_id(unit.ns);
        let visible = compute_visible(&resolved, ns, &unit.usings, &unit.using_spans, &by_path);
        resolved.files.insert(
            unit.file,
            FileScope {
                file: unit.file,
                ns,
                visible,
            },
        );
    }
    resolved
}

fn ns_id(index: usize) -> NsId {
    NsId(u32::try_from(index).unwrap_or(u32::MAX))
}

fn duplicate(diags: &mut Diags, name: &str, span: Span, previous: Span) {
    diags.emit(
        Diagnostic::error("E020", format!("duplicate definition of `{name}`"))
            .label(span, true, "redefined here")
            .label(previous, false, "first defined here"),
    );
}

/// The leading `len(name)` bytes of a declaration span (declarations start at
/// their name).
fn leading_name_span(span: Span, name: &str) -> Span {
    let len = u32::try_from(name.len()).unwrap_or(u32::MAX);
    Span::new(span.lo.0, span.lo.0.saturating_add(len), span.file)
}

struct Collector<'a> {
    resolved: &'a mut Resolved,
    diags: &'a mut Diags,
    ns: NsId,
    file: FileId,
}

impl Collector<'_> {
    fn push(&mut self, def: Def) -> DefId {
        let id = DefId(u32::try_from(self.resolved.defs.len()).unwrap_or(u32::MAX));
        if matches!(def.kind, DefKind::Hook(_)) {
            self.resolved.hooks.push(id);
        }
        self.resolved.defs.push(def);
        id
    }

    fn new_def(&self, name: &str, kind: DefKind, span: Span, vis: Visibility) -> Def {
        Def {
            name: name.to_owned(),
            kind,
            parent: None,
            ns: self.ns,
            file: self.file,
            span,
            name_span: leading_name_span(span, name),
            vis,
            generic_params: Vec::new(),
            members: HashMap::new(),
            hooks: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// Collects a file's top level; `private { }` marks its items private
    /// (top-level hooks stay public: `private` never gates hooks).
    fn top_level(&mut self, items: &[Item], vis: Visibility) {
        for item in items {
            if let ItemKind::Private { items } = &item.kind {
                self.top_level(items, Visibility::Private);
                continue;
            }
            let is_hook = item.name.starts_with('@');
            let vis = if is_hook { Visibility::Public } else { vis };
            let Some(id) = self.item(item, vis, None) else {
                continue;
            };
            if is_hook {
                continue;
            }
            let ns = self.ns.0 as usize;
            if let Some(previous) = self.resolved.namespaces[ns].items.get(&item.name) {
                let previous = self.resolved.def(*previous).span;
                duplicate(self.diags, &item.name, item.span, previous);
            } else {
                self.resolved.namespaces[ns]
                    .items
                    .insert(item.name.clone(), id);
            }
        }
    }

    /// Collects one item (and its members); `None` for non-definitions.
    fn item(&mut self, item: &Item, vis: Visibility, parent: Option<DefId>) -> Option<DefId> {
        let kind = match &item.kind {
            ItemKind::Func(decl) if item.name.starts_with('@') => {
                DefKind::Hook(Rc::new(decl.clone()))
            }
            ItemKind::Func(decl) => DefKind::Func(Rc::new(decl.clone())),
            ItemKind::Object { .. } => DefKind::Object,
            ItemKind::Enum { .. } => DefKind::Enum,
            ItemKind::Field { ty } => DefKind::Field(ty.clone()),
            ItemKind::Const { ty, value, .. } => DefKind::Const {
                ty: ty.clone(),
                value: Rc::new(value.clone()),
            },
            ItemKind::Using { .. } | ItemKind::Private { .. } | ItemKind::Error => return None,
        };
        let mut def = self.new_def(&item.name, kind, item.span, vis);
        def.parent = parent;
        def.generic_params = match &item.kind {
            ItemKind::Func(decl) => decl.generic_params.clone(),
            ItemKind::Object { generic_params, .. } | ItemKind::Enum { generic_params, .. } => {
                generic_params.clone()
            }
            _ => Vec::new(),
        };
        let id = self.push(def);
        match &item.kind {
            ItemKind::Object { members, .. } => self.members(id, members, Visibility::Public),
            ItemKind::Enum {
                variants, members, ..
            } => {
                for variant in variants {
                    let mut def = self.new_def(
                        &variant.name,
                        DefKind::Variant(variant.payload.clone()),
                        variant.span,
                        Visibility::Public,
                    );
                    def.name_span = variant.name_span;
                    def.parent = Some(id);
                    let variant_id = self.push(def);
                    self.add_member(id, &variant.name, variant_id, variant.span);
                    self.resolved.defs[id.0 as usize].slots.push(variant_id);
                }
                self.members(id, members, Visibility::Public);
            }
            _ => {}
        }
        Some(id)
    }

    /// Collects a type's members, flattening `private { }` blocks.
    fn members(&mut self, owner: DefId, items: &[Item], vis: Visibility) {
        for item in items {
            if let ItemKind::Private { items } = &item.kind {
                self.members(owner, items, Visibility::Private);
                continue;
            }
            let Some(id) = self.item(item, vis, Some(owner)) else {
                continue;
            };
            match self.resolved.def(id).kind {
                DefKind::Hook(_) => self.resolved.defs[owner.0 as usize].hooks.push(id),
                DefKind::Field(_) => {
                    self.resolved.defs[owner.0 as usize].slots.push(id);
                    self.add_member(owner, &item.name, id, item.span);
                }
                _ => self.add_member(owner, &item.name, id, item.span),
            }
        }
    }

    fn add_member(&mut self, owner: DefId, name: &str, id: DefId, span: Span) {
        if let Some(previous) = self.resolved.def(owner).members.get(name) {
            let previous = self.resolved.def(*previous).span;
            duplicate(self.diags, name, span, previous);
            return;
        }
        self.resolved.defs[owner.0 as usize]
            .members
            .insert(name.to_owned(), id);
    }
}

/// The dotted path of the enclosing folder (`""` for a top-level namespace),
/// or `None` for the root itself.
fn parent_path(path: &str) -> Option<&str> {
    if path.is_empty() {
        return None;
    }
    Some(path.rfind('.').map_or("", |index| &path[..index]))
}

/// The nearest ancestor folder present in the unit; `None` only for the root.
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

/// Builds a file's visible names: its ancestors' and its own namespace's items
/// (any visibility; descendant reach), then each `using`d namespace's public
/// items. Local names shadow imports; two imports of one name are ambiguous.
fn compute_visible(
    resolved: &Resolved,
    ns: NsId,
    usings: &[String],
    using_spans: &[Span],
    by_path: &HashMap<String, NsId>,
) -> HashMap<String, Lookup> {
    let mut visible = HashMap::new();
    let mut chain = vec![ns];
    let mut current = resolved.namespaces[ns.0 as usize].parent;
    while let Some(ancestor) = current {
        if chain.contains(&ancestor) {
            break;
        }
        chain.push(ancestor);
        current = resolved.namespaces[ancestor.0 as usize].parent;
    }
    for ns in chain.iter().rev() {
        for (name, id) in &resolved.namespaces[ns.0 as usize].items {
            visible.insert(name.clone(), Lookup::One(*id));
        }
    }
    let local: HashSet<String> = visible.keys().cloned().collect();

    let mut imported: HashMap<String, Vec<(DefId, Span)>> = HashMap::new();
    let mut seen = HashSet::new();
    for (index, using) in usings.iter().enumerate() {
        let Some(&target) = by_path.get(using.as_str()) else {
            continue;
        };
        if !seen.insert(target) {
            continue;
        }
        let site = using_spans.get(index).copied().unwrap_or_else(Span::dummy);
        for (name, id) in &resolved.namespaces[target.0 as usize].items {
            if resolved.def(*id).vis == Visibility::Public && !local.contains(name) {
                imported.entry(name.clone()).or_default().push((*id, site));
            }
        }
    }
    for (name, mut items) in imported {
        items.sort_by_key(|(id, _)| *id);
        if let [(id, _)] = items.as_slice() {
            visible.insert(name, Lookup::One(*id));
        } else {
            let sites = items.iter().map(|(_, site)| *site).collect();
            visible.insert(name, Lookup::Ambiguous(sites));
        }
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_lex::Lexer;
    use lilac_module::{FileUnit, Namespace as LoadedNs};
    use lilac_parse::Parser;
    use lilac_span::SourceMap;
    use std::path::PathBuf;

    struct Spec {
        ns: usize,
        src: &'static str,
        usings: &'static [&'static str],
    }

    fn resolve_files(paths: &[&str], specs: &[Spec]) -> (Resolved, Vec<FileId>, Diags) {
        let mut sm = SourceMap::new();
        let mut diags = Diags::new();
        let mut namespaces: Vec<LoadedNs> = paths
            .iter()
            .map(|path| LoadedNs {
                path: (*path).to_owned(),
                dir: PathBuf::from(path),
                files: Vec::new(),
            })
            .collect();
        let mut files = Vec::new();
        let mut programs = Vec::new();
        for (index, spec) in specs.iter().enumerate() {
            let name = format!("f{index}.lilac");
            let file = sm.add_file(&name, spec.src);
            let (tokens, _) = Lexer::lex_all(spec.src, file);
            let program = Parser::new(tokens, file)
                .parse_program(&mut diags)
                .expect("parses");
            programs.push((file, program));
            namespaces[spec.ns].files.push(index);
            files.push(FileUnit {
                name: name.clone(),
                path: PathBuf::from(&name),
                file,
                ns: spec.ns,
                src: spec.src.to_owned(),
                usings: spec.usings.iter().map(|u| (*u).to_owned()).collect(),
                using_spans: spec.usings.iter().map(|_| Span::dummy()).collect(),
            });
        }
        let ids = files.iter().map(|f| f.file).collect();
        let project = Project {
            root: PathBuf::from("."),
            namespaces,
            files,
        };
        let resolved = resolve(&project, &programs, &mut diags);
        (resolved, ids, diags)
    }

    fn resolve_one(src: &'static str) -> (Resolved, FileId, Diags) {
        let (resolved, files, diags) = resolve_files(
            &[""],
            &[Spec {
                ns: 0,
                src,
                usings: &[],
            }],
        );
        (resolved, files[0], diags)
    }

    fn e020(diags: &Diags) -> usize {
        diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E020")
            .count()
    }

    fn one(resolved: &Resolved, file: FileId, name: &str) -> DefId {
        match resolved.lookup(file, name) {
            Some(Lookup::One(id)) => *id,
            other => panic!("`{name}`: {other:?}"),
        }
    }

    #[test]
    fn top_level_items_are_visible() {
        let (resolved, file, diags) = resolve_one("main: function () nothing {}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        let main = one(&resolved, file, "main");
        assert_eq!(resolved.def(main).name, "main");
        assert_eq!(resolved.qualified_name(main), "main");
    }

    #[test]
    fn duplicate_names_are_e020() {
        let (_, _, diags) =
            resolve_one("get: function () nothing {}\nget: function () nothing {}\n");
        assert_eq!(e020(&diags), 1);
    }

    #[test]
    fn hooks_are_unit_global_and_unscoped() {
        let (resolved, file, diags) = resolve_one(
            "@add: function (a: @bit8, b: @bit8) r: @bit8 => a\n@add: function (a: @bit32, b: @bit32) r: @bit32 => a\nNum: object {\n    d: @bit8\n    @negate: function (v: Num) r: Num => v\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(resolved.hooks.len(), 3);
        assert!(resolved.lookup(file, "@add").is_none());
        let num = one(&resolved, file, "Num");
        assert_eq!(resolved.def(num).hooks.len(), 1);
        let negate = resolved.def(num).hooks[0];
        assert_eq!(resolved.def(negate).parent, Some(num));
        assert_eq!(resolved.qualified_name(negate), "Num.@negate");
    }

    #[test]
    fn members_fields_and_private() {
        let (resolved, file, diags) = resolve_one(
            "Object: object {\n    a: @bit8\n    private {\n        b: @bit8\n        hidden: function () nothing {}\n    }\n    get: function (self) nothing {}\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let object = one(&resolved, file, "Object");
        let def = resolved.def(object);
        let fields: Vec<_> = def
            .slots
            .iter()
            .map(|id| resolved.def(*id).name.as_str())
            .collect();
        assert_eq!(fields, ["a", "b"]);
        let b = resolved.member(object, "b").expect("b");
        assert_eq!(resolved.def(b).vis, Visibility::Private);
        let hidden = resolved.member(object, "hidden").expect("hidden");
        assert_eq!(resolved.def(hidden).vis, Visibility::Private);
        assert!(resolved.member(object, "get").is_some());
    }

    #[test]
    fn enum_variants_and_members() {
        let (resolved, file, diags) = resolve_one(
            "Opt[T]: enum {\n    Some: T\n    None\n    get: function (self) v: T {}\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let opt = one(&resolved, file, "Opt");
        assert_eq!(resolved.def(opt).generic_params, ["T"]);
        let variants: Vec<_> = resolved
            .def(opt)
            .slots
            .iter()
            .map(|id| resolved.def(*id).name.as_str())
            .collect();
        assert_eq!(variants, ["Some", "None"]);
        assert!(resolved.member(opt, "get").is_some());
    }

    #[test]
    fn variant_and_method_clash_is_e020() {
        let (_, _, diags) = resolve_one("E: enum {\n    A\n    A: function () nothing {}\n}\n");
        assert_eq!(e020(&diags), 1, "{diags:?}");
    }

    #[test]
    fn namespaces_reach_and_privacy() {
        let (resolved, files, diags) = resolve_files(
            &["", "a", "a.b"],
            &[
                Spec {
                    ns: 0,
                    src: "root_fn: function () nothing {}\nprivate {\n    secret: function () nothing {}\n}\n",
                    usings: &["a"],
                },
                Spec {
                    ns: 1,
                    src: "a_fn: function () nothing {}\nprivate {\n    a_secret: function () nothing {}\n}\n",
                    usings: &[],
                },
                Spec {
                    ns: 2,
                    src: "b_fn: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        // The root imports `a`'s public items only.
        assert!(resolved.lookup(files[0], "a_fn").is_some());
        assert!(resolved.lookup(files[0], "a_secret").is_none());
        // A descendant sees its ancestors' items, private ones included.
        assert!(resolved.lookup(files[2], "a_secret").is_some());
        assert!(resolved.lookup(files[2], "secret").is_some());
        // An ancestor does not see its descendants without importing them.
        assert!(resolved.lookup(files[1], "b_fn").is_none());
        let a_fn = one(&resolved, files[0], "a_fn");
        assert_eq!(resolved.qualified_name(a_fn), "a.a_fn");
    }

    #[test]
    fn colliding_imports_are_ambiguous_and_locals_shadow() {
        let (resolved, files, diags) = resolve_files(
            &["", "a", "b"],
            &[
                Spec {
                    ns: 0,
                    src: "local: function () nothing {}\n",
                    usings: &["a", "b"],
                },
                Spec {
                    ns: 1,
                    src: "shared: function () nothing {}\nlocal: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 2,
                    src: "shared: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(matches!(
            resolved.lookup(files[0], "shared"),
            Some(Lookup::Ambiguous(sites)) if sites.len() == 2
        ));
        let local = one(&resolved, files[0], "local");
        assert_eq!(resolved.def(local).ns, NsId(0));
    }

    #[test]
    fn same_namespace_files_merge_and_clash() {
        let (_, _, diags) = resolve_files(
            &[""],
            &[
                Spec {
                    ns: 0,
                    src: "x: function () nothing {}\n",
                    usings: &[],
                },
                Spec {
                    ns: 0,
                    src: "x: function () nothing {}\n",
                    usings: &[],
                },
            ],
        );
        assert_eq!(e020(&diags), 1);
    }

    #[test]
    fn error_items_define_nothing() {
        let mut sm = SourceMap::new();
        let src = "broken: function ( {\n}\nok: function () nothing {}\n";
        let file = sm.add_file("t.lilac", src);
        let (tokens, _) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        let program = Parser::new(tokens, file)
            .parse_program(&mut diags)
            .expect("recovers");
        let project = Project {
            root: PathBuf::from("."),
            namespaces: vec![LoadedNs {
                path: String::new(),
                dir: PathBuf::from("."),
                files: vec![0],
            }],
            files: vec![FileUnit {
                name: "t.lilac".to_owned(),
                path: PathBuf::from("t.lilac"),
                file,
                ns: 0,
                src: src.to_owned(),
                usings: Vec::new(),
                using_spans: Vec::new(),
            }],
        };
        let resolved = resolve(&project, &[(file, program)], &mut diags);
        assert_eq!(resolved.defs.len(), 1);
        assert_eq!(resolved.defs[0].name, "ok");
    }
}
