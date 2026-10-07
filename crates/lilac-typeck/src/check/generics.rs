//! Generic instantiation core: monomorphizing generic functions.
//!
//! A generic declaration is a template; the first concrete reference to it
//! specializes the body under a substituted environment and caches the result
//! by `(DefId, Vec<TypeRef>)`. Failed instances poison silently on later
//! references; growing/recursive chains are bounded by [`MAX_DEPTH`] (`E051`).

use std::collections::HashMap;

use lilac_ast::{Expr, Item, ItemKind, Type as AstType};
use lilac_error::{Diagnostic, ErrorGuaranteed, CHAIN_FRAME_PREFIX};
use lilac_hir::{HirBlock, HirCallee, HirExpr, HirFunc};
use lilac_resolve::{flat_member_pairs, DefId, ResItem, ResKind};
use lilac_span::Span;
use lilac_ty::{FuncSig, TypeId, TypeKind, TypeRef};

use super::{
    find_item, is_poison, qualify_name, subst_ident, CallOutcome, CheckedBody, CheckedCall,
    Checker, FuncCtx,
};

/// One monomorphized function instance (`(DefId, args)` identity).
pub(super) struct FuncInstance {
    /// Template definition.
    pub def: DefId,
    /// Mangled IR symbol (`gen_function.1..bit32`).
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
        // Growing layouts (`next: GenObj[GenObj[T]]`) never repeat a memo key,
        // so bound the layout recursion like `instantiate_func` (design §3.3).
        if self.active_objects.len() >= MAX_DEPTH {
            self.failed_objects.insert(key);
            let chain = self.object_instantiation_chain();
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
        let saved_file = self.file;
        let Some((item, resolved, _parent)) = find_item(self.program, self.project, base) else {
            lilac_error::bug!(use_span, "instantiate_object on an unknown object");
        };
        // The object's field templates resolve in the file that declares the
        // object, not in the file that instantiated it.
        self.file = item.span.file;
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
        self.active_objects.push(key.clone());

        let layout = self.object_layout(members, generic_params, args);
        let mangled = self.types.mangle(id);
        self.layouts.insert(mangled.clone(), layout);

        let mut failed = false;
        let mut drop_hook = None;
        for (member, member_resolved) in flat_member_pairs(members, &resolved.kind) {
            let ItemKind::Func(decl) = &member.kind else {
                continue;
            };
            // In-body hooks are templates until the parent instance exists; the
            // drop hook is specialized below so M2/M3 lowering can call it
            // (design §3.3). Other hooks (conversion/operator) are out of scope.
            if matches!(member_resolved.kind, ResKind::Hook { .. }) {
                if member.name == "@on_drop" && decl.generic_params.is_empty() {
                    drop_hook = Some(member_resolved.id);
                }
                continue;
            }
            if !decl.generic_params.is_empty() {
                continue;
            }
            if !matches!(member_resolved.kind, ResKind::Func { .. }) {
                continue;
            }
            if self
                .instantiate_func(member_resolved.id, args, use_span)
                .is_err()
            {
                failed = true;
            }
        }
        if !failed {
            if let Some(hook_def) = drop_hook {
                // Register the concrete drop entry keyed by the instance's
                // mangled name, pointing at its monomorphized `@on_drop`
                // (`GenBox.1..bit32` -> `GenBox.1..bit32.@on_drop`).
                match self.instantiate_func(hook_def, args, use_span) {
                    Ok(_) => {
                        let hook = self.instance_name(hook_def, args);
                        self.drops.insert(mangled.clone(), hook);
                    }
                    Err(_) => failed = true,
                }
            }
        }
        self.active_objects.pop();
        self.file = saved_file;
        if failed {
            self.object_instances.remove(&key);
            self.layouts.remove(&mangled);
            self.failed_objects.insert(key);
            return Err(ErrorGuaranteed::new());
        }
        Ok(instance_ty)
    }

    /// Resolves an object's concrete field layout: each field template is
    /// resolved in the object's parameter scope, substituted with the
    /// instance's arguments, then registered (nested instances included).
    fn object_layout(
        &mut self,
        members: &[Item],
        generic_params: &[String],
        args: &[TypeRef],
    ) -> Vec<(String, Option<TypeRef>)> {
        let mut layout = Vec::new();
        for member in members {
            if let ItemKind::Field { ty } = &member.kind {
                let template = self.resolve_in_scope(ty, generic_params);
                let concrete = self.types.substitute(template, args);
                let concrete = self.instantiate_ty(concrete, member.span);
                layout.push((member.name.clone(), Some(concrete)));
            }
        }
        layout
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
        let mark = self.diags.checkpoint();
        let result = self.specialize_func(def, args, &sig, &name);
        if result.is_err() {
            self.instances.remove(&key);
            self.failed.insert(key);
            let guaranteed = self.instantiation_failure(def, args, use_span, mark);
            self.active.pop();
            return Err(guaranteed);
        }
        self.active.pop();
        result.map(|()| sig)
    }

    /// Drains the diagnostics captured while specializing `def[args]` and
    /// re-attributes the duck-typing requirement failures to `use_span` as a
    /// single `E042`. Structural failures (deferrals, recursion guards) are
    /// re-emitted unchanged; a failed instantiation with no requirement failure
    /// reports nothing new (design §3.5).
    pub(super) fn instantiation_failure(
        &mut self,
        def: DefId,
        args: &[TypeRef],
        use_span: Span,
        mark: usize,
    ) -> ErrorGuaranteed {
        let captured = self.diags.take_since(mark);
        let (requirements, structural): (Vec<Diagnostic>, Vec<Diagnostic>) =
            captured.into_iter().partition(is_requirement_failure);
        for diagnostic in structural {
            self.diags.emit(diagnostic);
        }
        if requirements.is_empty() {
            return ErrorGuaranteed::new();
        }
        let dedup = (def, args.to_vec(), use_span.file.0, use_span.lo.0);
        if !self.reported_failures.insert(dedup) {
            return ErrorGuaranteed::new();
        }

        let name = self.function_name(def);
        let params = self.func_scope(def).join(", ");
        let arg_list = args
            .iter()
            .map(|arg| self.types.display_ref(*arg))
            .collect::<Vec<_>>()
            .join(", ");
        let pretty = self.function_instance_display(def, args);
        let mut diagnostic = Diagnostic::error(
            "E042",
            format!("type `{arg_list}` does not satisfy `{name}[{params}]` requirements"),
        );

        // A missing-member failure is the most useful primary: point at the
        // actual use in the generic body and turn the instantiation into a
        // sourced note naming the pretty instance.
        let mut instantiation_note = false;
        if let Some(index) = requirements
            .iter()
            .position(|requirement| lookup_miss(requirement).is_some())
        {
            if let Some(label) = requirement_label(&requirements[index]) {
                diagnostic =
                    diagnostic.label(label.span, true, requirement_phrase(&requirements[index]));
            } else {
                diagnostic = diagnostic.label(use_span, true, "instantiated here");
            }
            diagnostic = diagnostic.label(
                use_span,
                false,
                format!("required by instantiation of {pretty} here"),
            );
            instantiation_note = true;
            for (position, requirement) in requirements.iter().enumerate() {
                if position == index {
                    continue;
                }
                if let Some(label) = requirement_label(requirement) {
                    diagnostic =
                        diagnostic.label(label.span, false, requirement_phrase(requirement));
                }
            }
        } else {
            diagnostic = diagnostic.label(use_span, true, "instantiated here");
            if let Some((item, _, _)) = find_item(self.program, self.project, def) {
                diagnostic = diagnostic.label(
                    item.span,
                    false,
                    format!("required by `{name}[{params}]` here"),
                );
            }
            for requirement in &requirements {
                if let Some(label) = requirement_label(requirement) {
                    diagnostic =
                        diagnostic.label(label.span, false, requirement_phrase(requirement));
                }
            }
        }
        // A single active frame is already named by the pretty instantiation
        // note; only multi-frame chains need the `in instantiation of` list
        // (which the emitter elides past five unless `--verbose`).
        let frames = self.instantiation_frames();
        if !(instantiation_note && frames.len() <= 1) {
            for frame in frames {
                diagnostic = diagnostic.note(format!("{CHAIN_FRAME_PREFIX}{frame}"));
            }
        }
        for requirement in &requirements {
            for note in &requirement.notes {
                diagnostic = diagnostic.note(note.clone());
            }
        }
        for help in self.requirement_helps(&requirements) {
            diagnostic = diagnostic.help(help);
        }
        self.diags.emit(diagnostic)
    }

    /// Whether `ty` is one of the current instantiation's substituted type
    /// parameters. A missing member on such a receiver is a generic requirement
    /// failure, so the required-signature note is meaningful.
    pub(super) fn is_generic_derived(&self, ty: TypeRef) -> bool {
        !self.active.is_empty() && self.current_args.iter().any(|arg| arg.id == ty.id)
    }

    /// Renders a body-side type in the generic's own parameter form: a type that
    /// equals a current substitution argument prints as its parameter name
    /// (`T`), otherwise concretely (`@bit32`). `ref` wrappers are preserved.
    ///
    /// If two parameters share a concrete type, the first matching parameter
    /// wins (`position` scans `current_params`/`current_args` in order); a
    /// requirement expressed with an ambiguous shared argument is rare, and the
    /// first-declared name is the least surprising choice.
    pub(super) fn display_ty_in_body(&self, ty: TypeRef) -> String {
        if let Some(index) = self.current_args.iter().position(|arg| arg.id == ty.id) {
            if let Some(name) = self.current_params.get(index) {
                if ty.is_ref && !self.current_args[index].is_ref {
                    return format!("ref {name}");
                }
                return name.clone();
            }
        }
        self.types.display_ref(ty)
    }

    /// Builds the `note:` describing the method signature the generic body
    /// requires at a missing-member use site.
    ///
    /// `receiver` is the value receiver type for `value.member(...)` (`None` for
    /// a bare-type `T.member(...)` receiver, which has no `self`). Receiver,
    /// argument, and return types all print in the generic's parameter form when
    /// they come from a current instantiation argument. Each argument is
    /// probe-checked with no expected type; diagnostics it emits are discarded.
    /// The return type is the use-site expected type, or `?`.
    pub(super) fn required_signature_note(
        &mut self,
        fctx: &mut FuncCtx,
        field: &str,
        receiver: Option<TypeRef>,
        args: &[Expr],
        expected: Option<TypeId>,
    ) -> String {
        let mut params = Vec::new();
        if let Some(ty) = receiver {
            // A method's first parameter is always `ref Self` (see
            // `check_instance_member_call`); qualify the base type with `ref`.
            let base = self.display_ty_in_body(TypeRef {
                id: ty.id,
                is_ref: false,
            });
            params.push(format!("self: ref {base}"));
        }
        for arg in args {
            let mark = self.diags.checkpoint();
            let (ty, _) = self.check_expr(fctx, arg, None);
            let _ = self.diags.take_since(mark);
            if is_poison(ty) {
                params.push("?".to_owned());
            } else {
                params.push(self.display_ty_in_body(ty));
            }
        }
        let ret = match expected {
            Some(id) if id != TypeId::ERROR => {
                self.display_ty_in_body(TypeRef { id, is_ref: false })
            }
            _ => "?".to_owned(),
        };
        format!(
            "based on this usage, the generic body requires a method matching:\n      fn {field}({}) -> {ret}",
            params.join(", ")
        )
    }

    /// Builds the `note:` describing the field the generic body requires at a
    /// missing-field use site. The field's type is the use-site expected type in
    /// the generic's own parameter form, or `?` when unconstrained.
    pub(super) fn required_field_note(&self, field: &str, expected: Option<TypeId>) -> String {
        let ty = match expected {
            Some(id) if id != TypeId::ERROR => {
                self.display_ty_in_body(TypeRef { id, is_ref: false })
            }
            _ => "?".to_owned(),
        };
        format!(
            "based on this usage, the generic body requires a field matching:\n      {field}: {ty}"
        )
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
        let Some((item, _resolved, parent)) = find_item(self.program, self.project, def) else {
            lilac_error::bug!(Span::dummy(), "instantiated function has no AST item");
        };
        let ItemKind::Func(decl) = &item.kind else {
            lilac_error::bug!(item.span, "instantiated item is not a function");
        };
        // The specialized body resolves its types in the file that declares the
        // generic, not in the file that instantiated it.
        let saved_file = self.file;
        self.file = item.span.file;
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
                is_extern: true,
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
                is_extern: false,
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
        self.file = saved_file;
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
    /// `<object-instance-mangle>.<member>` form (`GenObject.1..bit32.get`);
    /// free functions use `<Name>.<arity>.<arg>…` (design §3.4). `.` is the
    /// reserved separator; user identifiers cannot contain it.
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
                //(`GenObject.1..bit32.get.1..bit64`, design §3.4).
                let own = &args[count.min(args.len())..];
                if !own.is_empty() {
                    name.push('.');
                    name.push_str(&own.len().to_string());
                    for arg in own {
                        name.push('.');
                        if arg.is_ref {
                            name.push_str("ref.");
                        }
                        name.push_str(&self.types.mangle(arg.id));
                    }
                }
                return name;
            }
        }
        let mut name = format!("{}.{}", self.function_name(def), args.len());
        for arg in args {
            name.push('.');
            if arg.is_ref {
                name.push_str("ref.");
            }
            name.push_str(&self.types.mangle(arg.id));
        }
        name
    }

    /// The source member name of a method/associated function.
    fn member_name(&self, def: DefId) -> String {
        for (_, item) in self.project.all_items() {
            if let ResKind::Object { members, hooks, .. } = &item.kind {
                for (member_name, member) in members {
                    if member.id == def {
                        return member_name.clone();
                    }
                }
                for (hook_name, overloads) in hooks {
                    if overloads.iter().any(|hook| hook.id == def) {
                        return hook_name.clone();
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
        let own = find_item(self.program, self.project, def)
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
        let (_item, _resolved, parent) = find_item(self.program, self.project, def)?;
        let (parent_name, parent_def) = parent?;
        let count = self.object_params(parent_def).len();
        Some((parent_name, parent_def, count))
    }

    /// Namespace-qualified source name of an object definition by identity
    /// (root adds no prefix), matching the type interner's stored name.
    pub(super) fn object_name_of(&self, def: DefId) -> String {
        for (name, item) in self.project.all_items() {
            if item.id == def {
                return qualify_name(self.project, def, name);
            }
        }
        format!("object#{}", def.0)
    }

    /// The declared generic parameter names of an object, in order.
    pub(super) fn object_params(&self, def: DefId) -> Vec<String> {
        for (_, item) in self.project.all_items() {
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
        for (_, item) in self.project.all_items() {
            if item.id == def {
                return Some(item.clone());
            }
            if let ResKind::Object { members, hooks, .. } = &item.kind {
                for member in members.values() {
                    if member.id == def {
                        return Some(member.clone());
                    }
                }
                for hook in hooks.values().flatten() {
                    if hook.id == def {
                        return Some(hook.clone());
                    }
                }
            }
        }
        None
    }

    /// Source name of a function definition (qualified for object members).
    fn function_name(&self, def: DefId) -> String {
        for (name, item) in self.project.all_items() {
            if item.id == def {
                return name.to_owned();
            }
            if let ResKind::Object { members, hooks, .. } = &item.kind {
                for (member_name, member) in members {
                    if member.id == def {
                        return format!("{name}.{member_name}");
                    }
                }
                for (hook_name, overloads) in hooks {
                    if overloads.iter().any(|hook| hook.id == def) {
                        return format!("{name}.{hook_name}");
                    }
                }
            }
        }
        format!("func#{}", def.0)
    }

    /// Human-readable function instance for diagnostics: `<source name>[@arg,
    /// ...]` (e.g. `gen_function[@bit32]`). Mangling stays for IR symbols only.
    fn function_instance_display(&self, def: DefId, args: &[TypeRef]) -> String {
        let name = self.function_name(def);
        if args.is_empty() {
            return name;
        }
        let args = args
            .iter()
            .map(|arg| self.types.display_ref(*arg))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{name}[{args}]")
    }

    /// Human-readable object instance for diagnostics: `<ObjectName>[@arg, ...]`
    /// (e.g. `GenBox[@bit32]`). Mangling stays for IR symbols only.
    fn object_instance_display(&mut self, base: DefId, args: &[TypeRef]) -> String {
        let object_name = self.object_name_of(base);
        self.types.set_object_name(base, &object_name);
        let id = self.types.intern_instance(base, args.to_vec());
        self.types.display(id)
    }

    /// The active instantiation chain, one human-readable instance per frame
    /// (innermost last).
    fn instantiation_frames(&mut self) -> Vec<String> {
        let active = self.active.clone();
        active
            .iter()
            .map(|(def, args)| self.function_instance_display(*def, args))
            .collect()
    }

    /// The active instantiation chain as one `a -> b` string, elided in the
    /// middle past 5 frames.
    fn instantiation_chain(&mut self) -> String {
        let frames = self.instantiation_frames();
        if frames.len() > 5 {
            let head = frames[..2].join(" -> ");
            let tail = frames[frames.len() - 2..].join(" -> ");
            format!("{head} -> ... -> {tail} ({} frames)", frames.len())
        } else {
            frames.join(" -> ")
        }
    }

    /// The active object-instantiation chain, one human-readable instance per
    /// frame (innermost last).
    fn object_instantiation_frames(&mut self) -> Vec<String> {
        let active = self.active_objects.clone();
        active
            .iter()
            .map(|(base, args)| self.object_instance_display(*base, args))
            .collect()
    }

    /// The active object-instantiation chain as one `a -> b` string, elided in
    /// the middle past 5 frames.
    fn object_instantiation_chain(&mut self) -> String {
        let frames = self.object_instantiation_frames();
        if frames.len() > 5 {
            let head = frames[..2].join(" -> ");
            let tail = frames[frames.len() - 2..].join(" -> ");
            format!("{head} -> ... -> {tail} ({} frames)", frames.len())
        } else {
            frames.join(" -> ")
        }
    }

    /// Ranked `help:` suggestions for the captured requirement failures:
    /// edit-distance did-you-mean over the owner's members plus a discovered
    /// `@conversion` (design §3.5).
    fn requirement_helps(&self, requirements: &[Diagnostic]) -> Vec<String> {
        let mut helps = Vec::new();
        for requirement in requirements {
            if let Some((field, owner)) = missing_member(requirement) {
                let members = self.owner_members(&owner);
                if let Some(best) = closest_member(&field, &members) {
                    helps.push(format!("`{owner}` has `{best}`, did you mean `{best}`?"));
                }
            }
            if requirement.code == "E043" {
                if let Some((from, _to)) = conversion_pair(requirement) {
                    if self
                        .sigs
                        .conversions
                        .iter()
                        .any(|conversion| self.types.display(conversion.from) == from)
                    {
                        helps.push(format!(
                            "a `@conversion` from `{from}` exists; check its target"
                        ));
                    }
                }
            }
            if requirement.code == "E022" {
                if let Some((expected, found)) = arity_pair(requirement) {
                    helps.push(format!(
                        "the call takes {expected} arguments, but {found} were supplied"
                    ));
                }
            }
            if requirement.code == "E010" && requirement.msg.contains("expected ref ") {
                helps.push("try passing a reference: `ref <value>`".to_owned());
            }
        }
        helps
    }

    /// Concrete members of the object named `owner`, resolving an instance
    /// mangle (`GenObject.1..bit32`) back to its base name.
    fn owner_members(&self, owner: &str) -> Vec<String> {
        // The owner is either an IR mangle (`GenObject.1..bit32`) or the
        // display form (`GenObject[@bit32]`); recover the base name from both.
        let base = owner.split('.').next().unwrap_or(owner);
        let base = base.split('[').next().unwrap_or(base);
        let mut members: Vec<String> = Vec::new();
        for (name, item) in self.project.all_items() {
            if name == owner || name == base {
                if let ResKind::Object { members: table, .. } = &item.kind {
                    members.extend(table.keys().cloned());
                }
            }
        }
        for (object, field) in self.fields.keys() {
            if object == owner || object == base {
                members.push(field.clone());
            }
        }
        members.sort();
        members.dedup();
        members
    }
}

/// Codes that describe a duck-typing requirement failure on a concrete type
/// (member lookup, type compatibility, conversion, call arity). Structural
/// failures (`E022` recursion guards, `E030` deferrals) are re-emitted, not
/// folded.
fn is_requirement_failure(diagnostic: &Diagnostic) -> bool {
    matches!(diagnostic.code, "E010" | "E021" | "E040" | "E041" | "E043")
        || (diagnostic.code == "E022" && diagnostic.msg.starts_with("wrong number of arguments"))
}

/// Human phrase for one captured body failure, e.g. a lookup miss becomes
/// "missing method `other` on `@bit32`" (fields become "missing member ...").
fn requirement_phrase(diagnostic: &Diagnostic) -> String {
    if let Some((is_method, field, owner)) = lookup_miss(diagnostic) {
        let kind = if is_method { "method" } else { "member" };
        return format!("missing {kind} `{field}` on `{owner}`");
    }
    if diagnostic.code == "E022" {
        return diagnostic.msg.clone();
    }
    match diagnostic.labels.iter().find(|label| label.primary) {
        Some(label) => label.msg.clone(),
        None => diagnostic.msg.clone(),
    }
}

/// The primary label of a captured requirement, or its first label.
fn requirement_label(diagnostic: &Diagnostic) -> Option<&lilac_error::Label> {
    diagnostic
        .labels
        .iter()
        .find(|label| label.primary)
        .or_else(|| diagnostic.labels.first())
}

/// Extracts `(member, owner)` from a lookup-miss headline, if any.
fn missing_member(diagnostic: &Diagnostic) -> Option<(String, String)> {
    lookup_miss(diagnostic).map(|(_, field, owner)| (field, owner))
}

/// Extracts `(is_method, member, owner)` from a lookup-miss headline.
fn lookup_miss(diagnostic: &Diagnostic) -> Option<(bool, String, String)> {
    let (is_method, rest) =
        if let Some(rest) = diagnostic.msg.strip_prefix("cannot call static member `") {
            (true, rest)
        } else if let Some(rest) = diagnostic.msg.strip_prefix("cannot call method `") {
            (true, rest)
        } else if let Some(rest) = diagnostic.msg.strip_prefix("no member `") {
            (true, rest)
        } else {
            (false, diagnostic.msg.strip_prefix("no field `")?)
        };
    let (field, tail) = rest.split_once("` on `")?;
    let owner = tail.strip_suffix('`')?;
    Some((is_method, field.to_owned(), owner.to_owned()))
}

/// Extracts `(from, to)` from an `E043` conversion-miss headline.
fn conversion_pair(diagnostic: &Diagnostic) -> Option<(String, String)> {
    let rest = diagnostic.msg.strip_prefix("no conversion from ")?;
    let (from, to) = rest.split_once(" to ")?;
    Some((from.to_owned(), to.to_owned()))
}

/// Extracts `(expected, found)` counts from an arity `E022` headline.
fn arity_pair(diagnostic: &Diagnostic) -> Option<(String, String)> {
    let rest = diagnostic
        .msg
        .strip_prefix("wrong number of arguments: expected ")?;
    let (expected, found) = rest.split_once(", found ")?;
    Some((expected.to_owned(), found.to_owned()))
}

/// Closest member by edit distance within a small threshold, if any.
fn closest_member(field: &str, members: &[String]) -> Option<String> {
    members
        .iter()
        .map(|member| (levenshtein(field, member), member))
        .filter(|(distance, member)| *distance <= 2 && member.as_str() != field)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, member)| member.clone())
}

/// Levenshtein edit distance between two strings (no new crates).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use lilac_error::Diags;
    use lilac_hir::{HirBlock, HirExpr, HirExprKind, HirProgram, HirStmt};
    use lilac_lex::Lexer;
    use lilac_parse::Parser;
    use lilac_span::SourceMap;
    use lilac_ty::{TypeKind, TypeRef, Types};

    fn run(src: &str) -> (Diags, HirProgram, Types, SourceMap) {
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
        let project = lilac_resolve::ResolvedProject::single(module, file);
        let mut types = Types::new();
        let sigs = crate::collect_sigs(&project, &program, &mut types, &mut diags);
        let hir = crate::check_bodies(&program, &project, &sigs, &mut types, &mut diags);
        (diags, hir, types, sm)
    }

    fn check(src: &str) -> (Diags, HirProgram, Types) {
        let (diags, hir, types, _) = run(src);
        (diags, hir, types)
    }

    fn names(hir: &HirProgram) -> Vec<&str> {
        hir.funcs.iter().map(|func| func.name.as_str()).collect()
    }

    fn render_all(diags: &Diags, sm: &SourceMap) -> String {
        use lilac_error::{Emitter, EmitterHuman};
        diags
            .diagnostics()
            .iter()
            .map(|diagnostic| EmitterHuman::new().render(diagnostic, sm))
            .collect()
    }

    #[test]
    fn fails_generic_duck_typing_short() {
        let src = "gen_function[T]: function () out: T {\n    out = T.other()\n}\n\nmain: function () nothing {\n    variable: @bit32 = gen_function[@bit32]()\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        let e042: Vec<_> = diags
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == "E042")
            .collect();
        assert_eq!(e042.len(), 1, "{rendered}");
        assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
        assert!(
            rendered.contains("required by instantiation of gen_function[@bit32]"),
            "{rendered}"
        );
        assert!(rendered.contains("missing method `other`"), "{rendered}");
        assert!(!rendered.contains("error[E010]"), "{rendered}");
    }

    #[test]
    fn generic_missing_member_shows_required_signature() {
        // A bare-type receiver (`T.other()`) makes the captured body use the
        // `E042` primary, and the inferred signature is a note using the
        // use-site expected return type, with the pretty instance name in the
        // instantiation note.
        let src = "gen_function[T]: function () out: T {\n    out = T.other()\n}\n\nmain: function () nothing {\n    variable: @bit32 = gen_function[@bit32]()\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        let e042: Vec<_> = diags
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == "E042")
            .collect();
        assert_eq!(e042.len(), 1, "{rendered}");
        let primary = e042[0]
            .labels
            .iter()
            .find(|label| label.primary)
            .expect("E042 primary label");
        let (line, _) = sm.line_col(primary.span).expect("primary span has a file");
        assert_eq!(line, 2, "primary must be the body use: {rendered}");
        assert!(
            rendered.contains("fn other() -> T"),
            "missing inferred signature: {rendered}"
        );
        assert!(
            rendered.contains("required by instantiation of gen_function[@bit32]"),
            "missing pretty instantiation note: {rendered}"
        );
        // The single active frame is already named by the instantiation note,
        // so the `in instantiation of` chain must not duplicate it.
        assert!(
            !rendered.contains("in instantiation of gen_function[@bit32]"),
            "single-frame chain duplicated: {rendered}"
        );
        assert!(
            !rendered.contains(".1.."),
            "mangled symbol leaked: {rendered}"
        );
    }

    #[test]
    fn value_receiver_shows_required_signature() {
        // A value receiver of generic-param type renders `self: T`; the argument
        // is probe-checked (a decimal literal is `@byte_buffer`) and the return
        // comes from the generic parameter.
        let src = "gen[T]: function (value: T) out: T {\n    out = value.other(5)\n}\n\nmain: function () nothing {\n    x: @bit32 = gen[@bit32](0x1)\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
        assert!(
            rendered.contains("fn other(self: ref T, @byte_buffer) -> T"),
            "missing value-receiver signature: {rendered}"
        );
    }

    #[test]
    fn generic_param_form_in_required_signature() {
        // The receiver and an argument render their generic parameter names
        // (`T`, `U`) while a genuinely concrete return stays concrete (`@bit8`).
        let src = "gen[T, U]: function (value: T, extra: U) out: @bit8 {\n    out = value.other(extra)\n}\n\nmain: function () nothing {\n    x: @bit8 = gen[@bit32, @bit64](0x1, 0x2)\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
        assert!(
            rendered.contains("fn other(self: ref T, U) -> @bit8"),
            "missing generic-param signature: {rendered}"
        );
    }

    #[test]
    fn concrete_return_stays_concrete_in_required_signature() {
        // A return type that is not one of the instantiation arguments (a user
        // object) must render concretely, never as a generic parameter.
        let src = "Widget: object {\n    value: @bit32\n}\n\ngen[T]: function (value: T) out: Widget {\n    out = value.other(5)\n}\n\nmain: function () nothing {\n    x: Widget = gen[@bit32](0x1)\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
        assert!(
            rendered.contains("fn other(self: ref T, @byte_buffer) -> Widget"),
            "concrete return not rendered concretely: {rendered}"
        );
    }

    #[test]
    fn field_miss_shows_required_field_note() {
        // A field read on a generic-derived value gets the field form of the
        // inferred note, using the generic parameter for the expected type.
        let src = "gen[T]: function (value: T) out: T {\n    out = value.data\n}\n\nmain: function () nothing {\n    x: @bit32 = gen[@bit32](0x1)\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
        assert!(
            rendered.contains("missing member `data` on `@bit32`"),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "based on this usage, the generic body requires a field matching:\n      data: T"
            ),
            "missing field note: {rendered}"
        );
    }

    #[test]
    fn concrete_field_type_stays_concrete() {
        // The field note's type comes from the use-site expected type; a user
        // object stays concrete rather than becoming a generic parameter.
        let src = "Widget: object {\n    value: @bit32\n}\n\ngen[T]: function (value: T) out: Widget {\n    out = value.data\n}\n\nmain: function () nothing {\n    x: Widget = gen[@bit32](0x1)\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
        assert!(
            rendered.contains("requires a field matching:\n      data: Widget"),
            "concrete field type not rendered concretely: {rendered}"
        );
    }

    #[test]
    fn concrete_field_miss_has_no_inferred_note() {
        // An ordinary field miss outside a generic instantiation keeps the
        // plain `E021` shape with no inferred-requirement note.
        let src = "Object: object {\n    value: @bit32\n}\n\nmain: function () nothing {\n    o: Object = Object {\n        value = 0x1\n    }\n    x: @bit32 = o.data\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        assert!(
            rendered.contains("no field `data` on `Object`"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("requires a field matching"),
            "concrete field miss gained a note: {rendered}"
        );
    }

    #[test]
    fn no_cascade_one_error() {
        let src = "gen_function[T]: function () out: T {\n    out = T.other()\n}\n\nmain: function () nothing {\n    a: @bit32 = gen_function[@bit32]()\n    b: @bit32 = gen_function[@bit32]()\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
    }

    #[test]
    fn failing_object_method_reported_at_use_site() {
        // A missing member inside a generic object method points the `E042`
        // primary at the failing use in the method body; the instantiation note
        // carries the pretty method instance rather than a mangled symbol.
        let src = "GenObject[T]: object {\n    value: T\n    get: function (self) result: T {\n        result = self.other()\n    }\n}\n\nmain: function () nothing {\n    variable: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n}\n";
        let (diags, _, _, sm) = run(src);
        let e042: Vec<_> = diags
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.code == "E042")
            .collect();
        assert_eq!(e042.len(), 1, "{diags:?}");
        let primary = e042[0]
            .labels
            .iter()
            .find(|label| label.primary)
            .expect("E042 has a primary label");
        let (line, _) = sm.line_col(primary.span).expect("primary span has a file");
        assert_eq!(line, 4, "primary should be the body use: {diags:?}");
        let rendered = render_all(&diags, &sm);
        assert!(
            rendered.contains("required by instantiation of GenObject.get[@bit32]"),
            "{rendered}"
        );
        assert!(rendered.contains("missing method `other`"), "{rendered}");
        assert!(!rendered.contains(".1.."), "{rendered}");
    }

    #[test]
    fn field_miss_has_clean_tone() {
        // A field lookup miss inside a generic body must not leak the internal
        // milestone label; it reads as a requirement phrase.
        let src = "Object: object {\n    value: @bit32\n}\n\ngen[T]: function (v: T) out: @bit8 {\n    out = v.data\n}\n\nmain: function () nothing {\n    o: Object = Object {\n        value = 0x1\n    }\n    x: @bit8 = gen[Object](o)\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        assert_eq!(rendered.matches("error[E042]").count(), 1, "{rendered}");
        assert!(
            rendered.contains("missing member `data` on `Object`"),
            "{rendered}"
        );
        assert!(!rendered.contains("M4"), "{rendered}");
        assert!(!rendered.contains("E040"), "{rendered}");
    }

    #[test]
    fn generic_instance_miss_has_did_you_mean() {
        // The lookup owner is an instance's display form
        // (`GenObject[@bit32]`); did-you-mean must still find the base's
        // members.
        let src = "GenObject[T]: object {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\n\ngen[T]: function (value: T) out: @bit32 {\n    out = value.geet()\n}\n\nmain: function () nothing {\n    v: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    x: @bit32 = gen[GenObject[@bit32]](v)\n}\n";
        let (diags, _, _, sm) = run(src);
        let rendered = render_all(&diags, &sm);
        assert!(
            rendered.contains("`GenObject[@bit32]` has `get`, did you mean `get`?"),
            "{rendered}"
        );
    }

    #[test]
    fn generic_function_monomorphizes() {
        let (diags, hir, _) = check(
            "gen_function[T]: function (in: T) out: T {\n    out = in\n}\n\nmain: function () nothing {\n    out: @bit32 = gen_function[@bit32](0x1)\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let names = names(&hir);
        assert!(
            names.contains(&"gen_function.1..bit32"),
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
        let note = diags
            .diagnostics()
            .iter()
            .find(|d| d.code == "E051")
            .and_then(|d| d.notes.first())
            .cloned()
            .unwrap_or_default();
        assert!(note.contains("grow[@bit32]"), "readable chain: {note}");
        assert!(
            note.contains("grow[Box[@bit32]]"),
            "readable nested arg: {note}"
        );
        assert!(!note.contains(".1.."), "mangled symbol leaked: {note}");
    }

    #[test]
    fn cyclic_growing_object_layout_is_e051() {
        let (diags, _, _) = check(
            "GenObj[T]: object {\n    next: GenObj[GenObj[T]]\n}\n\nconsume[T]: function () nothing {\n}\n\nmain: function () nothing {\n    consume[GenObj[@bit32]]()\n}\n",
        );
        let count = diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E051")
            .count();
        assert_eq!(count, 1, "{diags:?}");
        let note = diags
            .diagnostics()
            .iter()
            .find(|d| d.code == "E051")
            .and_then(|d| d.notes.first())
            .cloned()
            .unwrap_or_default();
        assert!(note.contains("GenObj[@bit32]"), "readable chain: {note}");
        assert!(
            note.contains("GenObj[GenObj[@bit32]]"),
            "readable nested arg: {note}"
        );
        assert!(!note.contains(".1.."), "mangled symbol leaked: {note}");
    }

    #[test]
    fn bare_generic_object_static_is_e044() {
        let (diags, _, _) = check(
            "GenObj[T]: object {\n    value: T\n    ping: function () nothing {\n    }\n}\n\nmain: function () nothing {\n    GenObj.ping()\n}\n",
        );
        assert!(
            diags.diagnostics().iter().any(|d| d.code == "E044"),
            "{diags:?}"
        );
        assert!(
            !diags.diagnostics().iter().any(|d| d.code == "E070"),
            "{diags:?}"
        );
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
            .filter(|func| func.name.starts_with("rec."))
            .map(|func| func.name.as_str())
            .collect();
        assert_eq!(
            instances,
            vec!["rec.1..bit32"],
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
            .get("GenObject.1..bit32")
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
            names(&hir).contains(&"GenObject.1..bit32.get"),
            "missing monomorphized method: {:?}",
            names(&hir)
        );
        assert_no_param(&types, &hir);
    }

    #[test]
    fn generic_box_on_drop_registers() {
        let (diags, hir, types) = check(
            "GenBox[T]: object {\n    value: T\n    @on_drop: function (from: ref GenBox[T]) nothing {\n    }\n}\n\nmain: function () nothing {\n    box: GenBox[@bit32] = GenBox {\n        value = 0x0\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(
            hir.drops.get("GenBox.1..bit32").map(String::as_str),
            Some("GenBox.1..bit32.@on_drop"),
            "drops: {:?}",
            hir.drops
        );
        assert!(
            names(&hir).contains(&"GenBox.1..bit32.@on_drop"),
            "missing monomorphized on_drop: {:?}",
            names(&hir)
        );
        assert_no_param(&types, &hir);
    }

    #[test]
    fn generic_object_member_on_drop_instantiates() {
        let (diags, hir, types) = check(
            "GenBox[T]: object {\n    value: T\n    @on_drop: function (from: ref GenBox[T]) nothing {\n    }\n}\n\nmain: function () nothing {\n    box: GenBox[@bit32] = GenBox {\n        value = 0x0\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(
            hir.drops.get("GenBox.1..bit32").map(String::as_str),
            Some("GenBox.1..bit32.@on_drop"),
            "drops: {:?}",
            hir.drops
        );
        assert!(
            names(&hir).contains(&"GenBox.1..bit32.@on_drop"),
            "missing monomorphized on_drop: {:?}",
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
            names(&hir).contains(&"GenObject.1..bit32.get"),
            "missing instance static method: {:?}",
            names(&hir)
        );
    }

    #[test]
    fn method_own_generics_mangle_distinctly() {
        // A method's own generic arguments must be part of its symbol, or two
        // specializations collide (`GenObject.1..bit32.get` twice).
        let (diags, hir, _) = check(
            "GenObject[T]: object {\n    value: T\n    get[U]: function (x: U) result: @bit32 {\n        result = 0x2\n    }\n}\n\ngen[T]: function () out: @bit32 {\n    out = T.get[@bit32](0x0)\n}\n\ngen2[T]: function () out: @bit32 {\n    out = T.get[@bit64](0x0)\n}\n\nmain: function () nothing {\n    a: @bit32 = gen[GenObject[@bit32]]()\n    b: @bit32 = gen2[GenObject[@bit32]]()\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        let names = names(&hir);
        assert!(
            names.contains(&"GenObject.1..bit32.get.1..bit32"),
            "missing method-own-arg instance: {names:?}"
        );
        assert!(
            names.contains(&"GenObject.1..bit32.get.1..bit64"),
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
