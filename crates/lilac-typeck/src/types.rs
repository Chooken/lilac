//! Type syntax, concrete type layouts, and generic type instances.
//!
//! Every concrete object/enum type that occurs gets a [`Layout`] the first time
//! it is mentioned (`ensure_type`). For an instance of a generic type that is
//! also when its member hooks are instantiated and registered, so operators,
//! conversions and RAII hooks of `Box[@bit32]` exist as soon as the type does.

use lilac_ast::{FuncType, Type as AstType, TypeShape};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::{FuncKey, Layout, SemanticKind, TypeHooks};
use lilac_resolve::{DefId, DefKind};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeKind, TypeRef};

use crate::generics::Failure;
use crate::paths::path_tail;
use crate::{Checker, TypeState, MAX_RECURSION, MAX_TYPE_NESTING};

impl Checker<'_> {
    /// Resolves type syntax in the current environment: generic parameter
    /// names become the environment's arguments (or `Param` placeholders in a
    /// template), and every type mentioned gets its layout: a template's
    /// too, unsized, so its fields and variants are known
    /// (`o: Optional[ref T] = .None` in a generic body).
    pub(crate) fn resolve_type(&mut self, ty: &AstType) -> TypeRef {
        let template = self.resolve_syntax(ty);
        if template.is_error() {
            return template;
        }
        let resolved = if self.env.args.is_empty() {
            template
        } else {
            let args = self.env.args.clone();
            self.types.substitute(template, &args)
        };
        if self.ensure_syntax(resolved, ty).is_err() {
            return TypeRef::ERROR;
        }
        resolved
    }

    /// [`Self::ensure_type`] for a type resolved from `syntax`: an array
    /// written in it is laid out at its own length, so an array too large
    /// for the target (`E067`) is reported at the length that made it so.
    fn ensure_syntax(&mut self, ty: TypeRef, syntax: &AstType) -> Result<(), ErrorGuaranteed> {
        if let (
            TypeShape::Array {
                element, len_span, ..
            },
            TypeKind::Array { elem, .. },
        ) = (&syntax.shape, self.types.kind(ty.id).clone())
        {
            self.ensure_syntax(TypeRef::value(elem), element)?;
            self.note_array(ty.id, syntax.span);
            return self.lay_out_array(ty.id, *len_span);
        }
        self.ensure_type(ty, syntax.span)
    }

    /// Resolves type syntax to a template (parameters stay `Param`).
    pub(crate) fn resolve_syntax(&mut self, ty: &AstType) -> TypeRef {
        match &ty.shape {
            // The parser already reported it.
            TypeShape::Error => TypeRef::ERROR,
            TypeShape::Function(func) => {
                if !ty.is_ref {
                    self.bare_function_type(ty);
                }
                let id = self.resolve_function(func);
                // A bare one recovers as the reference it should be.
                self.function_value(ty, TypeRef::reference(id))
            }
            TypeShape::Array {
                element,
                len,
                len_span,
            } => self.resolve_array(ty, element, len, *len_span),
            TypeShape::FormatArgs { writer } => self.resolve_format_args(ty, writer),
            // Outside a method's receiver, `self` names no type.
            TypeShape::SelfType => self.resolve_named(ty, "self", &[], false),
            TypeShape::Named { name, args } => self.resolve_named(ty, name, args, false),
        }
    }

    /// The function type `func` spells, failure outs included. Only an
    /// alias's target may be one without `ref`, and only it may fail;
    /// every other caller checks both ([`Checker::function_value`]).
    pub(crate) fn resolve_function(&mut self, func: &FuncType) -> TypeId {
        let mut slots = |slots: &[lilac_ast::Param]| -> Vec<TypeRef> {
            slots
                .iter()
                .map(|slot| self.resolve_syntax(&slot.ty))
                .collect()
        };
        let params = slots(&func.params);
        let returns = slots(&func.returns);
        let failures = func.failures.as_deref().map(slots);
        self.types
            .intern_function(params, returns, failures, func.is_export)
    }

    /// `ty`, the type of a value written at `syntax`: a function type that
    /// can fail is `E057`, since no function value can fail yet.
    pub(crate) fn function_value(&mut self, syntax: &AstType, ty: TypeRef) -> TypeRef {
        let TypeKind::Func {
            failures: Some(_), ..
        } = self.types.kind(ty.id)
        else {
            return ty;
        };
        self.diags.emit(
            Diagnostic::error(codes::E057, "a function value cannot fail")
                .label(syntax.span, true, "this function type has `else` outs")
                .help(
                    "only a named function can fail: call it directly, or wrap the call in a function that handles the failure",
                ),
        );
        TypeRef::ERROR
    }

    /// `E077` for a function type written without `ref`, at `ty`: its fix
    /// inserts the `ref`.
    fn bare_function_type(&mut self, ty: &AstType) {
        let keyword = if ty.name_span.is_dummy() {
            ty.span
        } else {
            ty.name_span
        };
        self.diags.emit(
            Diagnostic::error(codes::E077, "a function type is a value only behind `ref`")
                .label(keyword, true, "write `ref function`")
                .fix(
                    Span::new(ty.span.lo.0, ty.span.lo.0, ty.span.file),
                    "ref ",
                    "a function value is a reference to its code: add `ref`",
                ),
        );
    }

    /// `E077` for an alias of a function type used without `ref`, at `ty`.
    fn bare_function_alias(&mut self, ty: &AstType, name: &str) {
        self.diags.emit(
            Diagnostic::error(
                codes::E077,
                format!("`{name}` is a function type, a value only behind `ref`"),
            )
            .label(ty.span, true, format!("write `ref {name}`"))
            .fix(
                Span::new(ty.span.lo.0, ty.span.lo.0, ty.span.file),
                "ref ",
                "a function value is a reference to its code: add `ref`",
            ),
        );
    }

    /// The type `ty`, a use of the alias `def` named `name`, stands for.
    /// `bare_function` as in [`Checker::resolve_named`].
    fn resolve_alias_use(
        &mut self,
        ty: &AstType,
        name: &str,
        def: DefId,
        args: &[AstType],
        bare_function: bool,
    ) -> TypeRef {
        if !args.is_empty() {
            self.arity_error(ty, name, Some(def), args.len());
            return TypeRef::ERROR;
        }
        // The alias's own error was reported at its declaration.
        let target = self.alias_type(def, ty.name_span);
        if target.is_error() {
            return TypeRef::ERROR;
        }
        if target.is_ref && ty.is_ref {
            self.diags.emit(
                Diagnostic::error(
                    codes::E010,
                    format!("`{name}` is already a reference; it cannot take `ref`"),
                )
                .label(ty.span, true, format!("write `{name}`"))
                .help("double references are invalid unless wrapped inside a structure"),
            );
            return TypeRef::ERROR;
        }
        // A bare function type lives only behind `ref`, but an alias's
        // target (`bare_function`) names one as it is.
        let bare = !target.is_ref && !ty.is_ref && self.types.is_func(target.id);
        if bare && bare_function {
            return target;
        }
        if bare {
            self.bare_function_alias(ty, name);
        }
        let value = TypeRef {
            id: target.id,
            is_ref: ty.is_ref || target.is_ref || bare,
        };
        self.function_value(ty, value)
    }

    /// Resolves a named type: a generic parameter, a builtin, or an
    /// object/enum with its arguments. `bare_function`: an alias of a bare
    /// function type may be named without `ref` (an alias's target).
    pub(crate) fn resolve_named(
        &mut self,
        ty: &AstType,
        name: &str,
        args: &[AstType],
        bare_function: bool,
    ) -> TypeRef {
        let name_span = path_tail(ty.name_span, name);
        if !ty.name_span.is_dummy() {
            let kind = if self.types.builtin(name).is_some() {
                SemanticKind::BuiltinType
            } else {
                SemanticKind::UserType
            };
            self.record_semantic(name_span, kind);
        }
        if let Some(index) = self.env.params.iter().position(|param| param == name) {
            if !args.is_empty() {
                self.arity_error(ty, name, None, args.len());
                return TypeRef::ERROR;
            }
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            let id = self.types.intern_param(index, name);
            return TypeRef {
                id,
                is_ref: ty.is_ref,
            };
        }
        if let Some(id) = self.types.builtin(name) {
            if !args.is_empty() {
                self.arity_error(ty, name, None, args.len());
                return TypeRef::ERROR;
            }
            if name == "unknown" && !ty.is_ref {
                self.diags.emit(
                    Diagnostic::error(codes::E023, "`unknown` can only live behind a reference")
                        .label(ty.span, true, "write `ref unknown`"),
                );
                return TypeRef::ERROR;
            }
            return TypeRef {
                id,
                is_ref: ty.is_ref,
            };
        }
        let Ok(found) = self.lookup_item(name, ty.name_span) else {
            return TypeRef::ERROR;
        };
        if let Some(def) = found.filter(|def| matches!(self.res.def(*def).kind, DefKind::Alias(_)))
        {
            self.record_use(name_span, def);
            return self.resolve_alias_use(ty, name, def, args, bare_function);
        }
        let Some(def) = found.filter(|def| self.res.def(*def).is_type()) else {
            let diagnostic = Diagnostic::error(codes::E023, format!("unknown type `{name}`"))
                .label(ty.span, true, "no such type in scope");
            let diagnostic = if name == "@format_args" {
                diagnostic
                    .help("a format names the type its holes are written into: `@format_args(W)`")
            } else if self.is_owner_param(name) {
                self.owner_param_note(diagnostic, name)
            } else {
                self.suggest_type(diagnostic, name, ty.name_span)
            };
            self.diags.emit(diagnostic);
            return TypeRef::ERROR;
        };
        self.record_use(name_span, def);
        let declared = self.res.def(def).generic_params.len();
        if args.len() != declared {
            self.arity_error(ty, name, Some(def), args.len());
            return TypeRef::ERROR;
        }
        let mut resolved_args = Vec::with_capacity(declared);
        for arg in args {
            let arg = self.resolve_syntax(arg);
            if arg.is_error() {
                return TypeRef::ERROR;
            }
            resolved_args.push(arg);
        }
        let mut resolved = self.type_of_def(def, resolved_args);
        resolved.is_ref = ty.is_ref;
        resolved
    }

    /// Whether `name` is a generic parameter of the type being checked that
    /// is not in scope: only a member constant is checked with its type as
    /// owner but without the type's parameters.
    pub(crate) fn is_owner_param(&self, name: &str) -> bool {
        self.env.owner.is_some_and(|owner| {
            !self.env.params.iter().any(|param| param == name)
                && self
                    .res
                    .def(owner)
                    .generic_params
                    .iter()
                    .any(|param| param == name)
        })
    }

    /// Says why a member constant cannot name its type's parameter `name`.
    pub(crate) fn owner_param_note(&self, diagnostic: Diagnostic, name: &str) -> Diagnostic {
        let owner = self
            .env
            .owner
            .map_or("", |owner| self.res.def(owner).name.as_str());
        diagnostic.note(format!(
            "a constant of `{owner}` is one value for every instance of it, so it cannot name `{name}`"
        ))
    }

    /// `E044` for a type applied to the wrong number of arguments.
    fn arity_error(&mut self, ty: &AstType, name: &str, def: Option<DefId>, found: usize) {
        let declared = def.map_or_else(Vec::new, |def| self.res.def(def).generic_params.clone());
        let noun = if declared.len() == 1 {
            "argument"
        } else {
            "arguments"
        };
        let mut diagnostic = Diagnostic::error(
            codes::E044,
            format!("wrong number of generic arguments for `{name}`"),
        )
        .label(
            ty.span,
            true,
            format!("expected {} generic {noun}, found {found}", declared.len()),
        );
        match def {
            Some(def) if !declared.is_empty() => {
                diagnostic = diagnostic.label(
                    self.res.def(def).span,
                    false,
                    format!("declared with `[{}]`", declared.join(", ")),
                );
            }
            _ => diagnostic = diagnostic.note(format!("`{name}` is not generic")),
        }
        self.diags.emit(diagnostic);
    }

    /// The type `def[args]` (a plain object/enum when `args` is empty).
    pub(crate) fn type_of_def(&mut self, def: DefId, args: Vec<TypeRef>) -> TypeRef {
        let name = self.res.qualified_name(def);
        self.types.set_object_name(def, &name);
        let id = if args.is_empty() {
            self.types.intern_object(def)
        } else {
            self.types.intern_instance(def, args)
        };
        TypeRef::value(id)
    }

    /// The template type of a type definition (`Box[T]` with `T` as `Param`).
    pub(crate) fn template_of(&mut self, def: DefId) -> TypeRef {
        let params = self.res.def(def).generic_params.clone();
        let args = params
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let index = u32::try_from(index).unwrap_or(u32::MAX);
                TypeRef::value(self.types.intern_param(index, name))
            })
            .collect();
        self.type_of_def(def, args)
    }

    /// The definition and arguments behind an object/enum type.
    pub(crate) fn type_parts(&self, id: TypeId) -> Option<(DefId, Vec<TypeRef>)> {
        match self.types.kind(id) {
            TypeKind::Object(def) => Some((*def, Vec::new())),
            TypeKind::Instance { base, args } => Some((*base, args.clone())),
            _ => None,
        }
    }

    /// Makes sure a concrete type (and everything it contains) has a layout
    /// and its hooks. A by-value cycle is `E053`. A definition laid out inside
    /// itself past [`MAX_RECURSION`] times (a generic type that keeps growing),
    /// or types nested past [`MAX_TYPE_NESTING`], is `E051`; once a definition
    /// recursed too deep, a later layout of it inside itself fails with that
    /// error, so a type that grows in two fields stops in linear time.
    ///
    /// A function or array type is walked once, like an object
    /// ([`Checker::ensured`]), so one nested `n` deep whose text doubles at
    /// each level (`function (T, T)` again and again) costs `n` steps, not
    /// `2^n`.
    pub(crate) fn ensure_type(&mut self, ty: TypeRef, span: Span) -> Result<(), ErrorGuaranteed> {
        self.ensure(ty, span, true)
    }

    /// [`Checker::ensure_type`]; with `defer`, a `ref` met while a type is
    /// laid out waits for that type ([`Checker::deferred_refs`]).
    fn ensure(&mut self, ty: TypeRef, span: Span, defer: bool) -> Result<(), ErrorGuaranteed> {
        match self.types.kind(ty.id).clone() {
            TypeKind::Func {
                params,
                returns,
                failures,
                ..
            } => {
                if self.ensured.contains(&ty.id) {
                    return Ok(());
                }
                for slot in params
                    .iter()
                    .chain(returns.iter())
                    .chain(failures.iter().flatten())
                {
                    self.ensure_type(*slot, span)?;
                }
                self.ensured.insert(ty.id);
                return Ok(());
            }
            TypeKind::Array { elem, .. } => {
                if self.ensured.contains(&ty.id) {
                    return Ok(());
                }
                self.ensure_type(TypeRef::value(elem), span)?;
                self.note_array(ty.id, span);
                // Not remembered when too large: each place that names it
                // is an error (`lay_out_array`).
                self.lay_out_array(ty.id, span)?;
                self.ensured.insert(ty.id);
                return Ok(());
            }
            TypeKind::FormatArgs { writer } => {
                return self.ensure_type(TypeRef::value(writer), span);
            }
            TypeKind::Object(_) | TypeKind::Instance { .. } => {}
            _ => return Ok(()),
        }
        match self.type_states.get(&ty.id) {
            Some(TypeState::Done) => return Ok(()),
            // Reported when its layout first failed, maybe held by a failed
            // instance until the end of the check.
            Some(TypeState::Failed) => {
                return Err(match self.failures.first().and_then(Failure::held) {
                    Some(held) => ErrorGuaranteed::held(held),
                    None => self.diags.already_reported(),
                });
            }
            Some(TypeState::Active) if ty.is_ref => return Ok(()),
            // Laid out once the type being laid out is.
            None if defer && ty.is_ref && !self.type_stack.is_empty() => {
                self.deferred_refs.push((ty, span));
                return Ok(());
            }
            Some(TypeState::Active) => {
                let name = self.show_id(ty.id);
                return Err(self.diags.emit(
                    Diagnostic::error(codes::E053, format!("`{name}` contains itself by value"))
                        .label(
                            span,
                            true,
                            "this field would make the type infinitely large",
                        )
                        .help("store it behind a `ref` instead"),
                ));
            }
            None => {}
        }
        let Some((def, args)) = self.type_parts(ty.id) else {
            return Ok(());
        };
        let count = self.type_counts.get(&def).copied().unwrap_or(0) + 1;
        if count > 1 && self.type_recursion_limited.contains(&def) {
            // The descent this definition started already reported `E051`.
            if let Some(error) = self.recursion_error {
                self.type_states.insert(ty.id, TypeState::Failed);
                return Err(error);
            }
        }
        if count > MAX_RECURSION || self.type_stack.len() >= MAX_TYPE_NESTING {
            self.type_states.insert(ty.id, TypeState::Failed);
            let error = self.type_depth_error(def, ty.id, span);
            if count > MAX_RECURSION {
                self.type_recursion_limited.insert(def);
                self.recursion_error.get_or_insert(error);
            }
            return Err(error);
        }
        self.type_states.insert(ty.id, TypeState::Active);
        self.type_stack.push(ty.id);
        self.type_counts.insert(def, count);
        let mark = self.deferred_refs.len();
        let laid_out = self.lay_out_active(ty, def, &args, span);
        // A `ref`'s referent never shapes this layout, so it is laid out
        // after it: laying it out inside would find this type's own field
        // types unfinished (`Holder { a: Optional[ref Node] }` laying out
        // `Node`, whose `Optional[ref Node]` is still `Active`) and call
        // them a cycle. This type stays on the stack meanwhile, so a type
        // that grows through a `ref` still meets `MAX_RECURSION`. A
        // referent's own error is its own: this type's layout stands.
        let deferred = self.deferred_refs.split_off(mark);
        if laid_out.is_ok() {
            for (referent, at) in deferred {
                let _ = self.ensure(referent, at, false);
            }
        }
        self.type_stack.pop();
        self.type_counts.insert(def, count - 1);
        laid_out
    }

    /// Lays out `ty` (of `def[args]`), which is on the stack and
    /// `Active`, and marks it `Done` or `Failed`.
    fn lay_out_active(
        &mut self,
        ty: TypeRef,
        def: DefId,
        args: &[TypeRef],
        span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        let env = self.env_of(def, args.to_vec(), None);
        let layout = self.with_env(env, |c| c.compute_layout(def));
        let Ok((layout, spans)) = layout else {
            // `compute_layout` reported it.
            self.type_states.insert(ty.id, TypeState::Failed);
            return Err(self.diags.already_reported());
        };
        if let Err(error) = self.no_format_slots(&layout, &spans) {
            self.type_states.insert(ty.id, TypeState::Failed);
            return Err(error);
        }
        if let Err(error) = self.lay_out_type(ty.id, &layout, &spans) {
            self.type_states.insert(ty.id, TypeState::Failed);
            return Err(error);
        }
        self.out.layouts.insert(ty.id, layout);
        self.type_states.insert(ty.id, TypeState::Done);
        if !args.is_empty() {
            self.register_instance_hooks(def, args, span);
        }
        Ok(())
    }

    /// The definition behind an object/enum type.
    fn type_def(&self, id: TypeId) -> Option<DefId> {
        match self.types.kind(id) {
            TypeKind::Object(def) | TypeKind::Instance { base: def, .. } => Some(*def),
            _ => None,
        }
    }

    /// The layout `E051` for `ty` (of definition `def`), met at `span`
    /// while the types on [`Checker::type_stack`] are laid out: `def` would
    /// appear past [`MAX_RECURSION`] times among them, or they would nest
    /// past [`MAX_TYPE_NESTING`].
    fn type_depth_error(&mut self, def: DefId, ty: TypeId, span: Span) -> ErrorGuaranteed {
        let mut chain = self.type_stack.clone();
        chain.push(ty);
        let shown = |id: TypeId| self.show_id(id);
        let chain_note = format!(
            "instantiation chain: {}",
            elide_chain(chain.len(), |index| shown(chain[index]))
        );
        let name = self.res.def(def).name.clone();
        let recursive = self.type_counts.get(&def).copied().unwrap_or(0) >= MAX_RECURSION;
        if !recursive {
            return self.diags.emit(
                Diagnostic::error(codes::E051, "types nest too deeply to lay out")
                    .label(span, true, format!("`{name}` is nested here"))
                    .note(chain_note)
                    .note(format!(
                        "a type's fields may nest types at most {MAX_TYPE_NESTING} deep"
                    )),
            );
        }
        let mut diagnostic = Diagnostic::error(
            codes::E051,
            "generic instantiation is too deep (possible infinite recursion)",
        )
        .label(span, true, "instantiated here")
        .note(chain_note);
        let mut repeats = chain
            .iter()
            .filter(|laid| self.type_def(**laid) == Some(def))
            .filter_map(|laid| Some((*laid, self.type_parts(*laid)?.1)));
        if let (Some((first, from)), Some((second, to))) = (repeats.next(), repeats.next()) {
            if self.grew(&from, &to) {
                diagnostic = diagnostic.note(format!(
                    "the type arguments grow each time: {} -> {}",
                    shown(first),
                    shown(second)
                ));
            }
        }
        self.diags.emit(diagnostic.note(format!(
            "a type may contain itself at most {MAX_RECURSION} times, and `{name}` would contain itself once more"
        )))
    }

    /// Resolves a type's fields (or variants) in its own environment.
    /// Returns the layout with where each of its [`Layout::slots`] is
    /// declared (an enum's tag at the enum).
    fn compute_layout(&mut self, def: DefId) -> Result<(Layout, Vec<Span>), ErrorGuaranteed> {
        let slots = self.res.def(def).slots.clone();
        let mut failed = false;
        if matches!(self.res.def(def).kind, DefKind::Enum) {
            let mut variants = Vec::with_capacity(slots.len());
            let mut spans = vec![self.res.def(def).name_span];
            for slot in slots {
                let variant = self.res.def(slot);
                let name = variant.name.clone();
                let span = variant.span;
                let payload = match &variant.kind {
                    DefKind::Variant(Some(ty)) => {
                        let ty = ty.clone();
                        let resolved = self.slot_type(&ty, failed);
                        failed |= resolved.is_error();
                        spans.push(span);
                        Some(resolved)
                    }
                    _ => None,
                };
                variants.push((name, payload));
            }
            let tag = Self::tag_type(variants.len());
            // `resolve_type` reported each failed payload.
            return if failed {
                Err(self.diags.already_reported())
            } else {
                Ok((Layout::Enum { variants, tag }, spans))
            };
        }
        let mut fields = Vec::with_capacity(slots.len());
        let mut spans = Vec::with_capacity(slots.len());
        for slot in slots {
            let field = self.res.def(slot);
            let name = field.name.clone();
            let span = field.span;
            let DefKind::Field(ty) = &field.kind else {
                continue;
            };
            let ty = ty.clone();
            let resolved = self.slot_type(&ty, failed);
            failed |= resolved.is_error();
            fields.push((name, resolved));
            spans.push(span);
        }
        // `resolve_type` reported each failed field.
        if failed {
            Err(self.diags.already_reported())
        } else {
            Ok((Layout::Object { fields }, spans))
        }
    }

    /// A field or payload type of a layout being computed. Once a slot
    /// failed, the layout fails, so the later slots are only resolved
    /// (their names checked and reported), not laid out: laying them out
    /// could only add errors that follow from the first, such as a growing
    /// type meeting itself again at every level (`W[T]` holding a
    /// `W[W[T]]` that holds a `W[T]`).
    fn slot_type(&mut self, ty: &AstType, failed: bool) -> TypeRef {
        if failed {
            let _ = self.resolve_syntax(ty);
            return TypeRef::ERROR;
        }
        self.resolve_type(ty)
    }

    /// The smallest bit container holding `count` variant tags.
    pub(crate) fn tag_type(count: usize) -> TypeId {
        match count {
            0..=256 => Builtin::Bit8,
            257..=65_536 => Builtin::Bit16,
            _ => Builtin::Bit32,
        }
        .id()
    }

    /// Instantiates and registers every member hook of `def[args]`. Glue
    /// calls the RAII hooks wherever the type drops or copies, so their
    /// bodies are queued now; every other hook is parked until used
    /// ([`Checker::use_hook`]), so one that fails for these arguments is an
    /// error only where it is used with them.
    fn register_instance_hooks(&mut self, def: DefId, args: &[TypeRef], span: Span) {
        for hook in self.res.def(def).hooks.clone() {
            let key = FuncKey {
                def: hook,
                args: args.to_vec(),
            };
            let raii = matches!(
                self.res.def(hook).name.as_str(),
                "@on_drop" | "@on_copy" | "@on_override"
            );
            let registered = if raii && !self.park_hooks {
                self.request(key.clone(), span)
            } else {
                self.park(key.clone(), span)
            };
            if let Ok(sig) = registered {
                self.add_hook(&key, sig);
            }
        }
    }

    /// Checks every non-generic type declaration (layouts, variant tokens),
    /// except the generated `@builtin` ones.
    pub(crate) fn check_types(&mut self) {
        for id in self.res.ids() {
            let def = self.res.def(id);
            match &def.kind {
                DefKind::Variant(_) => {
                    let span = def.name_span;
                    self.record_semantic(span, SemanticKind::Variant);
                }
                // `@builtin`'s generated types are laid out on first use, so
                // they stay out of programs that never name them.
                DefKind::Object | DefKind::Enum if self.is_builtin(id) => {}
                // Laid out like a type of its own, so an unused alias of a
                // type too large (`E067`) is still reported.
                DefKind::Alias(_) => {
                    let span = def.span;
                    let ty = self.alias_type(id, def.name_span);
                    if !ty.is_error() {
                        let env = self.env_of(id, Vec::new(), None);
                        let _ = self.with_env(env, |c| c.ensure_type(ty, span));
                    }
                }
                DefKind::Object | DefKind::Enum if def.generic_params.is_empty() => {
                    let span = def.span;
                    let ty = self.type_of_def(id, Vec::new());
                    let env = self.env_of(id, Vec::new(), None);
                    let _ = self.with_env(env, |c| c.ensure_type(ty, span));
                }
                DefKind::Object | DefKind::Enum => {
                    // Templates: resolve field syntax once so unknown names
                    // are reported even when the type is never instantiated.
                    let env = self.env_of(id, Vec::new(), None);
                    let slots = def.slots.clone();
                    self.with_env(env, |c| {
                        for slot in slots {
                            match &c.res.def(slot).kind {
                                DefKind::Field(ty) | DefKind::Variant(Some(ty)) => {
                                    let ty = ty.clone();
                                    let _ = c.resolve_syntax(&ty);
                                }
                                _ => {}
                            }
                        }
                    });
                }
                _ => {}
            }
        }
    }

    /// Binds each concrete type's RAII hooks for lowering.
    pub(crate) fn finish_type_hooks(&mut self) {
        let types: Vec<TypeId> = self.out.layouts.keys().copied().collect();
        for ty in types {
            let hooks = TypeHooks {
                drop: self.raii_hook("@on_drop", ty),
                copy: self.raii_hook("@on_copy", ty),
                override_: self.raii_hook("@on_override", ty),
            };
            if hooks.drop.is_some() || hooks.copy.is_some() || hooks.override_.is_some() {
                self.out.hooks.insert(ty, hooks);
            }
        }
    }

    /// The RAII hook `name` whose first parameter is `ref ty`.
    pub(crate) fn raii_hook(&self, name: &str, ty: TypeId) -> Option<FuncKey> {
        self.hooks.get(name)?.iter().find_map(|hook| {
            let first = hook.sig.params.first()?;
            (first.is_ref && first.id == ty).then(|| hook.key.clone())
        })
    }

    /// The fields of an object type: `(name, type, field definition)`.
    pub(crate) fn fields_of(&self, ty: TypeId) -> Option<Vec<(String, TypeRef)>> {
        match self.out.layouts.get(&ty)? {
            Layout::Object { fields } => Some(fields.clone()),
            Layout::Enum { .. } => None,
        }
    }

    /// The variants of an enum type.
    pub(crate) fn variants_of(&self, ty: TypeId) -> Option<Vec<(String, Option<TypeRef>)>> {
        match self.out.layouts.get(&ty)? {
            Layout::Enum { variants, .. } => Some(variants.clone()),
            Layout::Object { .. } => None,
        }
    }
}

/// Joins the `len` chain frames `frame` writes with `->`, eliding the
/// middle past five frames (only the shown ones are written).
pub(crate) fn elide_chain(len: usize, frame: impl Fn(usize) -> String) -> String {
    if len > 5 {
        let head = [frame(0), frame(1)].join(" -> ");
        let tail = [frame(len - 2), frame(len - 1)].join(" -> ");
        format!("{head} -> ... -> {tail} ({len} frames)")
    } else {
        (0..len).map(frame).collect::<Vec<_>>().join(" -> ")
    }
}
