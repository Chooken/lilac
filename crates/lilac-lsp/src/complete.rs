//! Completion and signature help.
//!
//! Requests arrive mid-edit, before the next compile, so the context (what
//! precedes the cursor) is read from the document's current text and the
//! names come from the last compile. The cursor is mapped into that
//! compile's text by the edit between the two ([`map_offset`]). After a
//! `.` (a tag or a member) the request waits for a compile of the current
//! text instead ([`needs_current_compile`]): only the compiler knows the
//! enum a tag expects and what a member is looked up in.

use std::collections::{HashMap, HashSet};

use lilac_driver::Analysis;
use lilac_hir::intrinsics::Intrinsic;
use lilac_hir::{Layout, LocalSite, MemberReceiver};
use lilac_resolve::{DefId, DefKind, Lookup, NsId, Segment, Visibility};
use lilac_span::FileId;
use lilac_ty::{TypeKind, TypeRef};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit, Documentation,
    ParameterInformation, ParameterLabel, Position, Range, SignatureHelp, SignatureInformation,
    TextEdit,
};

use crate::convert::{byte_offset, position};
use crate::handlers::{file_for, intrinsic_signature, signature};
use crate::index::Retained;

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
        cursor: usize,
    ) -> Option<Self> {
        let analysis = retained.session.analysis()?;
        let file = file_for(retained, uri)?;
        let old = &retained.session.source_map.file(file)?.src;
        let at = u32::try_from(map_offset(old, text, cursor)).ok()?;
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
            Named::Type(def) | Named::Constructor(def) => {
                resolved.member(def, segment).map(Named::Item)
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
        let kind = match &self.analysis.resolved.def(id).kind {
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
            detail: signature(self.retained, id, info),
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
                        DefKind::Func(_) | DefKind::Variant(_)
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
                    detail: Some(format!("namespace {}", data.path)),
                })
            })
            .collect()
    }

    /// The entries for a bare name: locals, the file's visible items and
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
                detail: Some(types.display_ref(site.ty)),
            })
            .collect();
        if let Some(scope) = self.analysis.resolved.files.get(&self.file) {
            entries.extend(scope.visible.values().filter_map(|lookup| match lookup {
                Lookup::One(id) => self.item_entry(*id),
                Lookup::Ambiguous(_) => None,
            }));
            let resolved = &self.analysis.resolved;
            entries.extend(scope.aliases.iter().map(|(name, (ns, _))| Entry {
                label: name.clone(),
                kind: CompletionItemKind::MODULE,
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
            detail: None,
        }));
        entries.extend(builtin_types().filter(|entry| !entry.label.starts_with('@')));
        entries
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
                detail: payload.clone(),
            })
            .collect()
    }
}

/// One completion before it is placed in the document.
struct Entry {
    label: String,
    kind: CompletionItemKind,
    detail: Option<String>,
}

fn builtin_types() -> impl Iterator<Item = Entry> {
    lilac_ty::Builtin::ALL.into_iter().map(|builtin| Entry {
        label: builtin.name().to_owned(),
        kind: CompletionItemKind::TYPE_PARAMETER,
        detail: Some("builtin type".to_owned()),
    })
}

/// Maps `byte` in `new` to the matching offset in `old`, the same text
/// before an edit: unchanged before the edit, shifted after it, and the
/// edit's start inside it.
#[must_use]
pub fn map_offset(old: &str, new: &str, byte: usize) -> usize {
    let (old, new) = (old.as_bytes(), new.as_bytes());
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    if byte <= prefix {
        byte
    } else if byte >= new.len() - suffix {
        old.len() - (new.len() - byte)
    } else {
        prefix
    }
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The dotted path ending just before `end` (`a.b`, `@builtin.Target`).
fn path_before(text: &str, end: usize) -> &str {
    let bytes = text.as_bytes();
    let mut start = end;
    while start > 0 && (is_ident(bytes[start - 1]) || matches!(bytes[start - 1], b'.' | b'@')) {
        start -= 1;
    }
    &text[start..end]
}

/// Whether `cursor` is after a `//` on its line.
fn in_comment(text: &str, cursor: usize) -> bool {
    let line_start = text[..cursor].rfind('\n').map_or(0, |newline| newline + 1);
    text[line_start..cursor].contains("//")
}

/// Whether a completion at `position` of `text` is after a `.` (a tag or
/// a member, maybe with part of a name): the compiler must check the text
/// as it is now to know the enum the tag expects or what the member is
/// looked up in, so it waits for that compile.
#[must_use]
pub fn needs_current_compile(text: &str, position_at: Position) -> bool {
    let Some(cursor) = byte_offset(text, position_at) else {
        return false;
    };
    if in_comment(text, cursor) {
        return false;
    }
    let bytes = text.as_bytes();
    let mut start = cursor;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    start > 0 && bytes[start - 1] == b'.'
}

/// `textDocument/completion` at `position` of the document `text`.
#[must_use]
pub fn completion(
    retained: &Retained,
    uri: &lsp_types::Url,
    text: &str,
    position_at: Position,
) -> Option<CompletionResponse> {
    let cursor = byte_offset(text, position_at)?;
    if in_comment(text, cursor) {
        return None;
    }
    let scope = Scope::new(retained, uri, text, cursor)?;
    let bytes = text.as_bytes();
    let mut start = cursor;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    let entries = if start > 0 && bytes[start - 1] == b'@' {
        start -= 1;
        let mut entries: Vec<Entry> = Intrinsic::names()
            .into_iter()
            .map(|name| Entry {
                detail: Intrinsic::lookup(&name)
                    .map(|intrinsic| intrinsic_signature(&name, intrinsic)),
                label: name,
                kind: CompletionItemKind::FUNCTION,
            })
            .collect();
        entries.extend(builtin_types().filter(|entry| entry.label.starts_with('@')));
        entries.push(Entry {
            label: "@builtin".to_owned(),
            kind: CompletionItemKind::MODULE,
            detail: Some("namespace @builtin".to_owned()),
        });
        entries
    } else if start > 0 && bytes[start - 1] == b'.' {
        scope.dot_entries()
    } else {
        scope.names()
    };

    let range = Range::new(position(text, start), position(text, cursor));
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
            ..CompletionItem::default()
        })
        .collect();
    Some(CompletionResponse::Array(items))
}

/// The call the cursor is in: the callee path and the argument index, read
/// back from the cursor to the unmatched `(`.
fn call_at(text: &str, cursor: usize) -> Option<(&str, u32)> {
    let bytes = text.as_bytes();
    let mut depth = 0u32;
    let mut commas = 0u32;
    let mut in_string = false;
    let mut at = cursor;
    while at > 0 {
        at -= 1;
        match bytes[at] {
            b'"' => in_string = !in_string,
            _ if in_string => {}
            b')' | b']' => depth += 1,
            b'(' | b'[' if depth > 0 => depth -= 1,
            b'(' => {
                let callee = path_before(text, at);
                return (!callee.is_empty()).then_some((callee, commas));
            }
            b',' if depth == 0 => commas += 1,
            b'{' | b'}' | b'[' => return None,
            _ => {}
        }
    }
    None
}

/// `textDocument/signatureHelp` at `position` of the document `text`.
#[must_use]
pub fn signature_help(
    retained: &Retained,
    uri: &lsp_types::Url,
    text: &str,
    position_at: Position,
) -> Option<SignatureHelp> {
    let cursor = byte_offset(text, position_at)?;
    let (callee, argument) = call_at(text, cursor)?;
    if let Some(intrinsic) = (!callee.contains('.'))
        .then(|| Intrinsic::lookup(callee))
        .flatten()
    {
        let label = intrinsic_signature(callee, intrinsic);
        let params = label
            .find('(')
            .and_then(|open| Some((open, open + label[open..].find(')')?)))
            .map(|(open, close)| split_params(&label, open + 1, close))
            .unwrap_or_default();
        return Some(help(label, params, argument));
    }

    let scope = Scope::new(retained, uri, text, cursor)?;
    let segments: Vec<&str> = callee.split('.').collect();
    let (receiver, method) = match segments.split_last()? {
        (name, []) => (None, *name),
        (name, rest) => (Some(scope.resolve(rest)?), *name),
    };
    let def = match receiver {
        None => match scope.head(method)? {
            Named::Item(def) => def,
            _ => return None,
        },
        Some(receiver) => match scope.step(receiver, method)? {
            Named::Item(def) => def,
            _ => return None,
        },
    };
    let by_value = matches!(receiver, Some(Named::Value(_)));
    let decl = scope.analysis.resolved.def(def).func()?.clone();
    let sig = scope.analysis.sigs.get(&def)?;
    let types = &scope.analysis.types;
    let info = scope.retained.index.defs.get(&def)?;

    let mut label = format!("{}(", info.qualified);
    let mut params = Vec::new();
    for (index, (param, ty)) in decl.params.iter().zip(&sig.params).enumerate() {
        if index == 0 && param.is_receiver() && by_value {
            continue;
        }
        if !params.is_empty() {
            label.push_str(", ");
        }
        let text = if param.is_ref_self() {
            "self".to_owned()
        } else {
            format!("{}: {}", param.name, types.display_ref(*ty))
        };
        let start = utf16_len(&label);
        label.push_str(&text);
        params.push((start, utf16_len(&label)));
    }
    let returns: Vec<String> = decl
        .returns
        .iter()
        .zip(&sig.returns)
        .map(|(slot, ty)| {
            if slot.name.is_empty() {
                types.display_ref(*ty)
            } else {
                format!("{}: {}", slot.name, types.display_ref(*ty))
            }
        })
        .collect();
    if returns.is_empty() {
        label.push_str(") nothing");
    } else {
        label.push_str(") ");
        label.push_str(&returns.join(", "));
    }
    Some(help(label, params, argument))
}

/// The UTF-16 offsets of the comma-separated parameters of `label` between
/// `open` and `close` (byte offsets of an ASCII label).
fn split_params(label: &str, open: usize, close: usize) -> Vec<(u32, u32)> {
    let mut params = Vec::new();
    let mut start = open;
    for (index, ch) in label[open..close].char_indices() {
        if ch == ',' {
            params.push((start, open + index));
            start = open + index + 2;
        }
    }
    if close > open {
        params.push((start, close));
    }
    params
        .into_iter()
        .map(|(start, end)| (utf16_len(&label[..start]), utf16_len(&label[..end])))
        .collect()
}

fn utf16_len(text: &str) -> u32 {
    u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

fn help(label: String, params: Vec<(u32, u32)>, argument: u32) -> SignatureHelp {
    let count = u32::try_from(params.len()).unwrap_or(u32::MAX);
    SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: None::<Documentation>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_map_across_an_edit() {
        let old = "ab\ncd\nef";
        let new = "ab\ncdXY\nef";
        assert_eq!(map_offset(old, new, 1), 1);
        assert_eq!(map_offset(old, new, 7), 5); // the `\n` after the edit
        assert_eq!(map_offset(old, new, 6), 5); // inside the edit: its start
        assert_eq!(map_offset(old, old, 4), 4);
    }

    #[test]
    fn calls_are_found_back_to_their_paren() {
        assert_eq!(call_at("x = add(1, f(2), ", 17), Some(("add", 2)));
        assert_eq!(call_at("p.sum(", 6), Some(("p.sum", 0)));
        assert_eq!(call_at("f(\"a,(\", ", 9), Some(("f", 1)));
        assert_eq!(call_at("{ x }", 3), None);
    }

    #[test]
    fn intrinsic_parameters_split() {
        let label = "@i_add_8(@bit8, @bit8) @bit8";
        assert_eq!(split_params(label, 9, 21), vec![(9, 14), (16, 21)]);
    }
}
