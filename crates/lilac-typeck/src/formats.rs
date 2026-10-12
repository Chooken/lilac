//! F-strings: the type `@format_args(W)`, the `@format` hook each hole is
//! formatted through, the slot rules that make a string literal or one
//! value a format, the three consumer intrinsics, and where a format may
//! live (`E072`).
//!
//! An f-string is data: static pieces, and one borrowed `ref T` per hole
//! with the `@format(ref T, ref W)` hook that formats it. Its `W` comes from
//! where it goes (`fstring_writer`), as a number literal's width does.

use lilac_ast::{Expr, FStrPart, Type as AstType};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::intrinsics::{Intrinsic, Slot};
use lilac_hir::{
    HirCallee, HirExpr, HirExprKind, HirFormat, HirHole, HirStmt, Layout, LocalId, SemanticKind,
};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeKind, TypeRef};

use crate::body::FnCx;
use crate::calls::Called;
use crate::{Checker, Requirement};

/// Why a format may not be somewhere (`E072`).
const LIVES_IN_LOCALS: &str = "a format borrows its holes, so only locals and parameters hold one";

impl Checker<'_> {
    /// Resolves `@format_args(W)` syntax: `W` by value.
    pub(crate) fn resolve_format_args(&mut self, ty: &AstType, writer: &AstType) -> TypeRef {
        if !ty.name_span.is_dummy() {
            self.record_semantic(ty.name_span, SemanticKind::BuiltinType);
        }
        let resolved = self.resolve_syntax(writer);
        if resolved.is_error() {
            return TypeRef::ERROR;
        }
        if resolved.is_ref {
            self.diags.emit(
                Diagnostic::error(
                    codes::E010,
                    "the writer of `@format_args` cannot be a `ref`",
                )
                .label(writer.span, true, "a reference type")
                .help("name the type itself: every formatter takes a `ref` to it"),
            );
            return TypeRef::ERROR;
        }
        TypeRef {
            id: self.types.intern_format_args(resolved.id),
            is_ref: ty.is_ref,
        }
    }

    /// `W` when `id` is `@format_args(W)`.
    pub(crate) fn format_writer(&self, id: TypeId) -> Option<TypeId> {
        match self.types.kind(id) {
            TypeKind::FormatArgs { writer } => Some(*writer),
            _ => None,
        }
    }

    /// `$"..."`: the pieces, and each hole checked, borrowed, and paired
    /// with its `@format` for the `W` the destination gives.
    pub(crate) fn check_fstring(
        &mut self,
        fcx: &mut FnCx,
        parts: &[FStrPart],
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        let (pieces, hole_exprs) = split_parts(parts);
        let writer = self.fstring_writer(expected, span);
        let mut holes = Vec::with_capacity(hole_exprs.len());
        let mut failed = writer.is_err();
        for (expr, hole_span) in hole_exprs {
            // A nested f-string formats into the same `W`; with no `W`, it
            // is checked against the error type, which reports nothing.
            let nested = matches!(expr, Expr::FString { .. }).then(|| match writer {
                Ok(writer) => self.types.intern_format_args(writer),
                Err(_) => TypeId::ERROR,
            });
            let value = self.check_expr(fcx, expr, nested);
            let Ok(writer) = writer else {
                continue;
            };
            match self.hole(value, writer, expr.span()) {
                Some(hole) => {
                    self.record_hole(hole_span, &hole);
                    holes.push(hole);
                }
                None => failed = true,
            }
        }
        match writer {
            Ok(writer) if !failed => self.format_value(writer, pieces, holes, span),
            _ => HirExpr::poison(span),
        }
    }

    /// The `@format_args(writer)` value of `pieces` and `holes`.
    fn format_value(
        &mut self,
        writer: TypeId,
        pieces: Vec<Vec<u8>>,
        holes: Vec<HirHole>,
        span: Span,
    ) -> HirExpr {
        HirExpr {
            ty: TypeRef::value(self.types.intern_format_args(writer)),
            span,
            kind: HirExprKind::Format(HirFormat {
                writer,
                pieces,
                holes,
            }),
        }
    }

    /// The `W` an f-string takes from its destination: an expected
    /// `@format_args(W)`, else the one `@format_args(W)` with a
    /// `@conversion` to the expected type. Nothing expected, or two
    /// candidates, is `E073`; none is `E043`.
    fn fstring_writer(
        &mut self,
        expected: Option<TypeId>,
        span: Span,
    ) -> Result<TypeId, ErrorGuaranteed> {
        let Some(expected) = expected else {
            return Err(self.diags.emit(
                Diagnostic::error(codes::E073, "an f-string needs a type here")
                    .label(span, true, "nothing says what this formats into")
                    .help(
                        "use it where an `@format_args(W)` (or a type that converts from \
                         one) is expected, or give it one with `as`",
                    ),
            ));
        };
        if expected == TypeId::ERROR {
            // The expected type's own error covers it.
            return Err(self.diags.already_reported());
        }
        if let Some(writer) = self.format_writer(expected) {
            return Ok(writer);
        }
        let shown = self.show_id(expected);
        let writers: Vec<TypeId> = self
            .conversion_pairs()
            .into_iter()
            .filter(|(_, to)| *to == expected)
            .filter_map(|(from, _)| self.format_writer(from))
            .collect();
        match writers.as_slice() {
            [writer] => Ok(*writer),
            [] => {
                let diagnostic = Diagnostic::error(
                    codes::E043,
                    format!("no conversion from an f-string to {shown}"),
                )
                .label(span, true, format!("expected `{shown}` here"))
                .help(format!(
                    "an f-string is an `@format_args(W)`; `{shown}` needs a `@conversion` from one"
                ));
                Err(self.emit_involving(diagnostic, &[expected]))
            }
            _ => {
                let names: Vec<String> = writers
                    .iter()
                    .map(|writer| {
                        let format = self.types.intern_format_args(*writer);
                        format!("`{}`", self.show_id(format))
                    })
                    .collect();
                let diagnostic = Diagnostic::error(
                    codes::E073,
                    format!("this f-string could convert to `{shown}` from more than one format"),
                )
                .label(span, true, "which formatter type is meant is not known")
                .note(format!("`{shown}` converts from {}", names.join(" and ")))
                .help("pick one with `as`");
                Err(self.emit_involving(diagnostic, &[expected]))
            }
        }
    }

    /// One hole of a format into `writer`: `value` borrowed (a `ref T` hole
    /// is formatted as `T`) with its `@format(ref T, ref W)`, else `E071`.
    pub(crate) fn hole(&mut self, value: HirExpr, writer: TypeId, span: Span) -> Option<HirHole> {
        if value.ty.is_error() {
            return None;
        }
        let target = value.ty.id;
        let hook = self.overloads("@format").into_iter().find(|hook| {
            hook.sig.params == [TypeRef::reference(target), TypeRef::reference(writer)]
        });
        let Some(hook) = hook else {
            let (shown, into) = (self.show_id(target), self.show_id(writer));
            let diagnostic = Diagnostic::error(
                codes::E071,
                format!("`{shown}` cannot be formatted into `{into}`"),
            )
            .label(span, true, format!("no `@format` for `{shown}`"));
            let diagnostic = if self.is_declared_type(target) {
                diagnostic.help(format!(
                    "add `@format: function (self, out: ref {into}) nothing` to `{shown}`"
                ))
            } else {
                diagnostic.help(format!(
                    "define one: `@format: function (value: ref {shown}, out: ref {into}) nothing`"
                ))
            };
            self.emit_requirement(diagnostic, Requirement::Format, &[target, writer]);
            return None;
        };
        self.use_hook(&hook.key, span);
        let value = if value.ty.is_ref {
            value
        } else {
            HirExpr {
                ty: TypeRef::reference(target),
                span: value.span,
                kind: HirExprKind::RefOf(Box::new(value)),
            }
        };
        Some(HirHole {
            value,
            format: hook.key,
        })
    }

    /// Whether `ty` is an object or enum declared in source, which can take
    /// a member `@format`.
    fn is_declared_type(&self, ty: TypeId) -> bool {
        let def = match self.types.kind(ty) {
            TypeKind::Object(def) => *def,
            TypeKind::Instance { base, .. } => *base,
            _ => return false,
        };
        !self.res.def(def).span.is_dummy()
    }

    /// A value in an `@format_args(writer)` slot that is not one already:
    /// a string literal is a format with no holes, and any other value the
    /// one-hole format `$"{value}"`.
    pub(crate) fn format_slot(&mut self, expr: HirExpr, writer: TypeId) -> HirExpr {
        let span = expr.span;
        if let (HirExprKind::Str(bytes), false) = (&expr.kind, expr.ty.is_ref) {
            return self.format_value(writer, vec![bytes.clone()], Vec::new(), span);
        }
        match self.hole(expr, writer, span) {
            Some(hole) => self.format_value(writer, vec![Vec::new(), Vec::new()], vec![hole], span),
            None => HirExpr::poison(span),
        }
    }

    /// `@format_args_holes(args)`, `@format_args_piece(args, index)` and
    /// `@format_args_hole(args, index, out)`: `args` any format (or a `ref`
    /// to one), `out` a `ref W` for its own `W`.
    pub(crate) fn check_format_consumer(
        &mut self,
        fcx: &mut FnCx,
        intrinsic: Intrinsic,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        let (want, outs) = match intrinsic {
            Intrinsic::FormatArgsHoles => (1, vec![Builtin::BitNative.value()]),
            Intrinsic::FormatArgsPiece => (2, vec![Builtin::ByteBuffer.value()]),
            _ => (3, Vec::new()),
        };
        let arity = self.intrinsic_arity(name, want, args.len(), span);
        self.callee(fcx, args, arity)?;
        let format = self.check_expr(fcx, &args[0], None);
        if format.ty.is_error() {
            // The format reported it.
            return Err(self.callee_failed(fcx, &args[1..], self.diags.already_reported()));
        }
        let format = Self::auto_deref(format);
        let Some(writer) = self.format_writer(format.ty.id) else {
            let found = self.show(format.ty);
            let diagnostic = Diagnostic::error(
                codes::E010,
                format!("mismatched types: expected an `@format_args(W)`, found `{found}`"),
            )
            .label(args[0].span(), true, "not a format");
            let error = self.emit_requirement(
                diagnostic,
                Requirement::Mismatch {
                    expected_ref: false,
                },
                &[format.ty.id],
            );
            return Err(self.callee_failed(fcx, &args[1..], error));
        };
        let mut checked = vec![format];
        if want >= 2 {
            let index = self.intrinsic_arg(
                fcx,
                &args[1],
                Slot::Builtin(Builtin::BitNative),
                false,
                span,
            );
            let Some(index) = index else {
                // `intrinsic_arg` reported it.
                return Err(self.callee_failed(fcx, &args[2..], self.diags.already_reported()));
            };
            checked.push(index);
        }
        if want == 3 {
            let out = self.check_expr(fcx, &args[2], Some(writer));
            if out.ty.is_error() {
                // The writer reported it.
                return Err(self.diags.already_reported());
            }
            if !(out.ty.is_ref && out.ty.id == writer) {
                let (into, found) = (self.show_id(writer), self.show(out.ty));
                let diagnostic = Diagnostic::error(
                    codes::E010,
                    format!("mismatched types: expected `ref {into}`, found `{found}`"),
                )
                .label(
                    args[2].span(),
                    true,
                    format!("the format writes into `{into}`"),
                );
                return Err(self.emit_requirement(
                    diagnostic,
                    Requirement::Mismatch { expected_ref: true },
                    &[out.ty.id, writer],
                ));
            }
            checked.push(out);
        }
        Ok(Called::Call {
            callee: HirCallee::Intrinsic(intrinsic),
            args: checked,
            outs,
        })
    }

    /// `E072` when a by-value `ty` is a format: `what` names where it would
    /// be ("a field"). Through the requirement path, so a generic instance
    /// that puts a format there fails as `E042`.
    pub(crate) fn format_place(
        &mut self,
        ty: TypeRef,
        span: Span,
        what: &str,
    ) -> Result<(), ErrorGuaranteed> {
        if ty.is_ref || self.format_writer(ty.id).is_none() {
            return Ok(());
        }
        let shown = self.show_id(ty.id);
        let diagnostic = Diagnostic::error(codes::E072, format!("a format cannot be {what}"))
            .label(span, true, format!("`{shown}` here"))
            .note(LIVES_IN_LOCALS)
            .help("to keep the text, format it into a value that owns it, such as a `String`");
        Err(self.emit_requirement(diagnostic, Requirement::FormatPlace, &[ty.id]))
    }

    /// `E072` for each field or enum payload of `layout` that is a format;
    /// `spans[i]` is where slot `i` is declared.
    pub(crate) fn no_format_slots(
        &mut self,
        layout: &Layout,
        spans: &[Span],
    ) -> Result<(), ErrorGuaranteed> {
        let what = match layout {
            Layout::Object { .. } => "a field",
            Layout::Enum { .. } => "an enum payload",
        };
        let mut result = Ok(());
        for (index, (_, ty)) in layout.slots().into_iter().enumerate() {
            let span = spans.get(index).copied().unwrap_or_else(Span::dummy);
            if let Err(error) = self.format_place(ty, span, what) {
                result = Err(error);
            }
        }
        result
    }

    /// `E072` for a format stored where it may outlive its holes: through
    /// a `ref` (`store_through_ref`), or, when one of its holes is a
    /// temporary, into a local that outlives the statement.
    pub(crate) fn format_store(
        &mut self,
        value: &HirExpr,
        store_through_ref: bool,
        span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        if value.ty.is_error() || value.ty.is_ref || self.format_writer(value.ty.id).is_none() {
            return Ok(());
        }
        if store_through_ref {
            let diagnostic =
                Diagnostic::error(codes::E072, "a format cannot be stored through a `ref`")
                    .label(span, true, "the target may outlive this call")
                    .note(LIVES_IN_LOCALS);
            return Err(self.emit_involving(diagnostic, &[value.ty.id]));
        }
        if has_temporary_hole(value) {
            let diagnostic = Diagnostic::error(
                codes::E072,
                "this format borrows a temporary that ends with the statement",
            )
            .label(span, true, "the local outlives the statement")
            .help("declare a new local for it: `name: Format = $\"...\"`");
            return Err(self.emit_involving(diagnostic, &[value.ty.id]));
        }
        Ok(())
    }

    /// The initializer of a local, when it is a format: each hole that
    /// borrows a temporary is evaluated, in order, into a hidden local
    /// declared before it, so the temporary lives as long as the local
    /// (and drops with the block, after it). Holes of a nested format are
    /// extended the same way, first. A temporary hole in an arm of a value
    /// `if` or `match` is `E072`.
    pub(crate) fn extend_holes(
        &mut self,
        fcx: &mut FnCx,
        value: &mut HirExpr,
        out: &mut Vec<HirStmt>,
    ) {
        if let HirExprKind::Format(format) = &mut value.kind {
            for hole in &mut format.holes {
                let HirExprKind::RefOf(inner) = &mut hole.value.kind else {
                    continue;
                };
                if is_lasting(inner) {
                    continue;
                }
                self.extend_holes(fcx, inner, out);
                let local: LocalId = fcx.hidden(inner.ty, inner.span);
                let span = inner.span;
                let ty = inner.ty;
                let taken = std::mem::replace(
                    &mut **inner,
                    HirExpr {
                        ty,
                        span,
                        kind: HirExprKind::Local(local),
                    },
                );
                out.push(HirStmt::Let {
                    local,
                    value: taken,
                });
            }
        } else if matches!(
            value.kind,
            HirExprKind::If { .. } | HirExprKind::Match { .. }
        ) && self.format_writer(value.ty.id).is_some()
            && has_temporary_hole(value)
        {
            let diagnostic = Diagnostic::error(
                codes::E072,
                "a format in an `if` or `match` arm cannot borrow a temporary",
            )
            .label(value.span, true, "a hole here ends with the statement")
            .help("declare a local for the value the hole formats, before this one");
            self.emit_involving(diagnostic, &[value.ty.id]);
        }
    }
}

/// An f-string's pieces (one more than its holes: adjacent text joins, and
/// a missing piece is empty) and its hole expressions with their spans.
fn split_parts(parts: &[FStrPart]) -> (Vec<Vec<u8>>, Vec<(&Expr, Span)>) {
    let mut pieces = vec![Vec::new()];
    let mut holes = Vec::new();
    for part in parts {
        match part {
            FStrPart::Lit { bytes, .. } => {
                if let Some(last) = pieces.last_mut() {
                    last.extend_from_slice(bytes);
                }
            }
            FStrPart::Hole { expr, span } => {
                holes.push((expr, *span));
                pieces.push(Vec::new());
            }
        }
    }
    (pieces, holes)
}

/// Whether a place outlives the statement that names it: a local, a
/// global, a place behind a `ref`, or a field of one of those.
fn is_lasting(expr: &HirExpr) -> bool {
    match &expr.kind {
        HirExprKind::Local(_) | HirExprKind::Global(_) | HirExprKind::Deref(_) => true,
        HirExprKind::Field { base, .. } => is_lasting(base),
        _ => false,
    }
}

/// Whether a format value (or any arm of a value `if`/`match` of one)
/// has a hole that borrows a temporary.
fn has_temporary_hole(value: &HirExpr) -> bool {
    match &value.kind {
        HirExprKind::Format(format) => format.holes.iter().any(|hole| match &hole.value.kind {
            HirExprKind::RefOf(inner) => !is_lasting(inner),
            _ => false,
        }),
        HirExprKind::If { then, els, .. } => has_temporary_hole(then) || has_temporary_hole(els),
        HirExprKind::Match { arms, els, .. } => {
            arms.iter().any(|arm| has_temporary_hole(&arm.value)) || has_temporary_hole(els)
        }
        _ => false,
    }
}
