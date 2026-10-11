//! Sizes: every concrete object, enum and array type is laid out by
//! `lilac-layout` for the target when it is first built (`ensure_type`), and
//! one larger than the target allows is `E067`, at the field or array length
//! where it crossed the limit. Templates are never laid out: a size is only
//! known for concrete types.

use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::Layout;
use lilac_layout::{Part, SizeAlign, TooLarge};
use lilac_span::Span;
use lilac_target::Scalar;
use lilac_ty::{TypeId, TypeKind, TypeRef};

use crate::{Checker, Requirement};

impl Checker<'_> {
    /// Lays out the object or enum `id`, whose slots ([`Layout::slots`]) are
    /// all laid out already; `spans[i]` is where slot `i` is declared.
    pub(crate) fn lay_out_type(
        &mut self,
        id: TypeId,
        layout: &Layout,
        spans: &[Span],
    ) -> Result<(), ErrorGuaranteed> {
        // A template instance (`Box[T]`) has no size until `T` is known.
        if self.types.contains_param(id) {
            return Ok(());
        }
        let parts: Vec<SizeAlign> = layout
            .slots()
            .iter()
            .map(|(_, ty)| self.size_of_laid_out(*ty))
            .collect();
        match lilac_layout::struct_layout(&self.target, &parts) {
            Ok(placed) => {
                self.sizes.insert(id, placed.size_align());
                Ok(())
            }
            Err(too_large) => {
                let index = match too_large.part {
                    Part::Field(index) => index,
                    // A struct crosses the limit at a field, never a length.
                    Part::Length => 0,
                };
                let span = spans.get(index).copied().unwrap_or_else(Span::dummy);
                let name = self.types.display(id);
                Err(self.too_large(
                    id,
                    too_large,
                    span,
                    format!("this field brings `{name}` to {} bytes", too_large.size),
                ))
            }
        }
    }

    /// Lays out the array type `id`, whose element is laid out already;
    /// `len_span` is its length, blamed when it crosses the limit.
    pub(crate) fn lay_out_array(
        &mut self,
        id: TypeId,
        len_span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        if self.sizes.contains_key(&id) || self.types.contains_param(id) {
            return Ok(());
        }
        let TypeKind::Array { elem, len } = *self.types.kind(id) else {
            return Ok(());
        };
        let elem = self.size_of_laid_out(TypeRef::value(elem));
        match lilac_layout::array_layout(&self.target, elem, len) {
            Ok(sized) => {
                self.sizes.insert(id, sized);
                Ok(())
            }
            // Not remembered: each place that names the array is an error.
            Err(too_large) => Err(self.too_large(
                id,
                too_large,
                len_span,
                format!("this length makes the array {} bytes", too_large.size),
            )),
        }
    }

    /// `E067` for type `id`, through the requirement path so a generic
    /// instance that builds it fails as `E042`.
    fn too_large(
        &mut self,
        id: TypeId,
        too_large: TooLarge,
        span: Span,
        label: String,
    ) -> ErrorGuaranteed {
        let name = self.types.display(id);
        let diagnostic = Diagnostic::error(
            codes::E067,
            format!("type `{name}` is larger than the target allows"),
        )
        .label(span, true, label)
        .note(format!(
            "an object on `{}` is at most {} bytes",
            self.target, too_large.limit
        ));
        self.emit_requirement(diagnostic, Requirement::Size, &[id])
    }

    /// The size and alignment of a type that is laid out (a scalar, a
    /// pointer, or a type [`Self::lay_out_type`] or
    /// [`Self::lay_out_array`] already measured).
    fn size_of_laid_out(&self, ty: TypeRef) -> SizeAlign {
        let scalar = |scalar| SizeAlign::scalar(&self.target, scalar);
        if ty.is_ref {
            return scalar(Scalar::Pointer);
        }
        match self.types.kind(ty.id) {
            TypeKind::Bit8 => scalar(Scalar::Bit8),
            TypeKind::Bit16 => scalar(Scalar::Bit16),
            TypeKind::Bit32 => scalar(Scalar::Bit32),
            TypeKind::Bit64 => scalar(Scalar::Bit64),
            TypeKind::FBit16 => scalar(Scalar::Float16),
            TypeKind::FBit32 => scalar(Scalar::Float32),
            TypeKind::FBit64 => scalar(Scalar::Float64),
            TypeKind::FBit128 => scalar(Scalar::Float128),
            // `@bit_native` is `uintptr_t`; a function value is a pointer.
            TypeKind::BitNative | TypeKind::Func { .. } => scalar(Scalar::Pointer),
            TypeKind::ByteBuffer => self.byte_buffer(),
            TypeKind::FormatArgs => {
                // A count, then sixteen byte buffers (`lilac_ir::builtin_fields`).
                let mut parts = vec![scalar(Scalar::Pointer)];
                parts.extend(std::iter::repeat_n(self.byte_buffer(), 16));
                self.builtin_struct(&parts)
            }
            TypeKind::Object(_) | TypeKind::Instance { .. } | TypeKind::Array { .. } => {
                match self.sizes.get(&ty.id) {
                    Some(sized) => *sized,
                    None => lilac_error::bug!(
                        Span::dummy(),
                        "`{}` is used before it is laid out",
                        self.types.display(ty.id)
                    ),
                }
            }
            TypeKind::Unknown | TypeKind::Param { .. } | TypeKind::Error => {
                lilac_error::bug!(Span::dummy(), "`{}` has no size", self.types.display(ty.id))
            }
        }
    }

    /// `@byte_buffer`: a length and a pointer (`lilac_ir::builtin_fields`).
    fn byte_buffer(&self) -> SizeAlign {
        let pointer = SizeAlign::scalar(&self.target, Scalar::Pointer);
        self.builtin_struct(&[pointer, pointer])
    }

    /// A builtin aggregate's C struct; always far below any limit.
    fn builtin_struct(&self, parts: &[SizeAlign]) -> SizeAlign {
        match lilac_layout::struct_layout(&self.target, parts) {
            Ok(placed) => placed.size_align(),
            Err(too_large) => lilac_error::bug!(Span::dummy(), "a builtin aggregate: {too_large}"),
        }
    }
}
