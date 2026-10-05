//! Signature collection: type names, func/hook tables, conversions.

use std::collections::HashMap;

use lilac_ast::{Item, ItemKind, Program, Type as AstType};
use lilac_error::{Diagnostic, Diags};
use lilac_resolve::{DefId, Module, ResItem, ResKind};
use lilac_ty::{FuncSig, TypeId, TypeRef, Types};

/// Collected signatures for a module.
#[derive(Clone, Debug, Default)]
pub struct Sigs {
    /// Function `DefId` → signature (generic funcs stored as `Param` templates).
    pub funcs: HashMap<DefId, FuncSig>,
    /// Hook name → overload set in definition order.
    pub hooks: HashMap<String, Vec<(DefId, FuncSig)>>,
    /// `@conversion` bindings in definition order.
    pub conversions: Vec<Conversion>,
}

/// One `@conversion(from → to)` binding.
#[derive(Clone, Debug)]
pub struct Conversion {
    /// Source base type.
    pub from: TypeId,
    /// Target base type.
    pub to: TypeId,
    /// Implementing function.
    pub func: DefId,
}

/// Compiler-known `@`-intrinsics (SPEC §3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Intrinsic {
    /// `i/f.add/sub/mul/div/mod`: same-`T` in, `T` out.
    Arith,
    /// `i/f.less/greater_than`: same-`T` in, `@bit8` out.
    Cmp,
    /// `i/f.negate`: `T` in, `T` out.
    Negate,
    /// Parses `@byte_buffer` digits as an integer (folds in M3).
    AsInt,
    /// Parses `@byte_buffer` digits as a float (folds in M3).
    AsFloat,
    /// Size in bytes as `@bit_native`.
    SizeOf,
    /// Narrows; target width comes from the expected type.
    Truncate,
    /// Zero-extends; target width comes from the expected type.
    ExtendZero,
    /// Loads one byte.
    LoadU8,
    /// Stores one byte.
    StoreU8,
    /// Copies bytes.
    Memcpy,
    /// First-byte pointer of a `@byte_buffer`.
    ByteBufferPtr,
    /// Length of a `@byte_buffer`.
    ByteBufferLen,
    /// Borrowing view over pointer plus length.
    ByteBufferFrom,
    /// Empty `@format_args` table.
    FormatArgsNew,
    /// Appends a piece.
    FormatArgsPush,
    /// Piece count.
    FormatArgsCount,
    /// One piece as a borrowed view.
    FormatArgsPiece,
    /// Compile-time error if live after folding (M2 decides liveness).
    CompileError,
}

/// Looks up a compiler-known intrinsic by `@`-name.
pub fn lookup_intrinsic(name: &str) -> Option<Intrinsic> {
    match name {
        "@i_add" | "@i_sub" | "@i_mul" | "@i_div" | "@i_div_u" | "@i_mod" | "@i_mod_u"
        | "@f_add" | "@f_sub" | "@f_mul" | "@f_div" | "@f_mod" => Some(Intrinsic::Arith),
        "@i_less_than" | "@i_less_than_u" | "@i_greater_than" | "@i_greater_than_u"
        | "@f_less_than" | "@f_greater_than" => Some(Intrinsic::Cmp),
        "@i_negate" | "@f_negate" => Some(Intrinsic::Negate),
        "@as_int" => Some(Intrinsic::AsInt),
        "@as_float" => Some(Intrinsic::AsFloat),
        "@size_of" => Some(Intrinsic::SizeOf),
        "@truncate" => Some(Intrinsic::Truncate),
        "@extend_zero" => Some(Intrinsic::ExtendZero),
        "@load_u8" => Some(Intrinsic::LoadU8),
        "@store_u8" => Some(Intrinsic::StoreU8),
        "@memcpy" => Some(Intrinsic::Memcpy),
        "@byte_buffer_ptr" => Some(Intrinsic::ByteBufferPtr),
        "@byte_buffer_len" => Some(Intrinsic::ByteBufferLen),
        "@byte_buffer_from" => Some(Intrinsic::ByteBufferFrom),
        "@format_args_new" => Some(Intrinsic::FormatArgsNew),
        "@format_args_push" => Some(Intrinsic::FormatArgsPush),
        "@format_args_count" => Some(Intrinsic::FormatArgsCount),
        "@format_args_piece" => Some(Intrinsic::FormatArgsPiece),
        "@compile_error" => Some(Intrinsic::CompileError),
        _ => None,
    }
}

/// Resolves a syntactic type to a `TypeRef`.
///
/// A name bound in `params` (the enclosing declaration's generic parameters)
/// resolves to `Param`; `Name[arg…]` resolves to an applied `Instance` after an
/// `E044` arity check (no interning on mismatch); unknown names report `E023`;
/// bare `unknown` (not behind `ref`) reports `E023`. Failures poison to
/// `TypeId::ERROR` so collection continues.
pub fn resolve_type(
    ty: &AstType,
    types: &mut Types,
    module: &Module,
    diags: &mut Diags,
    params: &[String],
) -> TypeRef {
    if let Some(func) = &ty.func {
        let mut param_tys = Vec::with_capacity(func.params.len());
        for param in &func.params {
            param_tys.push(resolve_type(&param.ty, types, module, diags, params));
        }
        let mut return_tys = Vec::with_capacity(func.returns.len());
        for slot in &func.returns {
            return_tys.push(resolve_type(&slot.ty, types, module, diags, params));
        }
        let id = types.intern_func(param_tys, return_tys);
        return TypeRef {
            id,
            is_ref: ty.is_ref,
        };
    }
    if !ty.generic_args.is_empty() {
        return resolve_applied(ty, types, module, diags, params);
    }
    if let Some(index) = params.iter().position(|name| name == &ty.name) {
        let id = types.intern_param(u32::try_from(index).unwrap_or(u32::MAX), &ty.name);
        return TypeRef {
            id,
            is_ref: ty.is_ref,
        };
    }
    if let Some(id) = types.builtin(&ty.name) {
        if ty.name == "unknown" && !ty.is_ref {
            diags.emit(
                Diagnostic::error("E023", "`unknown` can only live behind a reference").label(
                    ty.span,
                    true,
                    "write `ref unknown`".to_owned(),
                ),
            );
            return TypeRef {
                id: TypeId::ERROR,
                is_ref: false,
            };
        }
        return TypeRef {
            id,
            is_ref: ty.is_ref,
        };
    }
    if let Some(item) = module.items.get(&ty.name) {
        match &item.kind {
            ResKind::Object { .. } | ResKind::Enum { .. } => {
                let id = types.intern_object(item.id);
                types.set_object_name(item.id, &ty.name);
                return TypeRef {
                    id,
                    is_ref: ty.is_ref,
                };
            }
            _ => {}
        }
    }
    diags.emit(
        Diagnostic::error("E023", format!("unknown type `{}`", ty.name)).label(
            ty.span,
            true,
            "no such type in scope".to_owned(),
        ),
    );
    TypeRef {
        id: TypeId::ERROR,
        is_ref: false,
    }
}

/// Resolves `Name[arg…]` to an applied `Instance`, enforcing `E044` arity.
///
/// Wrong arity reports `E044` and returns poison **without** interning, so a
/// bogus application never enters the type table (design §3.2). Arguments
/// resolve in the same `params` scope, so `GenObj[ref @bit8]` and
/// `GenObj[GenObj[@bit32]]` work.
fn resolve_applied(
    ty: &AstType,
    types: &mut Types,
    module: &Module,
    diags: &mut Diags,
    params: &[String],
) -> TypeRef {
    let poison = TypeRef {
        id: TypeId::ERROR,
        is_ref: false,
    };
    let found = ty.generic_args.len();
    let Some(item) = module.items.get(&ty.name) else {
        if types.builtin(&ty.name).is_some() {
            emit_arity_error(diags, ty, found, None);
        } else {
            diags.emit(
                Diagnostic::error("E023", format!("unknown type `{}`", ty.name)).label(
                    ty.span,
                    true,
                    "no such type in scope".to_owned(),
                ),
            );
        }
        return poison;
    };
    let decl_params: Vec<String> = match &item.kind {
        ResKind::Object { generic_params, .. } => generic_params.clone(),
        _ => Vec::new(),
    };
    if found != decl_params.len() {
        emit_arity_error(diags, ty, found, Some((item.span, &decl_params)));
        return poison;
    }
    let args = ty
        .generic_args
        .iter()
        .map(|arg| resolve_type(arg, types, module, diags, params))
        .collect();
    types.set_object_name(item.id, &ty.name);
    let id = types.intern_instance(item.id, args);
    TypeRef {
        id,
        is_ref: ty.is_ref,
    }
}

/// Emits `E044` with expected/found; the declaration's parameter list (when
/// known) is labeled with its definition span.
fn emit_arity_error(
    diags: &mut Diags,
    ty: &AstType,
    found: usize,
    decl: Option<(lilac_span::Span, &[String])>,
) {
    let expected = decl.map_or(0, |(_, names)| names.len());
    let noun = if expected == 1 {
        "argument"
    } else {
        "arguments"
    };
    let mut diag = Diagnostic::error(
        "E044",
        format!("wrong number of generic arguments for `{}`", ty.name),
    )
    .label(
        ty.span,
        true,
        format!("expected {expected} generic {noun}, found {found}"),
    );
    match decl {
        Some((span, names)) if !names.is_empty() => {
            diag = diag.label(
                span,
                false,
                format!("declared with `[{}]`", names.join(", ")),
            );
        }
        _ => {
            diag = diag.note(format!("`{}` is not generic", ty.name));
        }
    }
    diags.emit(diag);
}

fn collect_param(
    param: &lilac_ast::Param,
    parent: Option<(&str, DefId, &[String])>,
    params: &[String],
    ctx: &mut SigCtx,
) -> TypeRef {
    if let Some((parent_name, parent_def, parent_params)) = parent {
        if param.name.is_empty() && param.ty.name == "self" && param.ty.generic_args.is_empty() {
            if parent_params.is_empty() {
                let id = ctx.types.intern_object(parent_def);
                ctx.types.set_object_name(parent_def, parent_name);
                return TypeRef { id, is_ref: true };
            }
            let args = parent_params
                .iter()
                .enumerate()
                .map(|(index, name)| {
                    let id = ctx
                        .types
                        .intern_param(u32::try_from(index).unwrap_or(u32::MAX), name);
                    TypeRef { id, is_ref: false }
                })
                .collect();
            let id = ctx.types.intern_instance(parent_def, args);
            ctx.types.set_object_name(parent_def, parent_name);
            return TypeRef { id, is_ref: true };
        }
    }
    resolve_type(&param.ty, ctx.types, ctx.module, ctx.diags, params)
}

struct SigCtx<'a> {
    module: &'a Module,
    types: &'a mut Types,
    diags: &'a mut Diags,
    sigs: &'a mut Sigs,
    warn_op_change: bool,
}

/// Registers a function or member-method signature. Generic declarations are
/// registered too: their slots hold `Param` placeholders until specialized.
fn collect_func(
    name: &str,
    item: &ResItem,
    parent: Option<(&str, DefId, &[String])>,
    ctx: &mut SigCtx,
) {
    let ResKind::Func { generic_params, .. } = &item.kind else {
        lilac_error::bug!(item.span, "collect_func on non-function item");
    };
    register(name, item, parent, generic_params, false, ctx);
}

/// Registers a hook signature. Hooks that declare their own generic parameters
/// are rejected with `E052`; hooks *inside* a generic body are allowed and
/// inherit the parent's parameters as a template.
fn collect_hook_sig(
    name: &str,
    item: &ResItem,
    parent: Option<(&str, DefId, &[String])>,
    ctx: &mut SigCtx,
) {
    let generic = match &item.kind {
        ResKind::Hook { generic, .. } => *generic,
        _ => {
            lilac_error::bug!(item.span, "collect_hook_sig on non-hook item");
        }
    };
    if generic {
        ctx.diags.emit(
            Diagnostic::error(
                "E052",
                "hook bindings must be concrete; generic hooks are rejected",
            )
            .label(item.span, true, "generic parameters here".to_owned()),
        );
        return;
    }
    register(name, item, parent, &[], true, ctx);
}

/// Warns `W020` when a user hook changes an operator's shape: arithmetic
/// stays silent only for all-by-value `@bit` signatures, comparisons only
/// for `(T, T) -> @bit8`.
fn check_op_shape(
    name: &str,
    sig: &FuncSig,
    span: lilac_span::Span,
    types: &Types,
    diags: &mut Diags,
) {
    let arithmetic = matches!(name, "@add" | "@sub" | "@mul" | "@div" | "@mod" | "@negate");
    let comparison = matches!(
        name,
        "@less_than" | "@greater_than" | "@less_than_or_equal" | "@greater_than_or_equal"
    );
    if !arithmetic && !comparison {
        return;
    }
    let is_bit = |id: TypeId| {
        matches!(
            types.kind(id),
            lilac_ty::TypeKind::Bit8
                | lilac_ty::TypeKind::Bit16
                | lilac_ty::TypeKind::Bit32
                | lilac_ty::TypeKind::Bit64
                | lilac_ty::TypeKind::BitNative
        )
    };
    let bit8 = types.builtin("@bit8");
    let silent = if arithmetic {
        sig.params
            .iter()
            .all(|param| !param.is_ref && is_bit(param.id))
            && sig
                .returns
                .iter()
                .all(|slot| !slot.is_ref && is_bit(slot.id))
    } else {
        sig.params.len() == 2
            && !sig.params[0].is_ref
            && !sig.params[1].is_ref
            && sig.params[0].id == sig.params[1].id
            && sig.returns.len() == 1
            && !sig.returns[0].is_ref
            && Some(sig.returns[0].id) == bit8
    };
    if !silent {
        diags.emit(
            Diagnostic::warning("W020", format!("operator `{name}` changes type shape")).label(
                span,
                true,
                "custom shape here".to_owned(),
            ),
        );
    }
}

fn register(
    name: &str,
    item: &ResItem,
    parent: Option<(&str, DefId, &[String])>,
    own_params: &[String],
    is_hook: bool,
    ctx: &mut SigCtx,
) {
    let (ResKind::Func {
        params: decl_params,
        returns: decl_returns,
        ..
    }
    | ResKind::Hook {
        params: decl_params,
        returns: decl_returns,
        ..
    }) = &item.kind
    else {
        lilac_error::bug!(item.span, "register on non-function item");
    };
    // Membership scope: the parent object's parameters first (so `self` uses
    // `Param(0..n)`), then this declaration's own parameters.
    let mut scope: Vec<String> = Vec::with_capacity(own_params.len());
    if let Some((_, _, obj_params)) = parent {
        scope.extend(obj_params.iter().cloned());
    }
    scope.extend(own_params.iter().cloned());
    let mut sig_params = Vec::with_capacity(decl_params.len());
    for param in decl_params {
        sig_params.push(collect_param(param, parent, &scope, ctx));
    }
    let mut sig_returns = Vec::with_capacity(decl_returns.len());
    for param in decl_returns {
        sig_returns.push(collect_param(param, parent, &scope, ctx));
    }
    let sig = FuncSig {
        params: sig_params,
        returns: sig_returns,
    };
    if is_hook {
        ctx.sigs
            .hooks
            .entry(name.to_owned())
            .or_default()
            .push((item.id, sig.clone()));
        if ctx.warn_op_change {
            check_op_shape(name, &sig, item.span, ctx.types, ctx.diags);
        }
        if name == "@conversion" {
            let shaped = sig.params.len() == 1
                && sig.returns.len() == 1
                && !sig.params[0].is_ref
                && !sig.returns[0].is_ref;
            if !shaped {
                ctx.diags.emit(
                    Diagnostic::error(
                        "E022",
                        "`@conversion` needs exactly one by-value parameter and one by-value return",
                    )
                    .label(item.span, true, "bad conversion shape".to_owned()),
                );
                return;
            }
            ctx.sigs.conversions.push(Conversion {
                from: sig.params[0].id,
                to: sig.returns[0].id,
                func: item.id,
            });
        }
    } else {
        ctx.sigs.funcs.insert(item.id, sig);
    }
}

fn collect_item(
    item: &Item,
    resolved: &ResItem,
    parent: Option<(&str, DefId, &[String])>,
    ctx: &mut SigCtx,
) {
    match &item.kind {
        ItemKind::Func(_) => match &resolved.kind {
            ResKind::Hook { .. } => {}
            _ => collect_func(&item.name, resolved, parent, ctx),
        },
        ItemKind::Object {
            members,
            generic_params,
        } => {
            let ResKind::Object {
                members: resolved_members,
                generic_params: resolved_params,
            } = &resolved.kind
            else {
                return;
            };
            let member_parent = Some((item.name.as_str(), resolved.id, resolved_params.as_slice()));
            for member in members {
                if let Some(resolved_member) = resolved_members.get(&member.name) {
                    if matches!(resolved_member.kind, ResKind::Hook { .. }) {
                        collect_hook_sig(&member.name, resolved_member, member_parent, ctx);
                    } else {
                        collect_item(member, resolved_member, member_parent, ctx);
                    }
                }
            }
            if generic_params.is_empty() {
                for member in members {
                    if let Some(resolved_member) = resolved_members.get(&member.name) {
                        validate_raii_hook(member, resolved_member, &item.name, resolved.id, ctx);
                    }
                }
            }
        }
        ItemKind::Enum { .. }
        | ItemKind::Using { .. }
        | ItemKind::Const { .. }
        | ItemKind::Field { .. } => {}
    }
}

/// Validates `@init`/`@on_drop` member-hook shapes at declaration (SPEC §8
/// RAII): factories take plain parameters and return their object, drops
/// take exactly one `ref Self` and return nothing. Generic parents skip
/// validation (M4 owns generics).
fn validate_raii_hook(
    member: &Item,
    resolved_member: &ResItem,
    parent_name: &str,
    parent_def: DefId,
    ctx: &mut SigCtx,
) {
    if member.name != "@init" && member.name != "@on_drop" {
        return;
    }
    let ResKind::Hook { generic: false, .. } = &resolved_member.kind else {
        return;
    };
    let ItemKind::Func(decl) = &member.kind else {
        return;
    };
    let Some(sig) = ctx.sigs.hooks.get(&member.name).and_then(|overloads| {
        overloads
            .iter()
            .find(|(id, _)| *id == resolved_member.id)
            .map(|(_, sig)| sig.clone())
    }) else {
        return;
    };
    if member.name == "@init" {
        for param in &decl.params {
            if param.ty.name == "self" {
                ctx.diags.emit(
                    Diagnostic::error("E022", "`@init` can't have `self` as a parameter").label(
                        param.span,
                        true,
                        "factories take plain parameters".to_owned(),
                    ),
                );
            }
        }
        let parent_id = ctx.types.intern_object(parent_def);
        ctx.types.set_object_name(parent_def, parent_name);
        if !(sig.returns.len() == 1 && sig.returns[0].id == parent_id && !sig.returns[0].is_ref) {
            ctx.diags.emit(
                Diagnostic::error("E022", "`@init` must return exactly its object type").label(
                    member.span,
                    true,
                    "bad `@init` shape".to_owned(),
                ),
            );
        }
    } else if member.name == "@on_drop" {
        let parent_id = ctx.types.intern_object(parent_def);
        ctx.types.set_object_name(parent_def, parent_name);
        let shaped = sig.params.len() == 1
            && sig.params[0].is_ref
            && sig.params[0].id == parent_id
            && sig.returns.is_empty();
        if !shaped {
            ctx.diags.emit(
                Diagnostic::error(
                    "E022",
                    "`@on_drop` needs exactly one `ref Self` parameter and no returns",
                )
                .label(member.span, true, "bad `@on_drop` shape".to_owned()),
            );
        }
    }
}

/// Collects every signature in definition order.
///
/// Generic funcs/hooks are stored as templates (`Param` slots); hooks that
/// declare their own generic parameters report `E052`. `warn_op_change`
/// enables the `W020` operator-shape lint.
pub fn collect_sigs(
    module: &Module,
    program: &Program,
    types: &mut Types,
    diags: &mut Diags,
    warn_op_change: bool,
) -> Sigs {
    let mut sigs = Sigs::default();
    let mut ctx = SigCtx {
        module,
        types,
        diags,
        sigs: &mut sigs,
        warn_op_change,
    };
    for item in &program.items {
        if let Some(resolved) = ctx.module.items.get(&item.name) {
            let resolved = resolved.clone();
            collect_item(item, &resolved, None, &mut ctx);
        }
    }
    let top_hooks: Vec<(String, Vec<ResItem>)> = ctx
        .module
        .hooks
        .iter()
        .map(|(name, overloads)| (name.clone(), overloads.clone()))
        .collect();
    for (name, overloads) in &top_hooks {
        for overload in overloads {
            collect_hook_sig(name, overload, None, &mut ctx);
        }
    }
    sigs
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_error::Diags;
    use lilac_lex::Lexer;
    use lilac_parse::Parser;
    use lilac_resolve::Module;
    use lilac_span::{SourceMap, Span};

    fn sigs_of(src: &str) -> (Sigs, Types, Module, Diags) {
        sigs_of_warn(src, true)
    }

    fn sigs_of_warn(src: &str, warn_op_change: bool) -> (Sigs, Types, Module, Diags) {
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
        let module = lilac_resolve::collect(&program, &mut diags);
        let mut types = Types::new();
        let sigs = collect_sigs(&module, &program, &mut types, &mut diags, warn_op_change);
        (sigs, types, module, diags)
    }

    fn code_count(diags: &Diags, code: &str) -> usize {
        diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == code)
            .count()
    }

    fn named_type(name: &str) -> lilac_ast::Type {
        lilac_ast::Type {
            name: name.to_owned(),
            is_ref: false,
            generic_args: Vec::new(),
            func: None,
            span: Span::dummy(),
        }
    }

    fn applied_type(name: &str, args: Vec<lilac_ast::Type>) -> lilac_ast::Type {
        lilac_ast::Type {
            name: name.to_owned(),
            is_ref: false,
            generic_args: args,
            func: None,
            span: Span::dummy(),
        }
    }

    #[test]
    fn unknown_type_is_e023() {
        let mut types = Types::new();
        let module = Module::default();
        let mut diags = Diags::new();
        let resolved = resolve_type(&named_type("Nope"), &mut types, &module, &mut diags, &[]);
        assert_eq!(resolved.id, TypeId::ERROR);
        assert_eq!(code_count(&diags, "E023"), 1);
        let bare = resolve_type(&named_type("unknown"), &mut types, &module, &mut diags, &[]);
        assert_eq!(bare.id, TypeId::ERROR);
        assert_eq!(code_count(&diags, "E023"), 2);
    }

    #[test]
    fn generic_type_arg_interns_instance() {
        let (_, mut types, module, mut diags) = sigs_of("GenObj[T]: object { value: T }\n");
        let ty = applied_type("GenObj", vec![named_type("@bit32")]);
        let resolved = resolve_type(&ty, &mut types, &module, &mut diags, &[]);
        assert!(!resolved.is_ref);
        assert!(
            matches!(types.kind(resolved.id), lilac_ty::TypeKind::Instance { .. }),
            "expected an instance, got `{}`",
            types.display(resolved.id)
        );
        assert_eq!(types.display(resolved.id), "GenObj[@bit32]");
        assert_eq!(code_count(&diags, "E030"), 0);
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn generic_arg_arity_is_e044() {
        let (_, mut types, module, mut diags) = sigs_of("GenObj[T]: object { value: T }\n");
        let ty = applied_type("GenObj", vec![named_type("@bit8"), named_type("@bit16")]);
        let resolved = resolve_type(&ty, &mut types, &module, &mut diags, &[]);
        assert_eq!(resolved.id, TypeId::ERROR);
        assert_eq!(code_count(&diags, "E044"), 1);
    }

    #[test]
    fn template_sig_uses_param() {
        let mut types = Types::new();
        let module = Module::default();
        let mut diags = Diags::new();
        let params = vec!["T".to_owned()];
        let resolved = resolve_type(&named_type("T"), &mut types, &module, &mut diags, &params);
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(types.display(resolved.id), "T");
        assert!(matches!(
            types.kind(resolved.id),
            lilac_ty::TypeKind::Param { index: 0, .. }
        ));
    }

    #[test]
    fn generic_self_is_ref_instance() {
        let (sigs, types, module, diags) = sigs_of(
            "GenObj[T]: object {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let object = module.items.get("GenObj").expect("has GenObj");
        let lilac_resolve::ResKind::Object { members, .. } = &object.kind else {
            panic!("expected object");
        };
        let get = members.get("get").expect("has get");
        let sig = sigs.funcs.get(&get.id).expect("has get sig");
        assert_eq!(sig.params.len(), 1);
        assert!(sig.params[0].is_ref);
        let lilac_ty::TypeKind::Instance { base, args } = types.kind(sig.params[0].id) else {
            panic!(
                "self should be an instance, got `{}`",
                types.display(sig.params[0].id)
            );
        };
        assert_eq!(*base, object.id);
        assert_eq!(args.len(), 1);
        assert!(matches!(
            types.kind(args[0].id),
            lilac_ty::TypeKind::Param { index: 0, .. }
        ));
        assert!(matches!(
            types.kind(sig.returns[0].id),
            lilac_ty::TypeKind::Param { index: 0, .. }
        ));
    }

    #[test]
    fn generic_object_hook_not_e052() {
        let (_, _, _, diags) = sigs_of(
            "GenObj[T]: object {\n    value: T\n    @on_drop: function (from: ref GenObj[T]) nothing {\n    }\n}\n",
        );
        assert_eq!(code_count(&diags, "E052"), 0);
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn blanket_generic_hook_is_e052() {
        let (_, _, _, diags) =
            sigs_of("@add[T]: function (lhs: T, rhs: T) result: T {\n    result = lhs\n}\n");
        assert_eq!(code_count(&diags, "E052"), 1);
    }

    #[test]
    fn conversion_table_records_from_to() {
        let (sigs, types, _, diags) = sigs_of(
            "@conversion: function (from: @bit32) to: @bit8 {\n    to = @truncate(from)\n}\n",
        );
        assert!(!diags.has_errors());
        assert_eq!(sigs.conversions.len(), 1);
        let conversion = &sigs.conversions[0];
        assert_eq!(types.display(conversion.from), "@bit32");
        assert_eq!(types.display(conversion.to), "@bit8");
    }

    #[test]
    fn self_param_rewrites_to_ref_parent() {
        let (sigs, types, module, diags) = sigs_of(
            "Object: object {\n    get: function (self) result: @bit8 {\n        result = 0x1\n    }\n}\n",
        );
        assert!(!diags.has_errors());
        let object = module.items.get("Object").expect("has Object");
        let lilac_resolve::ResKind::Object { members, .. } = &object.kind else {
            panic!("expected object");
        };
        let get = members.get("get").expect("has get");
        let sig = sigs.funcs.get(&get.id).expect("has sig");
        assert_eq!(sig.params.len(), 1);
        assert!(sig.params[0].is_ref);
        assert_eq!(types.display(sig.params[0].id), "Object");
    }

    #[test]
    fn intrinsics_resolve_by_name() {
        assert!(matches!(lookup_intrinsic("@i_add"), Some(Intrinsic::Arith)));
        assert!(matches!(lookup_intrinsic("@f_add"), Some(Intrinsic::Arith)));
        assert!(matches!(
            lookup_intrinsic("@i_less_than"),
            Some(Intrinsic::Cmp)
        ));
        assert!(matches!(
            lookup_intrinsic("@i_less_than_u"),
            Some(Intrinsic::Cmp)
        ));
        assert!(matches!(
            lookup_intrinsic("@i_greater_than_u"),
            Some(Intrinsic::Cmp)
        ));
        assert!(matches!(
            lookup_intrinsic("@i_div_u"),
            Some(Intrinsic::Arith)
        ));
        assert!(matches!(
            lookup_intrinsic("@i_mod_u"),
            Some(Intrinsic::Arith)
        ));
        assert_eq!(lookup_intrinsic("@nope"), None);
    }

    #[test]
    fn compiler_error_alias_removed() {
        assert_eq!(lookup_intrinsic("@compilerError"), None);
        assert_eq!(
            lookup_intrinsic("@compile_error"),
            Some(Intrinsic::CompileError)
        );
    }

    fn matrix_add_src() -> &'static str {
        "Matrix: object {\n    value: @bit32\n}\n@add: function (lhs: Matrix, rhs: Matrix) result: @bit32 {\n    result = 0x0\n}\n"
    }

    #[test]
    fn op_change_warns_matrix() {
        let (_, _, _, diags) = sigs_of_warn(matrix_add_src(), true);
        assert_eq!(code_count(&diags, "W020"), 1);
        let msgs: Vec<String> = diags.diagnostics().iter().map(|d| d.msg.clone()).collect();
        assert!(msgs.iter().any(|m| m.contains("@add")), "{msgs:?}");
    }

    #[test]
    fn op_change_suppressed_by_flag() {
        let (_, _, _, diags) = sigs_of_warn(matrix_add_src(), false);
        assert_eq!(code_count(&diags, "W020"), 0);
    }

    #[test]
    fn op_bit_mixed_silent() {
        let (_, _, _, diags) = sigs_of_warn(
            "@add: function (lhs: @bit8, rhs: @bit32) result: @bit32 {\n    result = rhs\n}\n",
            true,
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(code_count(&diags, "W020"), 0);
    }

    #[test]
    fn cmp_shape_silent() {
        let (_, _, _, diags) = sigs_of_warn(
            "Matrix: object {\n    value: @bit32\n}\n@less_than: function (lhs: Matrix, rhs: Matrix) result: @bit8 {\n    result = 0x1\n}\n",
            true,
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(code_count(&diags, "W020"), 0);
    }

    #[test]
    fn cmp_shape_change_warns() {
        let (_, _, _, diags) = sigs_of_warn(
            "Matrix: object {\n    value: @bit32\n}\n@less_than: function (lhs: Matrix, rhs: Matrix) result: @bit32 {\n    result = 0x0\n}\n",
            true,
        );
        assert_eq!(code_count(&diags, "W020"), 1);
    }
}
