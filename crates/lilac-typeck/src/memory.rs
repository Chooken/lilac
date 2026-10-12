//! The memory builtins: `@move`, `@write`, `@read`, `@drop`, `@needs_drop`
//! and `@discard`. Typeck checks their operands; the ownership pass
//! (`places.rs`) and lowering give them their meaning.

use lilac_ast::Expr;
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::intrinsics::Intrinsic;
use lilac_hir::{HirCallee, HirExpr, HirExprKind, HirPath};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId};

use crate::body::FnCx;
use crate::calls::Called;
use crate::Checker;

impl Checker<'_> {
    /// One of the memory builtins (see the module docs).
    pub(crate) fn check_memory(
        &mut self,
        fcx: &mut FnCx,
        intrinsic: Intrinsic,
        name: &str,
        args: &[Expr],
        span: Span,
        expected: Option<TypeId>,
    ) -> Result<Called, ErrorGuaranteed> {
        let arity = if intrinsic == Intrinsic::Write { 2 } else { 1 };
        self.intrinsic_arity(name, arity, args.len(), span)?;
        match intrinsic {
            Intrinsic::Move => Ok(Called::Value(
                self.check_move(fcx, &args[0], span, expected),
            )),
            Intrinsic::NeedsDrop => {
                let ty = self.size_of_operand(fcx, &args[0])?;
                self.ensure_type(ty, args[0].span())?;
                let needs = self.needs_drop(ty);
                Ok(Called::Value(HirExpr {
                    ty: Builtin::Bit8.value(),
                    span,
                    kind: HirExprKind::Int(u64::from(needs)),
                }))
            }
            Intrinsic::Write => {
                let pointer = self.memory_ref(fcx, name, &args[0]);
                let target = pointer.as_ref().ok().map(|pointer| pointer.ty.deref());
                let value = self.check_expr(fcx, &args[1], target.map(|ty| ty.id));
                let pointer = pointer?;
                let value = self.coerce(value, pointer.ty.deref(), args[1].span());
                if value.ty.is_error() {
                    // The value or `coerce` reported it.
                    return Err(self.diags.already_reported());
                }
                self.format_store(&value, true, args[1].span())?;
                Ok(Called::Call {
                    callee: HirCallee::Intrinsic(intrinsic),
                    args: vec![pointer, value],
                    outs: Vec::new(),
                })
            }
            Intrinsic::Read | Intrinsic::Drop => {
                let pointer = self.memory_ref(fcx, name, &args[0])?;
                let outs = if intrinsic == Intrinsic::Read {
                    vec![pointer.ty.deref()]
                } else {
                    Vec::new()
                };
                Ok(Called::Call {
                    callee: HirCallee::Intrinsic(intrinsic),
                    args: vec![pointer],
                    outs,
                })
            }
            _ => self.check_discard(fcx, &args[0]),
        }
    }

    /// `@move(place)`: the ownership pass checks the place can be moved.
    fn check_move(
        &mut self,
        fcx: &mut FnCx,
        value: &Expr,
        span: Span,
        expected: Option<TypeId>,
    ) -> HirExpr {
        let place = self.check_expr(fcx, value, expected);
        if place.ty.is_error() {
            return place;
        }
        if !place.is_place() {
            self.diags.emit(
                Diagnostic::error(codes::E064, "only a variable or a field can be moved")
                    .label(value.span(), true, "not a place")
                    .help("a value that is not in a variable moves without `@move`"),
            );
            return HirExpr::poison(span);
        }
        HirExpr {
            ty: place.ty,
            span,
            kind: HirExprKind::Move(Box::new(place)),
        }
    }

    /// The address operand of `@write`, `@read` or `@drop`: a `ref` to a
    /// known type, so there is a type to move or drop.
    fn memory_ref(
        &mut self,
        fcx: &mut FnCx,
        name: &str,
        arg: &Expr,
    ) -> Result<HirExpr, ErrorGuaranteed> {
        let pointer = self.check_expr(fcx, arg, None);
        if pointer.ty.is_error() {
            // The operand reported it.
            return Err(self.diags.already_reported());
        }
        let unknown = pointer.ty.id == Builtin::Unknown.id();
        if pointer.ty.is_ref && !unknown {
            return Ok(pointer);
        }
        let found = self.show(pointer.ty);
        let (label, help) = if pointer.ty.is_ref {
            (
                "`ref unknown` has no type to move or drop",
                "cast it to the type the memory holds: `address as ref T`",
            )
        } else {
            (
                "not a `ref`",
                "pass the slot's address: `ref place`, or a `ref T` from an allocation",
            )
        };
        Err(self.emit_involving(
            Diagnostic::error(
                codes::E010,
                format!("`{name}` needs a `ref` to a known type, found `{found}`"),
            )
            .label(arg.span(), true, label)
            .help(help),
            &[pointer.ty.id],
        ))
    }

    /// `@discard(place)`: `place` is owned by this function (a local, a
    /// by-value parameter, or a field of one) and is of the type whose
    /// function this is (`E066`).
    fn check_discard(&mut self, fcx: &mut FnCx, arg: &Expr) -> Result<Called, ErrorGuaranteed> {
        let place = self.check_expr(fcx, arg, None);
        if place.ty.is_error() {
            // The operand reported it.
            return Err(self.diags.already_reported());
        }
        let shown = self.show(place.ty);
        let owned = !place.ty.is_ref && HirPath::of(&place).is_some();
        if !owned {
            let help = if place.ty.is_ref {
                "a `ref` does not own what it points at; take the value by value (`self: T`)"
            } else {
                "bind the value to a variable first"
            };
            return Err(self.emit_involving(
                Diagnostic::error(codes::E066, "`@discard` needs a value this function owns")
                    .label(arg.span(), true, format!("a `{shown}` it does not own"))
                    .help(help),
                &[place.ty.id],
            ));
        }
        let def = self.type_parts(place.ty.id).map(|(def, _)| def);
        let is_object = self.fields_of(place.ty.id).is_some();
        if def.is_none() || def != self.env.owner || !is_object {
            let diagnostic = if is_object {
                Diagnostic::error(
                    codes::E066,
                    format!("`@discard` of a `{shown}` outside `{shown}`"),
                )
                .label(arg.span(), true, format!("a `{shown}`"))
                .help(format!(
                    "only a function declared in `{shown}` may end one without its `@on_drop`"
                ))
            } else {
                Diagnostic::error(codes::E066, format!("`{shown}` cannot be discarded"))
                    .label(arg.span(), true, "not an object")
                    .help("`@discard` turns an object's fields into places of their own")
            };
            return Err(self.emit_involving(diagnostic, &[place.ty.id]));
        }
        Ok(Called::Call {
            callee: HirCallee::Intrinsic(Intrinsic::Discard),
            args: vec![place],
            outs: Vec::new(),
        })
    }
}
