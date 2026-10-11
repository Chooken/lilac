//! Type syntax, concrete type layouts, and generic type instances.
//!
//! Every concrete object/enum type that occurs gets a [`Layout`] the first time
//! it is mentioned (`ensure_type`). For an instance of a generic type that is
//! also when its member hooks are instantiated and registered, so operators,
//! conversions and RAII hooks of `Box[@bit32]` exist as soon as the type does.

use lilac_ast::{Type as AstType, TypeShape};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::{FuncKey, Layout, SemanticKind, TypeHooks};
use lilac_resolve::{DefId, DefKind};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeKind, TypeRef};

use crate::paths::path_tail;
use crate::{Checker, TypeState, MAX_DEPTH};

impl Checker<'_> {
    /// Resolves type syntax in the current environment: generic parameter
    /// names become the environment's arguments (or `Param` placeholders in a
    /// template), and every concrete type mentioned gets its layout.
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
        if !self.types.contains_param(resolved.id) && self.ensure_syntax(resolved, ty).is_err() {
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
                if func.failures.is_some() {
                    self.diags.emit(
                        Diagnostic::error(codes::E057, "a function type cannot fail")
                            .label(ty.span, true, "declared with `else` outs")
                            .help("only a named function can fail; call it directly"),
                    );
                    return TypeRef::ERROR;
                }
                let params = func
                    .params
                    .iter()
                    .map(|slot| self.resolve_syntax(&slot.ty))
                    .collect();
                let returns = func
                    .returns
                    .iter()
                    .map(|slot| self.resolve_syntax(&slot.ty))
                    .collect();
                let id = self.types.intern_func_abi(params, returns, func.is_export);
                TypeRef {
                    id,
                    is_ref: ty.is_ref,
                }
            }
            TypeShape::Array {
                element,
                len,
                len_span,
            } => self.resolve_array(ty, element, len, *len_span),
            // Outside a method's receiver, `self` names no type.
            TypeShape::SelfType => self.resolve_named(ty, "self", &[]),
            TypeShape::Named { name, args } => self.resolve_named(ty, name, args),
        }
    }

    /// Resolves a named type: a generic parameter, a builtin, or an
    /// object/enum with its arguments.
    fn resolve_named(&mut self, ty: &AstType, name: &str, args: &[AstType]) -> TypeRef {
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
        let Some(def) = found.filter(|def| self.res.def(*def).is_type()) else {
            let diagnostic = Diagnostic::error(codes::E023, format!("unknown type `{name}`"))
                .label(ty.span, true, "no such type in scope");
            let diagnostic = self.suggest_type(diagnostic, name, ty.name_span);
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
    /// and its hooks. A by-value cycle is `E053`; a layout chain that keeps
    /// growing is `E051`.
    pub(crate) fn ensure_type(&mut self, ty: TypeRef, span: Span) -> Result<(), ErrorGuaranteed> {
        match self.types.kind(ty.id).clone() {
            TypeKind::Func {
                params, returns, ..
            } => {
                for slot in params.iter().chain(returns.iter()) {
                    self.ensure_type(*slot, span)?;
                }
                return Ok(());
            }
            TypeKind::Array { elem, .. } => {
                self.ensure_type(TypeRef::value(elem), span)?;
                self.note_array(ty.id, span);
                return self.lay_out_array(ty.id, span);
            }
            TypeKind::Object(_) | TypeKind::Instance { .. } => {}
            _ => return Ok(()),
        }
        match self.type_states.get(&ty.id) {
            Some(TypeState::Done) => return Ok(()),
            // Reported when its layout first failed, maybe folded into an
            // `E042` that waits for the worklist to drain.
            Some(TypeState::Failed) => {
                return Err(match self.failures.first() {
                    Some((_, pending)) => ErrorGuaranteed::held(pending),
                    None => self.diags.already_reported(),
                });
            }
            Some(TypeState::Active) if ty.is_ref => return Ok(()),
            Some(TypeState::Active) => {
                let name = self.types.display(ty.id);
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
        if self.type_stack.len() >= MAX_DEPTH {
            let mut frames: Vec<String> = self
                .type_stack
                .iter()
                .map(|id| self.types.display(*id))
                .collect();
            frames.push(self.types.display(ty.id));
            self.type_states.insert(ty.id, TypeState::Failed);
            return Err(self.diags.emit(
                Diagnostic::error(
                    codes::E051,
                    "generic instantiation is too deep (possible infinite recursion)",
                )
                .label(span, true, "instantiated here")
                .note(format!("instantiation chain: {}", elide_chain(&frames))),
            ));
        }
        let Some((def, args)) = self.type_parts(ty.id) else {
            return Ok(());
        };
        self.type_states.insert(ty.id, TypeState::Active);
        self.type_stack.push(ty.id);
        let env = self.env_of(def, args.clone(), None);
        let layout = self.with_env(env, |c| c.compute_layout(def));
        self.type_stack.pop();
        let Ok((layout, spans)) = layout else {
            // `compute_layout` reported it.
            self.type_states.insert(ty.id, TypeState::Failed);
            return Err(self.diags.already_reported());
        };
        if let Err(error) = self.lay_out_type(ty.id, &layout, &spans) {
            self.type_states.insert(ty.id, TypeState::Failed);
            return Err(error);
        }
        self.out.layouts.insert(ty.id, layout);
        self.type_states.insert(ty.id, TypeState::Done);
        if !args.is_empty() {
            self.register_instance_hooks(def, &args, span);
        }
        Ok(())
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
                        let resolved = self.resolve_type(&ty);
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
            let resolved = self.resolve_type(&ty);
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

    /// The smallest bit container holding `count` variant tags.
    pub(crate) fn tag_type(count: usize) -> TypeId {
        match count {
            0..=256 => Builtin::Bit8,
            257..=65_536 => Builtin::Bit16,
            _ => Builtin::Bit32,
        }
        .id()
    }

    /// Instantiates and registers every member hook of `def[args]`.
    fn register_instance_hooks(&mut self, def: DefId, args: &[TypeRef], span: Span) {
        for hook in self.res.def(def).hooks.clone() {
            let key = FuncKey {
                def: hook,
                args: args.to_vec(),
            };
            if let Ok(sig) = self.request(key.clone(), span) {
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

/// Joins chain frames with `->`, eliding the middle past five frames.
pub(crate) fn elide_chain(frames: &[String]) -> String {
    if frames.len() > 5 {
        let head = frames[..2].join(" -> ");
        let tail = frames[frames.len() - 2..].join(" -> ");
        format!("{head} -> ... -> {tail} ({} frames)", frames.len())
    } else {
        frames.join(" -> ")
    }
}
