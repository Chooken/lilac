//! Symbol names: namespace prefixes, mangling, display names, and
//! `extern` link names and their signature checks.

use std::collections::{HashMap, HashSet};

use lilac_error::{codes, Diagnostic, Diags};
use lilac_hir::{FuncKey, HirFunc, HirProgram, LocalId};
use lilac_ir::{LirModule, LirTy, Symbol};
use lilac_resolve::{DefKind, Resolved};
use lilac_span::{SourceMap, Span};
use lilac_target::Target;
use lilac_ty::{TypeId, TypeKind, TypeRef, Types};

use crate::Lower;

/// The symbol an `extern` links under: its `extern "name"` link name, else
/// its declared name (never namespace-qualified).
pub(crate) fn extern_symbol(res: &Resolved, def: lilac_resolve::DefId) -> String {
    let def = res.def(def);
    def.func()
        .and_then(|decl| decl.link_name.clone())
        .unwrap_or_else(|| def.name.clone())
}

/// Diagnoses `extern` and `export` signatures the C ABI cannot carry
/// (`E028`): parameters and returns must be bit integers, floats, `ref T`,
/// or objects (a C struct by value, or an object wrapping exactly one
/// scalar field, which passes as that field), never an array, an empty
/// type, or an aggregate on a `target` that passes none
/// ([`lilac_target::Target::passes_c_aggregates`]); at most one return;
/// never linked as `main`. Two externs with one link name must declare the
/// same C signature.
pub fn check_extern_signatures(
    hir: &HirProgram,
    res: &Resolved,
    types: &Types,
    target: &Target,
    diags: &mut Diags,
) {
    let sources = SourceMap::new();
    let lower = Lower {
        hir,
        res,
        types,
        sources: &sources,
        names: HashMap::new(),
        bad_indices: Vec::new(),
        bad_panics: Vec::new(),
        bad_globals: Vec::new(),
        target: *target,
        module: LirModule::new(*target),
        symbols: HashMap::new(),
        externs: HashSet::new(),
        drop_glue: HashMap::new(),
        copy_glue: HashMap::new(),
        needs_drop: HashMap::new(),
        needs_copy: HashMap::new(),
        type_names: HashMap::new(),
        funcs: HashMap::new(),
    };
    check_link_names(&lower, diags);
    check_c_function_types(&lower, *target, diags);
    for func in &hir.funcs {
        let is_export = res
            .def(func.key.def)
            .func()
            .is_some_and(|decl| decl.is_export);
        if func.body.is_some() && !is_export {
            continue;
        }
        let kind = if is_export { "export" } else { "extern" };
        let name = &extern_symbol(res, func.key.def);
        // An export named `main` is `E055`.
        if name == "main" && !is_export {
            diags.emit(
                Diagnostic::error(
                    codes::E028,
                    "`extern` function `main` conflicts with the program entry point",
                )
                .label(func.span, true, "`main` is reserved for the entry point")
                .help("rename the `extern`; an `extern` symbol is unmangled and cannot be `main`"),
            );
        }
        let mut anonymous = 0;
        let mut place = |id: LocalId, prefix: &str| {
            let local = func.local(id);
            if local.name.is_empty() {
                anonymous += 1;
                format!("{prefix}.{anonymous}")
            } else {
                local.name.clone()
            }
        };
        for id in &func.params {
            let local = func.local(*id);
            if let Some((label, help)) = c_slot_problem(&lower, local.ty, *target) {
                let ty = lower.abi_ty(local.ty).0;
                let place = place(*id, "arg");
                diags.emit(
                    Diagnostic::error(
                        codes::E028,
                        format!("`{kind}` parameter `{place}` has unsupported C type `{ty}`"),
                    )
                    .label(local.span, true, label)
                    .help(help),
                );
            }
        }
        if func.returns.len() > 1 {
            let slot = func.local(func.returns[1]).span;
            diags.emit(
                Diagnostic::error(
                    codes::E028,
                    format!(
                        "`{kind}` function `{name}` declares {} out places; at most one is allowed",
                        func.returns.len()
                    ),
                )
                .label(slot, true, "extra out place")
                .help(format!("an `{kind}` may return at most one C value")),
            );
        } else if let Some(id) = func.returns.first() {
            let local = func.local(*id);
            if let Some((label, help)) = c_slot_problem(&lower, local.ty, *target) {
                let ty = lower.abi_ty(local.ty).0;
                let place = place(*id, "out");
                diags.emit(
                    Diagnostic::error(
                        codes::E028,
                        format!("`{kind}` return `{place}` has unsupported C type `{ty}`"),
                    )
                    .label(local.span, true, label)
                    .help(help),
                );
            }
        }
    }
}

/// Why a slot of type `ty` cannot cross the C ABI on `target`, as a label
/// and a help line, or `None` when it can.
fn c_slot_problem(
    lower: &Lower<'_>,
    ty: TypeRef,
    target: Target,
) -> Option<(&'static str, String)> {
    let lir = lower.abi_ty(ty).0;
    if matches!(lir, LirTy::Array(..)) {
        return Some((
            "an array",
            "C passes no array by value; pass a `ref` to it, or wrap it in a structure".to_owned(),
        ));
    }
    if !lilac_ir::is_c_abi_type(&lir) {
        return Some((
            "not a C ABI type",
            "C slots must be a bit integer, a float, `ref T` or a structure".to_owned(),
        ));
    }
    if lir.is_scalar() {
        return None;
    }
    if lower.is_empty_type(ty) {
        return Some((
            "an empty type",
            "C has no empty type; leave the slot out".to_owned(),
        ));
    }
    if !target.passes_c_aggregates() {
        return Some((
            "an aggregate by value",
            format!("C on `{target}` takes no aggregate by value here; pass a `ref` to it"),
        ));
    }
    None
}

impl Lower<'_> {
    /// Whether a value of `ty` holds no bytes: an object of empty fields
    /// (or none), or an array of none or of empty elements. C has no such
    /// type, so it cannot cross the C ABI.
    fn is_empty_type(&self, ty: TypeRef) -> bool {
        if ty.is_ref {
            return false;
        }
        match self.hir.layouts.get(&ty.id) {
            Some(lilac_hir::Layout::Object { fields }) => {
                fields.iter().all(|(_, field)| self.is_empty_type(*field))
            }
            Some(lilac_hir::Layout::Enum { .. }) => false,
            None => match self.types.kind(ty.id) {
                TypeKind::Array { elem, len } => {
                    *len == 0 || self.is_empty_type(TypeRef::value(*elem))
                }
                _ => false,
            },
        }
    }
}

/// `E028` for an `export function` type C cannot carry: each slot must be
/// a C type, with at most one result. Reported once per type, at the
/// first local of it.
fn check_c_function_types(lower: &Lower<'_>, target: Target, diags: &mut Diags) {
    let mut seen: HashSet<TypeId> = HashSet::new();
    for func in &lower.hir.funcs {
        for local in &func.locals {
            let TypeKind::Func {
                params,
                returns,
                c_abi: true,
                ..
            } = lower.types.kind(local.ty.id)
            else {
                continue;
            };
            if !seen.insert(local.ty.id) {
                continue;
            }
            let name = lower.types.display(local.ty.id);
            let bad = params.iter().chain(returns).find_map(|slot| {
                Some((lower.abi_ty(*slot).0, c_slot_problem(lower, *slot, target)?))
            });
            let (problem, help) = match (bad, returns.len()) {
                (Some((ty, (_, help))), _) => (format!("has the non-C slot type `{ty}`"), help),
                (None, 0 | 1) => continue,
                (None, count) => (
                    format!("returns {count} values; C returns at most one"),
                    "a C function returns at most one value".to_owned(),
                ),
            };
            diags.emit(
                Diagnostic::error(codes::E028, format!("`{name}` {problem}"))
                    .label(local.span, true, "a C function type")
                    .help(help),
            );
        }
    }
}

/// `E028` for two externs that link one symbol with different C signatures.
pub(crate) fn check_link_names(lower: &Lower<'_>, diags: &mut Diags) {
    // Link name -> (C signature, its Lilac spelling, declaration span) of
    // the first extern linking it.
    let mut linked: HashMap<String, (ExternSig, String, Span)> = HashMap::new();
    for func in lower.hir.funcs.iter().filter(|func| func.body.is_none()) {
        let name = extern_symbol(lower.res, func.key.def);
        let variadic = lower
            .res
            .def(func.key.def)
            .func()
            .is_some_and(|decl| decl.variadic);
        let abi = |ids: &[LocalId]| -> Vec<LirTy> {
            ids.iter()
                .map(|id| lower.abi_ty(func.local(*id).ty).0)
                .collect()
        };
        let sig = ExternSig {
            ins: abi(&func.params),
            outs: abi(&func.returns),
            variadic,
        };
        let text = extern_sig_text(func, lower.types, variadic);
        match linked.get(&name) {
            Some((first, first_text, first_span)) if *first != sig => {
                diags.emit(
                    Diagnostic::error(
                        codes::E028,
                        format!("`{name}` is declared with two different signatures"),
                    )
                    .label(func.span, true, format!("declared here as `{text}`"))
                    .label(
                        *first_span,
                        false,
                        format!("first declared as `{first_text}`"),
                    )
                    .help("externs that share a link name must declare the same C signature"),
                );
            }
            Some(_) => {}
            None => {
                linked.insert(name, (sig, text, func.span));
            }
        }
    }
}

/// The symbol prefix of a free function `name` in namespace `path`: the
/// path itself, or for the root namespace `.root` (so `foo` is `.root.foo`
/// and cannot take a C symbol an `extern` links against). Root `main` stays
/// bare: it is the entry point.
pub(crate) fn namespace_prefix(path: &str, name: &str) -> String {
    if !path.is_empty() {
        path.to_owned()
    } else if name == "main" {
        String::new()
    } else {
        ".root".to_owned()
    }
}

/// `ty` as C sees it: any `ref` is the opaque `ref #unknown`.
pub(crate) fn c_ty(ty: LirTy) -> LirTy {
    match ty {
        LirTy::Ref(_) => LirTy::opaque_ref(),
        other => other,
    }
}

/// An `extern`'s C signature, as two declarations of one link name compare.
#[derive(PartialEq)]
pub(crate) struct ExternSig {
    ins: Vec<LirTy>,
    outs: Vec<LirTy>,
    variadic: bool,
}

/// An `extern`'s signature in Lilac types (`(@bit32, ...) @bit32`).
pub(crate) fn extern_sig_text(func: &HirFunc, types: &Types, variadic: bool) -> String {
    let mut ins: Vec<String> = func
        .params
        .iter()
        .map(|id| types.display_ref(func.local(*id).ty))
        .collect();
    if variadic {
        ins.push("...".to_owned());
    }
    let outs: Vec<String> = func
        .returns
        .iter()
        .map(|id| types.display_ref(func.local(*id).ty))
        .collect();
    let outs = if outs.is_empty() {
        "nothing".to_owned()
    } else {
        outs.join(", ")
    };
    format!("({}) {outs}", ins.join(", "))
}

impl Lower<'_> {
    /// The LIR name of a concrete object/enum type (without `#`):
    /// `A.Holder`, or `A.Holder[m]` for an instance ([`Types::mangle`]).
    pub(crate) fn type_name(&self, id: TypeId) -> String {
        match self.type_names.get(&id) {
            Some(name) => name.clone(),
            None => self.types.mangle(id),
        }
    }

    /// A function's name as written in source, with generic arguments:
    /// `Holder[@bit32].make` for the symbol `Holder[m].make`. Stack
    /// traces print it. Each argument is cut at [`lilac_ty::DISPLAY_LIMIT`].
    pub(crate) fn display_of(&self, func: &HirFunc) -> String {
        let def = self.res.def(func.key.def);
        let generics = |args: &[TypeRef]| {
            if args.is_empty() {
                String::new()
            } else {
                let args: Vec<String> = args
                    .iter()
                    .map(|arg| self.types.display_ref(*arg))
                    .collect();
                format!("[{}]", args.join(", "))
            }
        };
        let (owner, own) = match def.parent {
            Some(parent) => {
                let count = self.res.def(parent).generic_params.len();
                let owner = self.res.qualified_name(parent);
                (
                    format!("{owner}{}", generics(&func.key.args[..count])),
                    &func.key.args[count..],
                )
            }
            None if matches!(def.kind, DefKind::Hook(_)) => (String::new(), &func.key.args[..]),
            None => (
                self.res.namespace_path(def.ns).to_owned(),
                &func.key.args[..],
            ),
        };
        let name = if owner.is_empty() {
            def.name.clone()
        } else {
            format!("{owner}.{}", def.name)
        };
        format!("{name}{}", generics(own))
    }

    /// The LIR symbol of a function instance: namespace or owning type
    /// (with its generic arguments), name, own generic arguments, and for
    /// a hook its signature, the types in regions ([`Types::mangle_args`],
    /// [`Types::mangle_signature`]): `Holder[m].make`,
    /// `Box[m].@on_drop(RI3BoxmEE)`.
    pub(crate) fn symbol_of(&self, func: &HirFunc) -> String {
        let def = self.res.def(func.key.def);
        if func.body.is_none() {
            return extern_symbol(self.res, func.key.def);
        }
        let is_hook = matches!(def.kind, DefKind::Hook(_));
        if let (true, None, Some(symbol)) = (
            is_hook,
            def.parent,
            lilac_hir::platform_hook_symbol(&def.name),
        ) {
            return symbol.to_owned();
        }
        let (prefix, own) = match def.parent {
            Some(parent) => {
                let count = self.res.def(parent).generic_params.len();
                let mut owner = self.res.qualified_name(parent);
                if count > 0 {
                    owner.push_str(&self.types.mangle_args(&func.key.args[..count]));
                }
                (owner, &func.key.args[count..])
            }
            None if is_hook => (String::new(), &func.key.args[..]),
            None => (
                namespace_prefix(self.res.namespace_path(def.ns), &def.name),
                &func.key.args[..],
            ),
        };
        let mut name = if prefix.is_empty() {
            def.name.clone()
        } else {
            format!("{prefix}.{}", def.name)
        };
        if !own.is_empty() {
            name.push_str(&self.types.mangle_args(own));
        }
        if is_hook {
            let slots = |ids: &[LocalId]| -> Vec<TypeRef> {
                ids.iter().map(|id| func.local(*id).ty).collect()
            };
            name.push_str(
                &self
                    .types
                    .mangle_signature(&slots(&func.params), &slots(&func.returns)),
            );
        }
        name
    }

    pub(crate) fn symbol(&self, key: &FuncKey) -> Symbol {
        match self.symbols.get(key) {
            Some(symbol) => symbol.clone(),
            None => lilac_error::bug!(Span::dummy(), "call to an unchecked instance {key:?}"),
        }
    }
}
