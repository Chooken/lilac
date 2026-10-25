//! `@array(T, N)`: the type syntax, the zeroed literal `@array(T, N) {}`,
//! `@array_at`/`@array_len`, and the rule that elements have no RAII hooks.

use std::collections::HashMap;

use lilac_ast::{Expr, StructField, Type as AstType};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::intrinsics::{Intrinsic, Slot};
use lilac_hir::{HirCallee, HirExpr, HirExprKind, Layout, SemanticKind};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeKind, TypeRef};

use crate::body::FnCx;
use crate::calls::Called;
use crate::literals::integer_magnitude;
use crate::{Checker, Requirement};

impl Checker<'_> {
    /// Resolves `@array(T, N)` syntax: `T` by value, `N` a literal from 1
    /// to `u32::MAX` (LLVM's array limit).
    pub(crate) fn resolve_array(
        &mut self,
        ty: &AstType,
        element: &AstType,
        len: &str,
        len_span: Span,
    ) -> TypeRef {
        if !ty.name_span.is_dummy() {
            self.record_semantic(ty.name_span, SemanticKind::BuiltinType);
        }
        let elem = self.resolve_syntax(element);
        if elem.is_error() {
            return TypeRef::ERROR;
        }
        if elem.is_ref {
            self.diags.emit(
                Diagnostic::error(codes::E010, "an array element cannot be a `ref`")
                    .label(element.span, true, "a reference element")
                    .help("wrap the reference in a structure"),
            );
            return TypeRef::ERROR;
        }
        let len = integer_magnitude(len)
            .and_then(|len| u32::try_from(len).ok())
            .filter(|len| *len > 0);
        let Some(len) = len else {
            self.diags.emit(
                Diagnostic::error(codes::E010, "an array length must be from 1 to 0xffffffff")
                    .label(len_span, true, "not a valid length"),
            );
            return TypeRef::ERROR;
        };
        TypeRef {
            id: self.types.intern_array(elem.id, u64::from(len)),
            is_ref: ty.is_ref,
        }
    }

    /// `@array(T, N) {}`: a zeroed array. An array has no fields to set.
    pub(crate) fn array_literal(
        &mut self,
        ty: TypeRef,
        fields: &[StructField],
        span: Span,
    ) -> HirExpr {
        if let Some(field) = fields.first() {
            self.diags.emit(
                Diagnostic::error(codes::E010, "an array literal takes no fields")
                    .label(field.span, true, "not a field of an array")
                    .help("write `@array(T, N) {}` and set elements with `@array_at`"),
            );
            return HirExpr::poison(span);
        }
        HirExpr {
            ty,
            span,
            kind: HirExprKind::Struct(Vec::new()),
        }
    }

    /// `@array_at(ref array, index)`: a `ref` to the element, bounds
    /// checked at run time.
    pub(crate) fn check_array_at(
        &mut self,
        fcx: &mut FnCx,
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        let arity = self.intrinsic_arity("@array_at", 2, args.len(), span);
        self.callee(fcx, args, arity)?;
        let array = self.check_expr(fcx, &args[0], None);
        if array.ty.is_error() {
            // The array reported it.
            return Err(self.callee_failed(fcx, &args[1..], self.diags.already_reported()));
        }
        let elem = match self.types.kind(array.ty.id) {
            TypeKind::Array { elem, .. } if array.ty.is_ref => *elem,
            _ => {
                let found = self.show(array.ty);
                let diagnostic = Diagnostic::error(
                    codes::E010,
                    format!("mismatched types: expected a `ref` array, found `{found}`"),
                )
                .label(args[0].span(), true, "pass `ref array`");
                return Err(self.emit_requirement(
                    diagnostic,
                    Requirement::Mismatch {
                        expected_ref: false,
                    },
                    &[array.ty.id],
                ));
            }
        };
        let index = self
            .intrinsic_arg(
                fcx,
                &args[1],
                Slot::Builtin(Builtin::BitNative),
                false,
                span,
            )
            .ok_or_else(|| self.diags.already_reported())?;
        Ok(Called::Call {
            callee: HirCallee::Intrinsic(Intrinsic::ArrayAt),
            args: vec![array, index],
            outs: vec![TypeRef::reference(elem)],
        })
    }

    /// `@array_len(Type)` / `@array_len(value)`: the length as a
    /// `@bit_native` literal.
    pub(crate) fn check_array_len(
        &mut self,
        fcx: &mut FnCx,
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        self.intrinsic_arity("@array_len", 1, args.len(), span)?;
        let ty = self.size_of_operand(fcx, &args[0])?;
        let TypeKind::Array { len, .. } = self.types.kind(ty.id) else {
            let found = self.show(ty);
            return Err(self.diags.emit(
                Diagnostic::error(
                    codes::E010,
                    format!("mismatched types: expected an array, found `{found}`"),
                )
                .label(args[0].span(), true, "not an array"),
            ));
        };
        Ok(Called::Value(HirExpr {
            ty: Builtin::BitNative.value(),
            span,
            kind: HirExprKind::Int(*len),
        }))
    }

    /// Remembers a concrete array type for [`Self::check_array_hooks`].
    pub(crate) fn note_array(&mut self, id: TypeId, span: Span) {
        if self.noted_arrays.insert(id) {
            self.arrays.push((id, span));
        }
    }

    /// `E010` for each array whose element has an RAII hook, itself or
    /// inside: arrays copy and drop as plain bytes. Runs once hooks are
    /// bound.
    pub(crate) fn check_array_hooks(&mut self) {
        let mut known = HashMap::new();
        for (id, span) in std::mem::take(&mut self.arrays) {
            let TypeKind::Array { elem, .. } = *self.types.kind(id) else {
                continue;
            };
            if self.has_hooks(elem, &mut known) {
                let elem = self.show_id(elem);
                self.diags.emit(
                    Diagnostic::error(
                        codes::E010,
                        format!("array element `{elem}` has RAII hooks"),
                    )
                    .label(span, true, "arrays hold plain data only")
                    .note("an array copies and drops its elements as plain bytes"),
                );
            }
        }
    }

    /// Whether `id` or anything it holds by value has an RAII hook. Each
    /// type is walked once across every array (`known`). By-value cycles
    /// have no layout (`E053`), so a type met again while it is walked
    /// (only possible through one) counts as hook-free.
    fn has_hooks(&self, id: TypeId, known: &mut HashMap<TypeId, bool>) -> bool {
        if let Some(&hooks) = known.get(&id) {
            return hooks;
        }
        known.insert(id, false);
        let hooks = self.out.hooks.contains_key(&id)
            || if let TypeKind::Array { elem, .. } = self.types.kind(id) {
                self.has_hooks(*elem, known)
            } else {
                let slots: Vec<TypeRef> = match self.out.layouts.get(&id) {
                    Some(Layout::Object { fields }) => fields.iter().map(|(_, ty)| *ty).collect(),
                    Some(Layout::Enum { variants, .. }) => {
                        variants.iter().filter_map(|(_, ty)| *ty).collect()
                    }
                    None => Vec::new(),
                };
                slots
                    .into_iter()
                    .any(|slot| !slot.is_ref && self.has_hooks(slot.id, known))
            };
        known.insert(id, hooks);
        hooks
    }
}
