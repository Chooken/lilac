//! Generic instantiation core: monomorphizing generic functions.
//!
//! A generic declaration is a template; the first concrete reference to it
//! specializes the body under a substituted environment and caches the result
//! by `(DefId, Vec<TypeRef>)`. Failed instances poison silently on later
//! references; growing/recursive chains are bounded by [`MAX_DEPTH`] (`E051`).

use std::collections::HashMap;

use lilac_ast::{Expr, ItemKind, Type as AstType};
use lilac_error::{Diagnostic, ErrorGuaranteed};
use lilac_hir::{HirBlock, HirCallee, HirExpr, HirFunc};
use lilac_resolve::{DefId, ResItem, ResKind};
use lilac_span::Span;
use lilac_ty::{FuncSig, TypeId, TypeKind, TypeRef};

use super::{
    find_item, resolve_type, subst_ident, CallOutcome, CheckedBody, CheckedCall, Checker, FuncCtx,
};

/// One monomorphized function instance (`(DefId, args)` identity).
pub(super) struct FuncInstance {
    /// Template definition.
    pub def: DefId,
    /// Mangled IR symbol (`gen_function__1__bit32`).
    pub name: String,
    /// Concrete (fully substituted) signature.
    pub sig: FuncSig,
}

/// Production instantiation depth cap (design §3.3).
///
/// Chosen to stop non-terminating chains well before the checker's recursive
/// call stack is exhausted; legitimate generic nesting is far shallower.
#[cfg(not(test))]
const MAX_DEPTH: usize = 16;
/// Test-only cap so `E051` is reachable without a huge chain (ruling R4).
#[cfg(test)]
const MAX_DEPTH: usize = 4;

impl Checker<'_> {
    /// Walks a resolved type, instantiating every concrete `Instance` it
    /// mentions (registering layouts) and returning an equivalent type.
    pub(super) fn instantiate_ty(&mut self, ty: TypeRef, use_span: Span) -> TypeRef {
        match self.types.kind(ty.id).clone() {
            TypeKind::Instance { base, args } => {
                let _ = self.instantiate_object(base, &args, use_span);
                let args = args
                    .iter()
                    .map(|arg| self.instantiate_ty(*arg, use_span))
                    .collect();
                let id = self.types.intern_instance(base, args);
                TypeRef {
                    id,
                    is_ref: ty.is_ref,
                }
            }
            TypeKind::Func { params, returns } => {
                let params = params
                    .iter()
                    .map(|param| self.instantiate_ty(*param, use_span))
                    .collect();
                let returns = returns
                    .iter()
                    .map(|ret| self.instantiate_ty(*ret, use_span))
                    .collect();
                let id = self.types.intern_func(params, returns);
                TypeRef {
                    id,
                    is_ref: ty.is_ref,
                }
            }
            _ => ty,
        }
    }

    /// Monomorphizes `base` at `args`, memoizing by `(base DefId, args)`.
    ///
    /// Registers a concrete field layout under the instance's mangled name and
    /// eagerly specializes the object's functions whose own generic parameter
    /// list is empty (methods and associated functions). Methods with their own
    /// generic parameters specialize at their call site. Failed instances
    /// poison silently on later references (design §3.3).
    pub(super) fn instantiate_object(
        &mut self,
        base: DefId,
        args: &[TypeRef],
        use_span: Span,
    ) -> Result<TypeRef, ErrorGuaranteed> {
        let key = (base, args.to_vec());
        if let Some(ty) = self.object_instances.get(&key) {
            return Ok(*ty);
        }
        if self.failed_objects.contains(&key) {
            return Err(ErrorGuaranteed::new());
        }
        let Some((item, resolved, _parent)) = find_item(self.program, self.module, base) else {
            lilac_error::bug!(use_span, "instantiate_object on an unknown object");
        };
        let ItemKind::Object {
            members,
            generic_params,
        } = &item.kind
        else {
            lilac_error::bug!(item.span, "instantiate_object on a non-object item");
        };
        let object_name = self.object_name_of(base);
        self.types.set_object_name(base, &object_name);
        let id = self.types.intern_instance(base, args.to_vec());
        let instance_ty = TypeRef { id, is_ref: false };
        self.object_instances.insert(key.clone(), instance_ty);

        // Concrete layout: resolve each field template in the object's
        // parameter scope, substitute the instance's arguments, then register
        // any nested instances the field mentions.
        let mut layout = Vec::new();
        for member in members {
            if let ItemKind::Field { ty } = &member.kind {
                let template =
                    resolve_type(ty, self.types, self.module, self.diags, generic_params);
                let concrete = self.types.substitute(template, args);
                let concrete = self.instantiate_ty(concrete, member.span);
                layout.push((member.name.clone(), Some(concrete)));
            }
        }
        let mangled = self.types.mangle(id);
        self.layouts.insert(mangled, layout);

        let resolved_members = match &resolved.kind {
            ResKind::Object { members, .. } => members.clone(),
            _ => std::collections::HashMap::new(),
        };
        let mut failed = false;
        for member in members {
            let ItemKind::Func(decl) = &member.kind else {
                continue;
            };
            if !decl.generic_params.is_empty() {
                continue;
            }
            let Some(member_resolved) = resolved_members.get(&member.name) else {
                continue;
            };
            if !matches!(member_resolved.kind, ResKind::Func { .. }) {
                continue;
            }
            if self
                .instantiate_func(member_resolved.id, args, member.span)
                .is_err()
            {
                failed = true;
            }
        }
        if failed {
            self.object_instances.remove(&key);
            self.failed_objects.insert(key);
            return Err(ErrorGuaranteed::new());
        }
        Ok(instance_ty)
    }

    /// Monomorphizes `def` at `args`, memoizing by `(DefId, args)`.
    ///
    /// A cached success is reused; a cached failure returns poison with no new
    /// diagnostic; a miss substitutes the template signature, checks the body
    /// under the substituted environment, and caches the instance. Recursion
    /// with identical arguments terminates through the in-progress memo;
    /// lengthening chains past [`MAX_DEPTH`] report `E051`.
    pub(super) fn instantiate_func(
        &mut self,
        def: DefId,
        args: &[TypeRef],
        use_span: Span,
    ) -> Result<FuncSig, ErrorGuaranteed> {
        let key = (def, args.to_vec());
        if let Some(instance) = self.instances.get(&key) {
            return Ok(instance.sig.clone());
        }
        if self.failed.contains(&key) {
            return Err(ErrorGuaranteed::new());
        }
        let Some(template) = self.func_sig(def) else {
            lilac_error::bug!(use_span, "instantiate_func on an unknown function");
        };
        let sig = FuncSig {
            params: template
                .params
                .iter()
                .map(|param| self.types.substitute(*param, args))
                .collect(),
            returns: template
                .returns
                .iter()
                .map(|ret| self.types.substitute(*ret, args))
                .collect(),
        };
        if self.active.len() >= MAX_DEPTH {
            self.failed.insert(key);
            let chain = self.instantiation_chain();
            self.diags.emit(
                Diagnostic::error(
                    "E051",
                    "generic instantiation is too deep (possible infinite recursion)",
                )
                .label(use_span, true, "instantiated here")
                .note(format!("instantiation chain: {chain}")),
            );
            return Err(ErrorGuaranteed::new());
        }
        let name = self.instance_name(def, args);
        self.instances.insert(
            key.clone(),
            FuncInstance {
                def,
                name: name.clone(),
                sig: sig.clone(),
            },
        );
        self.active.push(key.clone());
        let result = self.specialize_func(def, args, &sig, &name);
        self.active.pop();
        if result.is_err() {
            self.instances.remove(&key);
            self.failed.insert(key);
        }
        result.map(|()| sig)
    }

    /// Checks a generic function's body under the substituted `sig` and emits
    /// its concrete `HirFunc` (or records its specialized `=>` body). The
    /// generic parameter names are in scope while the body resolves types, and
    /// `args` substitutes them.
    fn specialize_func(
        &mut self,
        def: DefId,
        args: &[TypeRef],
        sig: &FuncSig,
        name: &str,
    ) -> Result<(), ErrorGuaranteed> {
        let Some((item, _resolved, parent)) = find_item(self.program, self.module, def) else {
            lilac_error::bug!(Span::dummy(), "instantiated function has no AST item");
        };
        let ItemKind::Func(decl) = &item.kind else {
            lilac_error::bug!(item.span, "instantiated item is not a function");
        };
        let scope = self.func_scope(def);
        let saved_params = std::mem::replace(&mut self.current_params, scope);
        let saved_args = std::mem::replace(&mut self.current_args, args.to_vec());
        let errors_before = self.diags.checkpoint();
        let is_inline = matches!(decl.body, Some(lilac_ast::FuncBody::Inline(_)));
        if is_inline {
            // Guard recursive generic `=>` inlining (mirrors `check_inline_call`).
            self.in_progress.insert(def);
        }
        let (outcome, params, returns) = self.check_func_body(decl, sig, parent);
        match outcome {
            CheckedBody::Extern => self.hir.push(HirFunc {
                def,
                name: name.to_owned(),
                params,
                returns,
                body: HirBlock {
                    stmts: Vec::new(),
                    span: item.span,
                },
                span: item.span,
            }),
            CheckedBody::Block(body) => self.hir.push(HirFunc {
                def,
                name: name.to_owned(),
                params,
                returns,
                body,
                span: item.span,
            }),
            CheckedBody::Inline { param_names, value } => {
                self.instance_inline
                    .insert((def, args.to_vec()), (param_names, value));
            }
            CheckedBody::Rejected => {}
        }
        if is_inline {
            self.in_progress.remove(&def);
        }
        self.current_params = saved_params;
        self.current_args = saved_args;
        if self.errors_since(errors_before) {
            Err(ErrorGuaranteed::new())
        } else {
            Ok(())
        }
    }

    /// Routes a call to a generic function (or generic object method): checks
    /// generic-argument arity (`E044`), resolves the arguments in the current
    /// instantiation environment, instantiates, and checks the call arguments.
    pub(super) fn check_generic_func_call(
        &mut self,
        fctx: &mut FuncCtx,
        def: DefId,
        generic_params: &[String],
        generic_args: &[AstType],
        args: &[Expr],
        span: Span,
    ) -> Result<CallOutcome, ErrorGuaranteed> {
        if self.in_progress.contains(&def) {
            // A generic `=>` body may not inline itself: the specialization is
            // still being checked, so no inline body exists to expand.
            return Err(self.diags.emit(
                Diagnostic::error("E022", "cannot inline recursive `=>` call").label(
                    span,
                    true,
                    "recursive inline calls never terminate".to_owned(),
                ),
            ));
        }
        let name = self.function_name(def);
        if generic_args.len() != generic_params.len() {
            return Err(self.emit_generic_arity(
                span,
                &name,
                generic_params.len(),
                generic_args.len(),
            ));
        }
        let resolved = self.resolve_generic_args(generic_args);
        if resolved.iter().any(|ty| ty.id == TypeId::ERROR) {
            return Err(ErrorGuaranteed::new());
        }
        let key = (def, resolved.clone());
        let sig = self.instantiate_func(def, &resolved, span)?;
        let Some(instance) = self.instances.get(&key) else {
            lilac_error::bug!(span, "instantiated function missing from memo");
        };
        let instance_name = instance.name.clone();
        let instance_def = instance.def;
        if let Some((param_names, body)) = self.instance_inline.get(&key).cloned() {
            if sig.returns.len() != 1 {
                return Err(ErrorGuaranteed::new());
            }
            let hir_args = self.check_args(fctx, &sig, args, span)?;
            let map: HashMap<String, HirExpr> = param_names.into_iter().zip(hir_args).collect();
            let expanded = subst_ident(&body, &map);
            return Ok(CallOutcome::Inline(expanded, sig.returns[0]));
        }
        let hir_args = self.check_args(fctx, &sig, args, span)?;
        Ok(CallOutcome::Call(CheckedCall {
            outs: sig.returns.clone(),
            callee: HirCallee::Hook {
                name: instance_name,
                def: instance_def,
            },
            args: hir_args,
        }))
    }

    /// Resolves explicit generic arguments in the current body's environment.
    pub(super) fn resolve_generic_args(&mut self, args: &[AstType]) -> Vec<TypeRef> {
        args.iter().map(|arg| self.resolve_ty(arg)).collect()
    }

    /// Emits `E044` for a call-level generic-argument arity mismatch.
    pub(super) fn emit_generic_arity(
        &mut self,
        span: Span,
        what: &str,
        expected: usize,
        found: usize,
    ) -> ErrorGuaranteed {
        let noun = if expected == 1 {
            "argument"
        } else {
            "arguments"
        };
        self.diags.emit(
            Diagnostic::error(
                "E044",
                format!("wrong number of generic arguments for `{what}`"),
            )
            .label(
                span,
                true,
                format!("expected {expected} generic {noun}, found {found}"),
            ),
        )
    }

    /// Whether any hard error was emitted at or after `mark`.
    fn errors_since(&self, mark: usize) -> bool {
        let diagnostics = self.diags.diagnostics();
        let start = mark.min(diagnostics.len());
        diagnostics[start..]
            .iter()
            .any(|diagnostic| diagnostic.level == lilac_error::Level::Error)
    }

    /// Mangled IR symbol for one instance. Object members use the design's
    /// `<object-instance-mangle>.<member>` form (`GenObject__1__bit32.get`);
    /// free functions use `<Name>__<arity>__<arg>…` (design §3.4).
    pub(super) fn instance_name(&mut self, def: DefId, args: &[TypeRef]) -> String {
        if let Some((_name, obj_def, count)) = self.method_object(def) {
            if count > 0 {
                let object_args: Vec<TypeRef> = args.iter().take(count).copied().collect();
                let object_name = self.object_name_of(obj_def);
                self.types.set_object_name(obj_def, &object_name);
                let object_id = self.types.intern_instance(obj_def, object_args);
                let member = self.member_name(def);
                let mut name = format!("{}.{member}", self.types.mangle(object_id));
                // A method's own generic arguments (beyond the object's) are
                // arity-tagged so distinct specializations never collide
                //(`GenObject__1__bit32.get__1__bit64`, design §3.4).
                let own = &args[count.min(args.len())..];
                if !own.is_empty() {
                    name.push_str("__");
                    name.push_str(&own.len().to_string());
                    for arg in own {
                        name.push_str("__");
                        if arg.is_ref {
                            name.push_str("ref_");
                        }
                        name.push_str(&self.types.mangle(arg.id));
                    }
                }
                return name;
            }
        }
        let mut name = format!("{}__{}", self.function_name(def), args.len());
        for arg in args {
            name.push_str("__");
            if arg.is_ref {
                name.push_str("ref_");
            }
            name.push_str(&self.types.mangle(arg.id));
        }
        name
    }

    /// The source member name of a method/associated function.
    fn member_name(&self, def: DefId) -> String {
        for item in self.module.items.values() {
            if let ResKind::Object { members, .. } = &item.kind {
                for (member_name, member) in members {
                    if member.id == def {
                        return member_name.clone();
                    }
                }
            }
        }
        self.function_name(def)
    }

    /// The generic-parameter scope of a function body: the enclosing object's
    /// parameters first, then the function's own (Task 3 convention, so
    /// `substitute` indices line up with the substitution vector).
    fn func_scope(&self, def: DefId) -> Vec<String> {
        let own = find_item(self.program, self.module, def)
            .and_then(|(_, resolved, _)| match &resolved.kind {
                ResKind::Func { generic_params, .. } | ResKind::Hook { generic_params, .. } => {
                    Some(generic_params.clone())
                }
                _ => None,
            })
            .unwrap_or_default();
        match self.method_object(def) {
            Some((_, obj_def, _)) => {
                let mut scope = self.object_params(obj_def);
                scope.extend(own);
                scope
            }
            None => own,
        }
    }

    /// The enclosing object of a method/associated function, if any:
    /// `(object name, object DefId, object parameter count)`.
    fn method_object(&self, def: DefId) -> Option<(String, DefId, usize)> {
        let (_item, _resolved, parent) = find_item(self.program, self.module, def)?;
        let (parent_name, parent_def) = parent?;
        let count = self.object_params(parent_def).len();
        Some((parent_name, parent_def, count))
    }

    /// Source name of an object definition by identity.
    pub(super) fn object_name_of(&self, def: DefId) -> String {
        for (name, item) in &self.module.items {
            if item.id == def {
                return name.clone();
            }
        }
        format!("object#{}", def.0)
    }

    /// The declared generic parameter names of an object, in order.
    pub(super) fn object_params(&self, def: DefId) -> Vec<String> {
        for item in self.module.items.values() {
            if item.id == def {
                if let ResKind::Object { generic_params, .. } = &item.kind {
                    return generic_params.clone();
                }
            }
        }
        Vec::new()
    }

    /// A top-level or object-member `ResItem` by identity.
    pub(super) fn res_item(&self, def: DefId) -> Option<ResItem> {
        for item in self.module.items.values() {
            if item.id == def {
                return Some(item.clone());
            }
            if let ResKind::Object { members, .. } = &item.kind {
                for member in members.values() {
                    if member.id == def {
                        return Some(member.clone());
                    }
                }
            }
        }
        None
    }

    /// Source name of a function definition (qualified for object members).
    fn function_name(&self, def: DefId) -> String {
        for (name, item) in &self.module.items {
            if item.id == def {
                return name.clone();
            }
            if let ResKind::Object { members, .. } = &item.kind {
                for (member_name, member) in members {
                    if member.id == def {
                        return format!("{name}.{member_name}");
                    }
                }
            }
        }
        format!("func#{}", def.0)
    }

    /// The active instantiation chain, elided after 5 frames.
    fn instantiation_chain(&mut self) -> String {
        let active = self.active.clone();
        let frames: Vec<String> = active
            .iter()
            .map(|(def, args)| self.instance_name(*def, args))
            .collect();
        if frames.len() > 5 {
            let head = frames[..2].join(" -> ");
            let tail = frames[frames.len() - 2..].join(" -> ");
            format!("{head} -> ... -> {tail} ({} frames)", frames.len())
        } else {
            frames.join(" -> ")
        }
    }
}

#[cfg(test)]
mod tests {
    use lilac_error::Diags;
    use lilac_hir::{HirBlock, HirExpr, HirExprKind, HirProgram, HirStmt};
    use lilac_lex::Lexer;
    use lilac_parse::Parser;
    use lilac_span::SourceMap;
    use lilac_ty::{TypeKind, TypeRef, Types};

    fn check(src: &str) -> (Diags, HirProgram, Types) {
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
        let sigs = crate::collect_sigs(&module, &program, &mut types, &mut diags, true);
        let hir = crate::check_bodies(&program, &module, &sigs, &mut types, &mut diags);
        (diags, hir, types)
    }

    fn names(hir: &HirProgram) -> Vec<&str> {
        hir.funcs.iter().map(|func| func.name.as_str()).collect()
    }

    #[test]
    fn generic_function_monomorphizes() {
        let (diags, hir, _) = check(
            "gen_function[T]: function (in: T) out: T {\n    out = in\n}\n\nmain: function () nothing {\n    out: @bit32 = gen_function[@bit32](0x1)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let names = names(&hir);
        assert!(
            names.contains(&"gen_function__1__bit32"),
            "missing monomorphized instance: {names:?}"
        );
        assert!(
            !names.contains(&"gen_function"),
            "generic template leaked into HIR: {names:?}"
        );
    }

    #[test]
    fn no_param_in_output() {
        let (diags, hir, types) = check(
            "gen_function[T]: function (in: T) out: T {\n    out = in\n}\n\nmain: function () nothing {\n    out: @bit32 = gen_function[@bit32](0x1)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_no_param(&types, &hir);
    }

    #[test]
    fn cyclic_growing_args_is_e051() {
        let (diags, _, _) = check(
            "Box[T]: object {\n    value: T\n}\n\ngrow[T]: function () nothing {\n    grow[Box[T]]()\n}\n\nmain: function () nothing {\n    grow[@bit32]()\n}\n",
        );
        let count = diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E051")
            .count();
        assert_eq!(count, 1, "{diags:?}");
    }

    #[test]
    fn recursive_same_args_ok() {
        let (diags, hir, _) = check(
            "rec[T]: function (x: T) result: T {\n    result = rec[T](x)\n}\n\nmain: function () nothing {\n    y: @bit32 = rec[@bit32](0x1)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let instances: Vec<&str> = hir
            .funcs
            .iter()
            .filter(|func| func.name.starts_with("rec__"))
            .map(|func| func.name.as_str())
            .collect();
        assert_eq!(
            instances,
            vec!["rec__1__bit32"],
            "expected exactly one cached instance, got {:?}",
            names(&hir)
        );
    }

    #[test]
    fn generic_inline_control_flow_defers() {
        // A generic `=>` body with control flow must emit the M3 deferral, not
        // cache a temp-bearing inline body (which would ICE in lowering).
        let (diags, _, _) = check(
            "pick[T]: function (x: T, c: @bit8) T => if c x else x\n\nmain: function () nothing {\n    y: @bit32 = pick[@bit32](0x1, 0x1)\n}\n",
        );
        assert!(
            diags.diagnostics().iter().any(|d| d.code == "E030"),
            "{diags:?}"
        );
    }

    #[test]
    fn generic_inline_named_slot_ok() {
        // The first return slot is assigned by the `=>` body, so no `E027`.
        let (diags, _, _) = check(
            "gen_inline[T]: function (x: T) out: T => x\n\nmain: function () nothing {\n    y: @bit32 = gen_inline[@bit32](0x1)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn recursive_generic_inline_is_e022() {
        let (diags, _, _) = check(
            "rec_inline[T]: function (x: T) T => rec_inline[T](x)\n\nmain: function () nothing {\n    y: @bit32 = rec_inline[@bit32](0x1)\n}\n",
        );
        assert!(
            diags.diagnostics().iter().any(|d| d.code == "E022"),
            "{diags:?}"
        );
    }

    #[test]
    fn generic_object_layout() {
        let (diags, hir, types) = check(
            "GenObject[T]: object {\n    value: T\n}\n\nmain: function () nothing {\n    variable: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let layout = hir
            .layouts
            .get("GenObject__1__bit32")
            .expect("missing instance layout");
        assert_eq!(layout.len(), 1, "{layout:?}");
        assert_eq!(layout[0].0, "value");
        let field = layout[0].1.expect("field carries a concrete type");
        assert_eq!(types.display(field.id), "@bit32");
        assert!(
            !matches!(types.kind(field.id), TypeKind::Param { .. }),
            "layout leaked a Param: `{}`",
            types.display(field.id)
        );
    }

    #[test]
    fn generic_object_method() {
        let (diags, hir, types) = check(
            "GenObject[T]: object {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\n\nmain: function () nothing {\n    variable: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    value: @bit32 = variable.get()\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(
            names(&hir).contains(&"GenObject__1__bit32.get"),
            "missing monomorphized method: {:?}",
            names(&hir)
        );
        assert_no_param(&types, &hir);
    }

    #[test]
    fn value_based_duck_typing_ok() {
        let (diags, _, _) = check(
            "GenObject[T]: object {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\n\ngen[T]: function (value: T) out: @bit32 {\n    out = value.get()\n}\n\nmain: function () nothing {\n    v: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    x: @bit32 = gen[GenObject[@bit32]](v)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn static_method_on_object_ok() {
        let (diags, _, _) = check(
            "Object: object {\n    get: function () result: @bit32 {\n        result = 0x2\n    }\n}\n\ngen[T]: function () out: @bit32 {\n    out = T.get()\n}\n\nmain: function () nothing {\n    x: @bit32 = gen[Object]()\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
    }

    #[test]
    fn static_method_on_instance_ok() {
        // Static duck typing over a concrete generic instance: `T.get()`
        // resolves against `GenObject[@bit32]` (Gap 2 coverage).
        let (diags, hir, _) = check(
            "GenObject[T]: object {\n    value: T\n    get: function () result: @bit32 {\n        result = 0x2\n    }\n}\n\ngen[T]: function () out: @bit32 {\n    out = T.get()\n}\n\nmain: function () nothing {\n    x: @bit32 = gen[GenObject[@bit32]]()\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(
            names(&hir).contains(&"GenObject__1__bit32.get"),
            "missing instance static method: {:?}",
            names(&hir)
        );
    }

    #[test]
    fn method_own_generics_mangle_distinctly() {
        // A method's own generic arguments must be part of its symbol, or two
        // specializations collide (`GenObject__1__bit32.get` twice).
        let (diags, hir, _) = check(
            "GenObject[T]: object {\n    value: T\n    get[U]: function (x: U) result: @bit32 {\n        result = 0x2\n    }\n}\n\ngen[T]: function () out: @bit32 {\n    out = T.get[@bit32](0x0)\n}\n\ngen2[T]: function () out: @bit32 {\n    out = T.get[@bit64](0x0)\n}\n\nmain: function () nothing {\n    a: @bit32 = gen[GenObject[@bit32]]()\n    b: @bit32 = gen2[GenObject[@bit32]]()\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let names = names(&hir);
        assert!(
            names.contains(&"GenObject__1__bit32.get__1__bit32"),
            "missing method-own-arg instance: {names:?}"
        );
        assert!(
            names.contains(&"GenObject__1__bit32.get__1__bit64"),
            "missing method-own-arg instance: {names:?}"
        );
    }

    fn assert_no_param(types: &Types, hir: &HirProgram) {
        for func in &hir.funcs {
            for param in func.params.iter().chain(func.returns.iter()) {
                assert_concrete(types, param.ty);
            }
            walk_block(types, &func.body);
        }
        for fields in hir.layouts.values() {
            for (_, ty) in fields {
                if let Some(ty) = ty {
                    assert_concrete(types, *ty);
                }
            }
        }
        for constant in &hir.consts {
            walk_expr(types, &constant.value);
        }
    }

    fn assert_concrete(types: &Types, ty: TypeRef) {
        assert!(
            !matches!(types.kind(ty.id), TypeKind::Param { .. }),
            "`Param` leaked into HIR: `{}`",
            types.display(ty.id)
        );
    }

    fn walk_block(types: &Types, block: &HirBlock) {
        for stmt in &block.stmts {
            walk_stmt(types, stmt);
        }
    }

    fn walk_stmt(types: &Types, stmt: &HirStmt) {
        match stmt {
            HirStmt::Let { ty, value, .. } => {
                assert_concrete(types, *ty);
                walk_expr(types, value);
            }
            HirStmt::MultiLet {
                bindings, value, ..
            } => {
                for binding in bindings {
                    assert_concrete(types, binding.ty);
                }
                walk_expr(types, value);
            }
            HirStmt::Assign { target, value, .. } => {
                walk_expr(types, target);
                walk_expr(types, value);
            }
            HirStmt::ExprStmt(expr) => walk_expr(types, expr),
            HirStmt::Return { .. } | HirStmt::Break { .. } => {}
            HirStmt::If {
                cond, then, els, ..
            } => {
                walk_expr(types, cond);
                walk_block(types, then);
                walk_block(types, els);
            }
            HirStmt::Loop { body, .. } => walk_block(types, body),
            HirStmt::Match {
                scrutinee,
                arms,
                els,
                ..
            } => {
                walk_expr(types, scrutinee);
                for arm in arms {
                    if let Some(capture) = &arm.capture {
                        assert_concrete(types, capture.ty);
                    }
                    walk_block(types, &arm.body);
                }
                walk_block(types, els);
            }
        }
    }

    fn walk_expr(types: &Types, expr: &HirExpr) {
        assert_concrete(types, expr.ty);
        match &expr.kind {
            HirExprKind::Variant {
                payload: Some(payload),
                ..
            } => walk_expr(types, payload),
            HirExprKind::Call { args, .. } => {
                for arg in args {
                    walk_expr(types, arg);
                }
            }
            HirExprKind::EqEq { lhs, rhs } => {
                walk_expr(types, lhs);
                walk_expr(types, rhs);
            }
            HirExprKind::Member { obj, .. } => walk_expr(types, obj),
            HirExprKind::RefOf { value } => walk_expr(types, value),
            HirExprKind::StructLit { fields } => {
                for field in fields {
                    walk_expr(types, &field.value);
                }
            }
            _ => {}
        }
    }
}
