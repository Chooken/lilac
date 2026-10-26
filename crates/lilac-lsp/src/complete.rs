//! Completion and signature help.
//!
//! Requests arrive mid-edit, before the next compile, so the context (what
//! precedes the cursor) is read from the document's current text and the
//! names come from the last compile. The cursor is mapped into that
//! compile's text through the edits made since it ([`map_offset`]). After a
//! `.` (a tag or a member) the request waits for a compile of the current
//! text instead ([`needs_current_compile`]): only the compiler knows the
//! enum a tag expects and what a member is looked up in.

use std::collections::{HashMap, HashSet};

use lilac_ast::{FuncType, Param, Type as AstType, TypeShape};
use lilac_driver::Analysis;
use lilac_hir::intrinsics::Intrinsic;
use lilac_hir::{Layout, LocalSite, MemberReceiver};
use lilac_resolve::{DefId, DefKind, Lookup, NsId, Segment, Visibility};
use lilac_span::FileId;
use lilac_ty::{TypeKind, TypeRef};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit, Documentation,
    MarkupContent, MarkupKind, ParameterInformation, ParameterLabel, Position, Range,
    SignatureHelp, SignatureInformation, TextEdit,
};

use crate::context::{After, Completing, Context};
use crate::convert::LineIndex;
use crate::documents::{map_offset, Edit};
use crate::handlers::file_for;
use crate::index::Retained;
use crate::render;

/// What a name or path names.
#[derive(Clone, Copy, Debug)]
enum Named {
    /// A namespace.
    Ns(NsId),
    /// An object or enum, written as a type (`Point.`, `Optional.`).
    Type(DefId),
    /// An object or enum written as a declaration's type (`p: Point.`):
    /// its static functions.
    Constructor(DefId),
    /// A value of a type (a local, a field, a constant).
    Value(TypeRef),
    /// Another item (a function, a variant).
    Item(DefId),
}

/// One compile seen from a cursor in one of its files.
struct Scope<'a> {
    retained: &'a Retained,
    analysis: &'a Analysis,
    file: FileId,
    /// The file's namespace.
    ns: NsId,
    /// The cursor, as an offset into the compile's text of the file.
    at: u32,
}

impl<'a> Scope<'a> {
    fn new(
        retained: &'a Retained,
        uri: &lsp_types::Url,
        text: &str,
        edits: &[Edit],
        cursor: usize,
    ) -> Option<Self> {
        let analysis = retained.session.analysis()?;
        let file = file_for(retained, uri)?;
        let old = &retained.session.source_map.file(file)?.src;
        // The edits connect the compiled text to this one when their
        // lengths add up; when they do not (the compile read the file
        // before the document opened with other text), the whole
        // difference is one edit.
        let grown: isize = edits
            .iter()
            .map(|edit| edit.len.cast_signed() - (edit.end - edit.start).cast_signed())
            .sum();
        let at = if old.len().cast_signed() + grown == text.len().cast_signed() {
            map_offset(edits, cursor)
        } else {
            map_offset(&[Edit::between(old, text, 0)], cursor)
        };
        let at = u32::try_from(at.min(old.len())).ok()?;
        let ns = analysis.resolved.files.get(&file)?.ns;
        Some(Self {
            retained,
            analysis,
            file,
            ns,
            at,
        })
    }

    /// The locals and parameters in scope at the cursor, by name (an inner
    /// declaration shadows an outer one).
    fn locals(&self) -> HashMap<&'a str, LocalSite> {
        let mut found: HashMap<&str, LocalSite> = HashMap::new();
        let source = self.retained.session.source_map.file(self.file);
        for site in &self.retained.index.locals {
            if site.span != site.decl
                || site.span.file != self.file
                || site.decl.hi.0 > self.at
                || !(site.scope.lo.0 <= self.at && self.at < site.scope.hi.0)
            {
                continue;
            }
            let Some(name) = source.and_then(|source| {
                source
                    .src
                    .get(site.span.lo.0 as usize..site.span.hi.0 as usize)
            }) else {
                continue;
            };
            let replace = found.get(name).is_none_or(|known| {
                // The innermost declaration wins; a concrete type over a
                // template's parameter type.
                (known.decl.lo.0, !self.is_param(known.ty))
                    < (site.decl.lo.0, !self.is_param(site.ty))
            });
            if replace {
                found.insert(name, *site);
            }
        }
        found
    }

    fn is_param(&self, ty: TypeRef) -> bool {
        matches!(self.analysis.types.kind(ty.id), TypeKind::Param { .. })
    }

    /// What the dotted path `segments` names.
    fn resolve(&self, segments: &[&str]) -> Option<Named> {
        let (head, rest) = segments.split_first()?;
        let mut named = self.head(head)?;
        for segment in rest {
            named = self.step(named, segment)?;
        }
        Some(named)
    }

    fn head(&self, name: &str) -> Option<Named> {
        if let Some(site) = self.locals().get(name) {
            return Some(Named::Value(site.ty));
        }
        let resolved = &self.analysis.resolved;
        if let Some(id) = self
            .own_type()
            .and_then(|owner| resolved.member(owner, name))
            .filter(|id| resolved.def(*id).is_own_name())
        {
            return Some(self.item(id));
        }
        if let Some(ns) = resolved.alias(self.file, name) {
            return Some(Named::Ns(ns));
        }
        if let Some(Lookup::One(id)) = resolved.lookup(self.file, name) {
            return Some(self.item(*id));
        }
        resolved.namespace_named(self.ns, name).map(Named::Ns)
    }

    fn item(&self, id: DefId) -> Named {
        match &self.analysis.resolved.def(id).kind {
            DefKind::Object | DefKind::Enum => Named::Type(id),
            // An alias of an object or enum has that type's statics.
            DefKind::Alias(_) => {
                let types = &self.analysis.types;
                match types.alias(id).map(|ty| types.kind(ty)) {
                    Some(TypeKind::Object(def) | TypeKind::Instance { base: def, .. }) => {
                        Named::Type(*def)
                    }
                    _ => Named::Item(id),
                }
            }
            DefKind::Const { .. } => self
                .analysis
                .hir
                .consts
                .get(&id)
                .map_or(Named::Item(id), |value| Named::Value(value.ty)),
            _ => Named::Item(id),
        }
    }

    fn step(&self, named: Named, segment: &str) -> Option<Named> {
        let resolved = &self.analysis.resolved;
        match named {
            Named::Ns(ns) => match resolved.ns_member(ns, segment, self.ns)? {
                Segment::Def(id) => Some(self.item(id)),
                Segment::Ns(ns) => Some(Named::Ns(ns)),
            },
            // A constant is a value: `Int.MAX.` lists its type's members.
            Named::Type(def) | Named::Constructor(def) => {
                resolved.member(def, segment).map(|id| self.item(id))
            }
            Named::Value(ty) => {
                if let Some((_, field)) = self
                    .fields(ty)
                    .into_iter()
                    .find(|(name, _)| name == segment)
                {
                    return field.map(Named::Value);
                }
                let def = self.type_def(ty)?;
                resolved.member(def, segment).map(Named::Item)
            }
            Named::Item(_) => None,
        }
    }

    /// The function type `ty` declares in `def`'s declaration: a
    /// `function (…)` as written, or the one an alias it names stands
    /// for (looked up from `def`'s file), for its slot names.
    fn func_syntax<'d>(&'d self, def: DefId, ty: &'d AstType) -> Option<&'d FuncType> {
        let resolved = &self.analysis.resolved;
        match &ty.shape {
            TypeShape::Function(func) => Some(func),
            TypeShape::Named { name, args } if args.is_empty() => {
                let alias = if name.contains('.') {
                    resolved.lookup_path(name)?
                } else {
                    match resolved.lookup(resolved.def(def).file, name)? {
                        Lookup::One(id) => *id,
                        Lookup::Ambiguous(..) => return None,
                    }
                };
                match &resolved.def(alias).kind {
                    DefKind::Alias(AstType {
                        shape: TypeShape::Function(func),
                        ..
                    }) => Some(func),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// The object or enum a value's type is built from.
    fn type_def(&self, ty: TypeRef) -> Option<DefId> {
        match self.analysis.types.kind(ty.id) {
            TypeKind::Object(def) => Some(*def),
            TypeKind::Instance { base, .. } => Some(*base),
            _ => None,
        }
    }

    /// A value's fields with their types when the type was laid out.
    fn fields(&self, ty: TypeRef) -> Vec<(String, Option<TypeRef>)> {
        if let Some(Layout::Object { fields }) = self.analysis.hir.layouts.get(&ty.id) {
            return fields
                .iter()
                .map(|(name, ty)| (name.clone(), Some(*ty)))
                .collect();
        }
        let Some(def) = self.type_def(ty) else {
            return Vec::new();
        };
        let resolved = &self.analysis.resolved;
        resolved
            .def(def)
            .slots
            .iter()
            .filter(|slot| matches!(resolved.def(**slot).kind, DefKind::Field(_)))
            .map(|slot| (resolved.def(*slot).name.clone(), None))
            .collect()
    }

    /// An item's completion entry.
    fn item_entry(&self, id: DefId) -> Option<Entry> {
        let info = self.retained.index.defs.get(&id)?;
        let resolved = &self.analysis.resolved;
        let kind = match &resolved.def(id).kind {
            DefKind::Func(_) if info.parent.is_some() => CompletionItemKind::METHOD,
            DefKind::Func(_) => CompletionItemKind::FUNCTION,
            DefKind::Hook(_) => return None,
            DefKind::Object => CompletionItemKind::STRUCT,
            DefKind::Enum => CompletionItemKind::ENUM,
            DefKind::Alias(_) => CompletionItemKind::TYPE_PARAMETER,
            DefKind::Const { .. } => CompletionItemKind::CONSTANT,
            DefKind::Global { .. } => CompletionItemKind::VARIABLE,
            DefKind::Field(_) => CompletionItemKind::FIELD,
            DefKind::Variant(_) => CompletionItemKind::ENUM_MEMBER,
        };
        Some(Entry {
            label: info.name.clone(),
            kind,
            documentation: resolved.def(id).doc.as_ref().map(|doc| doc.text.clone()),
            detail: Some(render::declaration(resolved, id, 0).text),
        })
    }

    /// The entries after `named.`.
    fn members(&self, named: Named) -> Vec<Entry> {
        let resolved = &self.analysis.resolved;
        match named {
            Named::Ns(ns) => {
                let Some(data) = resolved.namespaces.get(ns.0 as usize) else {
                    return Vec::new();
                };
                let mut entries: Vec<Entry> = data
                    .items
                    .values()
                    .filter(|id| {
                        resolved.def(**id).vis == Visibility::Public
                            || resolved.reaches(self.ns, ns)
                    })
                    .filter_map(|id| self.item_entry(*id))
                    .collect();
                entries.extend(self.child_namespaces(ns));
                entries
            }
            Named::Type(def) => resolved
                .def(def)
                .members
                .values()
                .filter(|id| {
                    matches!(
                        resolved.def(**id).kind,
                        DefKind::Func(_) | DefKind::Variant(_) | DefKind::Const { .. }
                    )
                })
                .filter_map(|id| self.item_entry(*id))
                .collect(),
            Named::Constructor(def) => resolved
                .def(def)
                .members
                .values()
                .filter(|id| match &resolved.def(**id).kind {
                    DefKind::Func(decl) => {
                        decl.params.first().is_none_or(|param| !param.is_receiver())
                    }
                    _ => false,
                })
                .filter_map(|id| self.item_entry(*id))
                .collect(),
            Named::Value(ty) => {
                let types = &self.analysis.types;
                let mut entries: Vec<Entry> = self
                    .fields(ty)
                    .into_iter()
                    .map(|(name, field)| Entry {
                        label: name,
                        kind: CompletionItemKind::FIELD,
                        documentation: None,
                        detail: field.map(|field| types.display_ref(field)),
                    })
                    .collect();
                if let Some(def) = self.type_def(ty) {
                    entries.extend(
                        resolved
                            .def(def)
                            .members
                            .values()
                            .filter(|id| matches!(resolved.def(**id).kind, DefKind::Func(_)))
                            .filter_map(|id| self.item_entry(*id)),
                    );
                }
                entries
            }
            Named::Item(_) => Vec::new(),
        }
    }

    /// The namespaces directly under `ns`, by their last segment.
    fn child_namespaces(&self, ns: NsId) -> Vec<Entry> {
        let resolved = &self.analysis.resolved;
        let parent = resolved.namespace_path(ns);
        resolved
            .namespaces
            .iter()
            .filter(|data| data.parent == Some(ns))
            .filter_map(|data| {
                let name = if parent.is_empty() {
                    data.path.as_str()
                } else {
                    data.path.strip_prefix(parent)?.strip_prefix('.')?
                };
                Some(Entry {
                    label: name.split('.').next()?.to_owned(),
                    kind: CompletionItemKind::MODULE,
                    documentation: None,
                    detail: Some(format!("namespace {}", data.path)),
                })
            })
            .collect()
    }

    /// The object or enum whose body holds the cursor. Types are
    /// top-level only, so there is at most one.
    fn own_type(&self) -> Option<DefId> {
        let resolved = &self.analysis.resolved;
        resolved
            .namespaces
            .get(self.ns.0 as usize)?
            .items
            .values()
            .copied()
            .find(|id| {
                let def = resolved.def(*id);
                def.is_type()
                    && def.file == self.file
                    && def.span.lo.0 <= self.at
                    && self.at < def.span.hi.0
            })
    }

    /// The entries for a bare name: locals, the constants and static
    /// functions of the type the cursor is in, the file's visible items and
    /// `using … as` aliases, the namespaces within reach, keywords and
    /// builtin types.
    fn names(&self) -> Vec<Entry> {
        let types = &self.analysis.types;
        let mut entries: Vec<Entry> = self
            .locals()
            .into_iter()
            .map(|(name, site)| Entry {
                label: name.to_owned(),
                kind: CompletionItemKind::VARIABLE,
                documentation: None,
                detail: Some(types.display_ref(site.ty)),
            })
            .collect();
        if let Some(owner) = self.own_type() {
            let resolved = &self.analysis.resolved;
            entries.extend(
                resolved
                    .def(owner)
                    .members
                    .values()
                    .filter(|id| resolved.def(**id).is_own_name())
                    .filter_map(|id| self.item_entry(*id)),
            );
        }
        if let Some(scope) = self.analysis.resolved.files.get(&self.file) {
            entries.extend(scope.visible.values().filter_map(|lookup| match lookup {
                Lookup::One(id) => self.item_entry(*id),
                Lookup::Ambiguous(_) => None,
            }));
            let resolved = &self.analysis.resolved;
            entries.extend(scope.aliases.iter().map(|(name, (ns, _))| Entry {
                label: name.clone(),
                kind: CompletionItemKind::MODULE,
                documentation: None,
                detail: Some(format!("namespace {}", resolved.namespace_path(*ns))),
            }));
        }
        let mut ns = Some(self.ns);
        while let Some(current) = ns {
            entries.extend(self.child_namespaces(current));
            ns = self
                .analysis
                .resolved
                .namespaces
                .get(current.0 as usize)
                .and_then(|data| data.parent);
        }
        entries.extend(lilac_lex::KEYWORDS.iter().map(|word| Entry {
            label: (*word).to_owned(),
            kind: CompletionItemKind::KEYWORD,
            documentation: None,
            detail: None,
        }));
        entries.extend(builtin_types().filter(|entry| !entry.label.starts_with('@')));
        entries
    }

    /// The names the name operand of the reflection test `test` may give
    /// (`x` in `@has_field(Point, x, …)`): the members of that kind of
    /// the type the path `ty` names, `None` when it names no structure or
    /// either. A generic function is never a test's match, so none is
    /// offered.
    fn tested_members(&self, test: Intrinsic, ty: &str) -> Option<Vec<Entry>> {
        let segments: Vec<&str> = ty.split('.').collect();
        let (Named::Type(def) | Named::Constructor(def)) = self.resolve(&segments)? else {
            return None;
        };
        let resolved = &self.analysis.resolved;
        let tested = |kind: &DefKind| match (test, kind) {
            (Intrinsic::HasField, DefKind::Field(_))
            | (Intrinsic::HasConstant, DefKind::Const { .. })
            | (Intrinsic::HasVariant, DefKind::Variant(_)) => true,
            (Intrinsic::HasMethod | Intrinsic::HasFunction, DefKind::Func(decl)) => {
                let method = decl.params.first().is_some_and(Param::is_receiver);
                decl.generic_params.is_empty() && method == (test == Intrinsic::HasMethod)
            }
            _ => false,
        };
        Some(
            resolved
                .def(def)
                .members
                .values()
                .filter(|id| tested(&resolved.def(**id).kind))
                .filter_map(|id| self.item_entry(*id))
                .collect(),
        )
    }

    /// The entries after a `.`: the members of what the compiler looked
    /// the name at the cursor up in, else the variants of the tag there.
    /// A generic body checked once per instance records a site per check;
    /// only the members they all have are offered.
    fn dot_entries(&self) -> Vec<Entry> {
        let mut receivers: Vec<MemberReceiver> = Vec::new();
        for site in &self.analysis.hir.members {
            if site.span.file == self.file
                && site.span.lo.0 <= self.at
                && self.at <= site.span.hi.0
                && !receivers.contains(&site.receiver)
            {
                receivers.push(site.receiver);
            }
        }
        let mut lists = receivers.into_iter().map(|receiver| {
            self.members(match receiver {
                MemberReceiver::Value(ty) => Named::Value(ty),
                MemberReceiver::Type(def) => Named::Type(def),
                MemberReceiver::Constructor(def) => Named::Constructor(def),
                MemberReceiver::Namespace(ns) => Named::Ns(ns),
            })
        });
        let Some(mut entries) = lists.next() else {
            return self.tag_variants();
        };
        for list in lists {
            let labels: HashSet<String> = list.into_iter().map(|entry| entry.label).collect();
            entries.retain(|entry| labels.contains(&entry.label));
        }
        entries
    }

    /// The variants the tag at the cursor may name (after a bare `.`):
    /// none when the compiler knows no enum is expected there.
    fn tag_variants(&self) -> Vec<Entry> {
        let mut sites = self.analysis.hir.tags.iter().filter(|site| {
            site.span.file == self.file && site.span.lo.0 <= self.at && self.at <= site.span.hi.0
        });
        // A generic body checked more than once records a tag once per
        // check; any that knows its enum answers.
        sites
            .find(|site| !site.variants.is_empty())
            .map(|site| &site.variants)
            .into_iter()
            .flatten()
            .map(|(name, payload)| Entry {
                label: name.clone(),
                kind: CompletionItemKind::ENUM_MEMBER,
                documentation: None,
                detail: payload.clone(),
            })
            .collect()
    }
}

/// One completion before it is placed in the document.
struct Entry {
    label: String,
    kind: CompletionItemKind,
    /// The declaration, on one line.
    detail: Option<String>,
    /// Its doc, in Markdown.
    documentation: Option<String>,
}

fn builtin_types() -> impl Iterator<Item = Entry> {
    let fixed = lilac_ty::Builtin::ALL.into_iter().map(|builtin| Entry {
        label: builtin.name().to_owned(),
        kind: CompletionItemKind::TYPE_PARAMETER,
        documentation: None,
        detail: Some("builtin type".to_owned()),
    });
    let applied = crate::handlers::TYPE_CONSTRUCTORS
        .iter()
        .map(|(name, shape)| Entry {
            label: (*name).to_owned(),
            kind: CompletionItemKind::TYPE_PARAMETER,
            documentation: None,
            detail: Some(format!("builtin type {shape}")),
        });
    fixed.chain(applied)
}

/// Whether a completion at `position` of `text` is after a `.` (a tag or
/// a member, maybe with part of a name): the compiler must check the text
/// as it is now to know the enum the tag expects or what the member is
/// looked up in, so it waits for that compile.
#[must_use]
pub fn needs_current_compile(lines: &LineIndex, position_at: Position) -> bool {
    lines
        .offset(position_at)
        .is_some_and(|cursor| Context::new(lines.text(), cursor).after_dot())
}

/// `textDocument/completion` at `position` of the document `lines`, the
/// text `edits` made from the one `retained` compiled.
#[must_use]
pub fn completion(
    retained: &Retained,
    uri: &lsp_types::Url,
    lines: &LineIndex,
    edits: &[Edit],
    position_at: Position,
) -> Option<CompletionResponse> {
    let text = lines.text();
    let cursor = lines.offset(position_at)?;
    let context = Context::new(text, cursor);
    let Completing { start, after } = context.completing()?;
    let scope = Scope::new(retained, uri, text, edits, cursor)?;
    // A reflection test's name operand, after a type operand.
    let tested = || {
        let (callee, argument) = context.call()?;
        let reflection = Intrinsic::lookup(&callee).filter(|test| {
            argument == 1
                && matches!(
                    test,
                    Intrinsic::HasField
                        | Intrinsic::HasMethod
                        | Intrinsic::HasFunction
                        | Intrinsic::HasConstant
                        | Intrinsic::HasVariant
                )
        })?;
        scope.tested_members(reflection, &context.first_argument()?)
    };
    let entries = if after == After::At {
        let mut entries: Vec<Entry> = Intrinsic::names()
            .into_iter()
            .map(|name| {
                let intrinsic = Intrinsic::lookup(&name);
                Entry {
                    documentation: intrinsic.map(Intrinsic::doc),
                    detail: intrinsic.map(|intrinsic| render::intrinsic(&name, intrinsic).text),
                    label: name,
                    kind: CompletionItemKind::FUNCTION,
                }
            })
            .collect();
        entries.extend(builtin_types().filter(|entry| entry.label.starts_with('@')));
        entries.push(Entry {
            label: "@builtin".to_owned(),
            kind: CompletionItemKind::MODULE,
            documentation: None,
            detail: Some("namespace @builtin".to_owned()),
        });
        entries
    } else if after == After::Dot {
        scope.dot_entries()
    } else if let Some(entries) = tested() {
        entries
    } else {
        scope.names()
    };

    let range = Range::new(lines.position(start), lines.position(cursor));
    let mut seen = HashSet::new();
    let items = entries
        .into_iter()
        .filter(|entry| seen.insert(entry.label.clone()))
        .map(|entry| CompletionItem {
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range,
                new_text: entry.label.clone(),
            })),
            label: entry.label,
            kind: Some(entry.kind),
            detail: entry.detail,
            documentation: entry.documentation.map(markdown),
            ..CompletionItem::default()
        })
        .collect();
    Some(CompletionResponse::Array(items))
}

/// `textDocument/signatureHelp` at `position` of the document `lines`,
/// the text `edits` made from the one `retained` compiled.
#[must_use]
pub fn signature_help(
    retained: &Retained,
    uri: &lsp_types::Url,
    lines: &LineIndex,
    edits: &[Edit],
    position_at: Position,
) -> Option<SignatureHelp> {
    let text = lines.text();
    let cursor = lines.offset(position_at)?;
    let (callee, argument) = Context::new(text, cursor).call()?;
    if let Some(intrinsic) = (!callee.contains('.'))
        .then(|| Intrinsic::lookup(&callee))
        .flatten()
    {
        let rendered = render::intrinsic(&callee, intrinsic);
        return Some(help(rendered, Some(intrinsic.doc()), argument));
    }

    let scope = Scope::new(retained, uri, text, edits, cursor)?;
    let segments: Vec<&str> = callee.split('.').collect();
    let (receiver, method) = match segments.split_last()? {
        (name, []) => (None, *name),
        (name, rest) => (Some(scope.resolve(rest)?), *name),
    };
    let named = match receiver {
        None => scope.head(method)?,
        Some(receiver) => scope.step(receiver, method)?,
    };
    let resolved = &scope.analysis.resolved;
    let doc_of = |def: DefId| resolved.def(def).doc.as_ref().map(|doc| doc.text.clone());
    let types = &scope.analysis.types;
    match named {
        Named::Item(def) if resolved.def(def).func().is_some() => {
            let by_value = matches!(receiver, Some(Named::Value(_)));
            let rendered = render::call(resolved, def, by_value)?;
            Some(help(rendered, doc_of(def), argument))
        }
        // A call through a global function reference (`HANDLER(`).
        Named::Item(def) => {
            let DefKind::Global { ty: syntax, .. } = &resolved.def(def).kind else {
                return None;
            };
            let ty = scope.analysis.hir.globals.get(&def)?.ty;
            let syntax = scope.func_syntax(def, syntax);
            let rendered = render::value_call(types, method, ty, syntax)?;
            Some(help(rendered, doc_of(def), argument))
        }
        // A call through a function-reference field (`self.on_done(`) or
        // local (`callback(`).
        Named::Value(ty) => {
            let field = match receiver {
                Some(Named::Value(owner)) => scope
                    .type_def(owner)
                    .and_then(|owner| resolved.member(owner, method))
                    .filter(|field| matches!(resolved.def(*field).kind, DefKind::Field(_))),
                _ => None,
            };
            let syntax = field.and_then(|field| match &resolved.def(field).kind {
                DefKind::Field(syntax) => scope.func_syntax(field, syntax),
                _ => None,
            });
            let rendered = render::value_call(types, method, ty, syntax)?;
            Some(help(rendered, field.and_then(doc_of), argument))
        }
        Named::Ns(_) | Named::Type(_) | Named::Constructor(_) => None,
    }
}

/// Markdown for an editor.
fn markdown(value: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent {
        kind: MarkupKind::Markdown,
        value,
    })
}

fn help(rendered: render::Rendered, doc: Option<String>, argument: u32) -> SignatureHelp {
    let render::Rendered {
        text: label,
        params,
    } = rendered;
    let count = u32::try_from(params.len()).unwrap_or(u32::MAX);
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: doc.map(markdown),
            parameters: Some(
                params
                    .into_iter()
                    .map(|(start, end)| ParameterInformation {
                        label: ParameterLabel::LabelOffsets([start, end]),
                        documentation: None,
                    })
                    .collect(),
            ),
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(argument.min(count.saturating_sub(1))),
    }
}
