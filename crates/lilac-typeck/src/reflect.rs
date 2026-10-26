//! The reflection tests: `@has_field`, `@has_method`, `@has_function`,
//! `@has_constant`, `@is_copyable` and `@is_same`. Each answers by the
//! lookup a use would take and folds to a `@bit8` literal per instance
//! (like `@needs_drop`); a template whose answer depends on its arguments
//! holds `HirExprKind::Unanswered`. A false test is silent: a missing
//! member, one of another type, a private one or a generic one is `0x0`,
//! never an error.
//!
//! Where a member test holds (`docs/reflection-plan.md`, "Where a test
//! holds") and is not known true, typeck **assumes** the member: a use
//! of it checks against a stand-in of the stated type
//! (`HirExprKind::Assumed`, `HirCallee::Assumed`), so every arm is
//! checked in every instance. A condition's [`Facts`] say what it proves
//! where it is true and where it is false; `if`, `and` and `or` push them
//! on [`FnCx::assumed`] over the code they guard. Lowering compiles only
//! the arm a known condition picks (`Lower::outcome`), so it never meets
//! a stand-in.

use lilac_ast::{BinOp, Expr, TypeShape};
use lilac_error::{codes, Diagnostic, ErrorGuaranteed};
use lilac_hir::intrinsics::Intrinsic;
use lilac_hir::{HirCallee, HirExpr, HirExprKind};
use lilac_resolve::{DefId, DefKind};
use lilac_span::Span;
use lilac_ty::{Builtin, TypeId, TypeKind, TypeRef};

use crate::body::FnCx;
use crate::calls::Called;
use crate::expr::Receiver;
use crate::Checker;

/// A member a test assumes where it holds and is not known true.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Assumption {
    /// The tested type, through a reference.
    pub owner: TypeId,
    /// The member's name.
    pub name: String,
    /// The test: `@has_field`, `@has_method`, `@has_function` or
    /// `@has_constant`.
    pub test: Intrinsic,
    /// The stated type (a function type for the function tests).
    pub ty: TypeRef,
}

/// What a condition proves: the assumptions that hold where it is true
/// and where it is false.
#[derive(Clone, Debug, Default)]
pub(crate) struct Facts {
    pub when_true: Vec<Assumption>,
    pub when_false: Vec<Assumption>,
}

impl Facts {
    /// What `not` of the condition proves.
    fn flipped(self) -> Self {
        Self {
            when_true: self.when_false,
            when_false: self.when_true,
        }
    }

    /// What `a and b` proves: true, both sides held; false, only what
    /// either side's falsity proves alike.
    fn both(self, right: Self) -> Self {
        Self {
            when_true: [self.when_true, right.when_true].concat(),
            when_false: common(self.when_false, &right.when_false),
        }
    }

    /// What `a or b` proves, the reverse of [`Facts::both`].
    fn either(self, right: Self) -> Self {
        self.flipped().both(right.flipped()).flipped()
    }
}

/// The assumptions in both lists.
fn common(left: Vec<Assumption>, right: &[Assumption]) -> Vec<Assumption> {
    left.into_iter().filter(|a| right.contains(a)).collect()
}

/// Whether `expr` is a member test itself (not one inside it).
pub(crate) fn is_member_test(expr: &Expr) -> bool {
    let Expr::Call { callee, .. } = expr else {
        return false;
    };
    let Expr::Ident(name, _) = &**callee else {
        return false;
    };
    matches!(
        Intrinsic::lookup(name),
        Some(
            Intrinsic::HasField
                | Intrinsic::HasMethod
                | Intrinsic::HasFunction
                | Intrinsic::HasConstant
        )
    )
}

/// The stand-in field `base.name` for `assumed`: a place of the stated
/// type. `base` is the object, through any reference.
pub(crate) fn stand_in_field(assumed: Assumption, base: HirExpr, span: Span) -> HirExpr {
    HirExpr {
        ty: assumed.ty,
        span,
        kind: HirExprKind::Assumed {
            base: Some(Box::new(base)),
            name: assumed.name,
        },
    }
}

impl FnCx {
    /// Assumes `facts` over the code checked next; returns the mark to
    /// truncate [`FnCx::assumed`] back to after it.
    pub(crate) fn assume(&mut self, facts: &[Assumption]) -> usize {
        let mark = self.assumed.len();
        self.assumed.extend_from_slice(facts);
        mark
    }

    /// The `@has_field` assumed of `name` on the object `base` (through
    /// any reference) is, if any.
    pub(crate) fn assumed_field(&self, base: &HirExpr, name: &str) -> Option<Assumption> {
        self.assumption(base.ty.id, name, Intrinsic::HasField)
    }

    /// `Type.NAME` where a `@has_constant` is assumed: the stand-in
    /// constant, a value of the stated type.
    pub(crate) fn assumed_constant(
        &self,
        receiver: Receiver,
        name: &str,
        span: Span,
    ) -> Option<HirExpr> {
        let Receiver::Type(ty) = receiver else {
            return None;
        };
        let assumed = self
            .assumption(ty.id, name, Intrinsic::HasConstant)
            .filter(|_| !ty.is_ref)?;
        Some(HirExpr {
            ty: assumed.ty,
            span,
            kind: HirExprKind::Assumed {
                base: None,
                name: assumed.name,
            },
        })
    }

    /// The innermost assumption of `name` on `owner` by `test`.
    pub(crate) fn assumption(
        &self,
        owner: TypeId,
        name: &str,
        test: Intrinsic,
    ) -> Option<Assumption> {
        self.assumed
            .iter()
            .rev()
            .find(|a| a.owner == owner && a.name == name && a.test == test)
            .cloned()
    }
}

impl Checker<'_> {
    /// Checks a condition and what it proves: `not`, `and` and `or` pass
    /// on their operands' [`Facts`], a member test its assumption, and
    /// anything else proves nothing.
    pub(crate) fn check_condition(
        &mut self,
        fcx: &mut FnCx,
        cond: &Expr,
        what: &str,
    ) -> (HirExpr, Facts) {
        match cond {
            Expr::Not { value, span } => {
                let (inner, facts) = self.check_condition(fcx, value, "not");
                let hir = if inner.ty.is_error() {
                    HirExpr::poison(*span)
                } else {
                    HirExpr {
                        ty: Builtin::Bit8.value(),
                        span: *span,
                        kind: HirExprKind::Not(Box::new(inner)),
                    }
                };
                (hir, facts.flipped())
            }
            Expr::BinOp {
                lhs,
                op: op @ (BinOp::And | BinOp::Or),
                rhs,
                span,
            } => self.check_logic(fcx, lhs, *op == BinOp::And, rhs, *span),
            _ if is_member_test(cond) => {
                fcx.tested = None;
                let hir = self.check_cond(fcx, cond, what);
                let facts = Facts {
                    when_true: fcx.tested.take().into_iter().collect(),
                    when_false: Vec::new(),
                };
                (hir, facts)
            }
            _ => (self.check_cond(fcx, cond, what), Facts::default()),
        }
    }

    /// `lhs and rhs` (`and`) or `lhs or rhs`: `rhs` runs only when `lhs`
    /// does not decide, so it is checked under what that proves.
    pub(crate) fn check_logic(
        &mut self,
        fcx: &mut FnCx,
        lhs: &Expr,
        and: bool,
        rhs: &Expr,
        span: Span,
    ) -> (HirExpr, Facts) {
        let what = if and { "and" } else { "or" };
        let (left, left_facts) = self.check_condition(fcx, lhs, what);
        let mark = fcx.assume(if and {
            &left_facts.when_true
        } else {
            &left_facts.when_false
        });
        let (right, right_facts) = self.check_condition(fcx, rhs, what);
        fcx.assumed.truncate(mark);
        let facts = if and {
            left_facts.both(right_facts)
        } else {
            left_facts.either(right_facts)
        };
        let hir = HirExpr {
            ty: Builtin::Bit8.value(),
            span,
            kind: HirExprKind::Logic {
                and,
                lhs: Box::new(left),
                rhs: Box::new(right),
            },
        };
        (hir, facts)
    }

    /// A call of an assumed member. A stand-in method or static function
    /// checks its arguments (after the `receiver`, passed as the stated
    /// type takes it) and outs against the stated type, and one that can
    /// fail must be handled where it is called, as a named call must; a
    /// stand-in field is called through the function reference it holds.
    pub(crate) fn assumed_call(
        &mut self,
        fcx: &mut FnCx,
        assumed: Assumption,
        receiver: Option<HirExpr>,
        generic_args: usize,
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        if generic_args > 0 {
            // No test states a generic member.
            let error = self.generic_arity(span, &assumed.name, 0, generic_args);
            return Err(self.callee_failed(fcx, args, error));
        }
        if assumed.test == Intrinsic::HasField {
            let Some(receiver) = receiver else {
                lilac_error::bug!(span, "a field call with no receiver");
            };
            let what = format!("field `{}`", assumed.name);
            let base = self.auto_deref(receiver);
            let at = base.span;
            let callee = stand_in_field(assumed, base, at);
            return self.value_call(fcx, callee, &what, args, span);
        }
        let TypeKind::Func {
            params,
            returns,
            failures,
            ..
        } = self.types.kind(assumed.ty.id).clone()
        else {
            lilac_error::bug!(span, "an assumed function of a non-function type");
        };
        let mut hir_args = Vec::new();
        if let Some(receiver) = receiver {
            // Only a method type that takes the owner first is assumed.
            let receiver = match params.first() {
                Some(first) if first.is_ref && !receiver.ty.is_ref => HirExpr {
                    ty: TypeRef::reference(receiver.ty.id),
                    span: receiver.span,
                    kind: HirExprKind::RefOf(Box::new(receiver)),
                },
                Some(first) if first.is_ref => receiver,
                _ => self.auto_deref(receiver),
            };
            hir_args.push(receiver);
        }
        let skip = hir_args.len().min(params.len());
        hir_args.extend(self.check_args(fcx, &params[skip..], args, span)?);
        let mut outs = returns;
        if let Some(failures) = failures {
            if fcx.handled != Some(span) {
                return Err(self.diags.emit(
                    Diagnostic::error(
                        codes::E057,
                        format!(
                            "`{}` can fail, and the failure is not handled",
                            assumed.name
                        ),
                    )
                    .label(span, true, "this call can fail")
                    .help(
                        "handle it where it happens: `value: T = call else { return }`, \
                         `if call -> value: T { ... }`, or `loop if call -> value: T { ... }`",
                    ),
                ));
            }
            // The failure outs, then the flag (`HirFunc::failure`).
            outs.extend(failures);
            outs.push(Builtin::Bit8.value());
        }
        Ok(Called::Call {
            callee: HirCallee::Assumed {
                name: assumed.name,
                ty: assumed.ty.id,
            },
            args: hir_args,
            outs,
        })
    }

    /// One of the reflection tests (see the module docs).
    pub(crate) fn check_reflect(
        &mut self,
        fcx: &mut FnCx,
        intrinsic: Intrinsic,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<Called, ErrorGuaranteed> {
        let arity = match intrinsic {
            Intrinsic::IsCopyable => 1,
            Intrinsic::IsSame => 2,
            _ => 3,
        };
        self.intrinsic_arity(name, arity, args.len(), span)?;
        let answer = match intrinsic {
            Intrinsic::IsCopyable => {
                let ty = self.test_type(fcx, &args[0])?;
                if self.types.contains_param(ty.id) {
                    None
                } else {
                    Some(self.is_copyable(ty))
                }
            }
            Intrinsic::IsSame => {
                let first = self.test_type(fcx, &args[0]);
                let second = self.test_type(fcx, &args[1]);
                self.same_type(first?, second?)
            }
            _ => {
                // Every operand is checked, so each reports its own error.
                let owner = self.test_type(fcx, &args[0]);
                let member = self.member_name(&args[1]);
                let stated = self.test_type(fcx, &args[2]);
                let (owner, (member, member_span), stated) = (owner?, member?, stated?);
                self.stated_function(intrinsic, stated, args[2].span())?;
                let answer = self.has_member(intrinsic, owner, &member, member_span, stated)?;
                // Known true, a use finds the real member. A method type
                // that does not take the owner first is no method's, so
                // nothing is assumed of it.
                let assumable = answer != Some(true)
                    && (intrinsic != Intrinsic::HasMethod || self.takes_owner(owner, stated));
                fcx.tested = assumable.then(|| Assumption {
                    owner: owner.deref().id,
                    name: member,
                    test: intrinsic,
                    ty: stated,
                });
                answer
            }
        };
        let kind = answer.map_or(HirExprKind::Unanswered, |yes| {
            HirExprKind::Int(u64::from(yes))
        });
        Ok(Called::Value(HirExpr {
            ty: Builtin::Bit8.value(),
            span,
            kind,
        }))
    }

    /// A test's type operand: a type, a value standing for its type
    /// ([`Self::size_of_operand`]), or a function type, which may fail and
    /// needs no `ref` (it states a member's type, like an alias's target).
    fn test_type(&mut self, fcx: &mut FnCx, arg: &Expr) -> Result<TypeRef, ErrorGuaranteed> {
        let function = |expr: &Expr| match expr {
            Expr::TypeApp(ty) => match &ty.shape {
                TypeShape::Function(func) => Some((ty.clone(), func.clone())),
                _ => None,
            },
            _ => None,
        };
        let ty = if let Some((_, func)) = function(arg) {
            let template = TypeRef::value(self.resolve_function(&func));
            self.substitute_env(template)
        } else if let Some((syntax, func)) = match arg {
            Expr::RefOf { value, .. } => function(value),
            _ => None,
        } {
            let template = TypeRef::reference(self.resolve_function(&func));
            let ty = self.function_value(&syntax, template);
            self.substitute_env(ty)
        } else {
            self.size_of_operand(fcx, arg)?
        };
        if ty.is_error() {
            // `resolve_function` or `function_value` reported it.
            return Err(self.diags.already_reported());
        }
        self.ensure_type(ty, arg.span())?;
        Ok(ty)
    }

    /// `ty` at the arguments of the body being checked.
    fn substitute_env(&mut self, ty: TypeRef) -> TypeRef {
        if self.env.args.is_empty() || ty.is_error() {
            return ty;
        }
        let args = self.env.args.clone();
        self.types.substitute(ty, &args)
    }

    /// A test's member name: a bare identifier (`E022` otherwise).
    fn member_name(&mut self, arg: &Expr) -> Result<(String, Span), ErrorGuaranteed> {
        if let Expr::Ident(name, span) = arg {
            if !name.starts_with('@') {
                return Ok((name.clone(), *span));
            }
        }
        Err(self.diags.emit(
            Diagnostic::error(codes::E022, "expected a member name")
                .label(arg.span(), true, "write the member's name here")
                .help("a member is named by its identifier: `@has_field(T, count, Int)`"),
        ))
    }

    /// `E010` unless the type a `@has_method` or `@has_function` states is
    /// a function type (written without `ref`).
    fn stated_function(
        &mut self,
        intrinsic: Intrinsic,
        stated: TypeRef,
        span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        if !matches!(intrinsic, Intrinsic::HasMethod | Intrinsic::HasFunction) {
            return Ok(());
        }
        if !stated.is_ref && matches!(self.types.kind(stated.id), TypeKind::Func { .. }) {
            return Ok(());
        }
        let found = self.show(stated);
        let mut diagnostic = Diagnostic::error(
            codes::E010,
            format!("expected a function type, found `{found}`"),
        )
        .label(span, true, "the member's function type goes here");
        diagnostic = if stated.is_ref {
            diagnostic.help("a test states the function's type itself: drop the `ref`")
        } else {
            diagnostic.help("state its type: `function (ref T) @bit8`")
        };
        Err(self.diags.emit(diagnostic))
    }

    /// Whether the stated function type takes `owner` first, as a method
    /// of it does (`ref T`, or `T` for `self: T`).
    fn takes_owner(&self, owner: TypeRef, stated: TypeRef) -> bool {
        match self.types.kind(stated.id) {
            TypeKind::Func { params, .. } => {
                params.first().is_some_and(|first| first.id == owner.id)
            }
            _ => false,
        }
    }

    /// Whether `a` and `b` are one type; `None` while a parameter in
    /// either could still make them so, or not.
    fn same_type(&self, a: TypeRef, b: TypeRef) -> Option<bool> {
        if a == b {
            return Some(true);
        }
        let open = self.types.contains_param(a.id) || self.types.contains_param(b.id);
        // A parameter may stand for a `ref`, so `ref`-ness decides nothing.
        let could = a.is_ref != b.is_ref || self.could_match(a.id, b.id);
        (!(open && could)).then_some(false)
    }

    /// `@has_field`, `@has_method`, `@has_function` or `@has_constant` on
    /// `owner` (looked through a reference): the member a use of `name`
    /// would find, if it is there, visible and of exactly the `stated`
    /// type. A found member's name is a use of it.
    fn has_member(
        &mut self,
        intrinsic: Intrinsic,
        owner: TypeRef,
        name: &str,
        name_span: Span,
        stated: TypeRef,
    ) -> Result<Option<bool>, ErrorGuaranteed> {
        let owner = owner.deref();
        let Some((def, type_args)) = self.type_parts(owner.id) else {
            // A parameter has its argument's members; any other type
            // (a builtin, an array, a function) has none.
            let param = matches!(self.types.kind(owner.id), TypeKind::Param { .. });
            return Ok((!param).then_some(false));
        };
        let Some(member) = self.res.member(def, name) else {
            return Ok(Some(false));
        };
        if self.private_hidden(member, def) {
            return Ok(Some(false));
        }
        let found = match (intrinsic, &self.res.def(member).kind) {
            (Intrinsic::HasField, DefKind::Field(_)) => {
                let field = self
                    .fields_of(owner.id)
                    .and_then(|fields| fields.into_iter().find(|(field, _)| *field == name));
                match field {
                    Some((_, ty)) => ty,
                    // A template's type may have no layout yet.
                    None if self.types.contains_param(owner.id) => return Ok(None),
                    None => return Ok(Some(false)),
                }
            }
            (Intrinsic::HasConstant, DefKind::Const { .. }) => self.const_type(member),
            (Intrinsic::HasMethod | Intrinsic::HasFunction, DefKind::Func(_)) => {
                let method = intrinsic == Intrinsic::HasMethod;
                match self.member_function_type(member, def, &type_args, method)? {
                    Some(ty) => ty,
                    None => return Ok(Some(false)),
                }
            }
            _ => return Ok(Some(false)),
        };
        if found.is_error() {
            // `const_type` or the member's signature reported it.
            return Err(self.diags.already_reported());
        }
        let answer = self.same_type(found, stated);
        if answer != Some(false) {
            self.record_use(name_span, member);
        }
        Ok(answer)
    }

    /// The function type of `member`, a function of `owner` at
    /// `type_args`: its parameters (a method's receiver first), success
    /// outs and failure outs. `None` when it is not what the test asks
    /// for: a method (`method`) or a static function, and never a generic
    /// one, whose type no test can spell.
    fn member_function_type(
        &mut self,
        member: DefId,
        owner: DefId,
        type_args: &[TypeRef],
        method: bool,
    ) -> Result<Option<TypeRef>, ErrorGuaranteed> {
        let def = self.res.def(member);
        let Some(decl) = def.func() else {
            return Ok(None);
        };
        let receiver = decl
            .params
            .first()
            .is_some_and(lilac_ast::Param::is_receiver);
        if !def.generic_params.is_empty() || receiver != method {
            return Ok(None);
        }
        let (successes, fails) = (decl.returns.len(), decl.failures.is_some());
        let c_abi = decl.is_export || decl.is_extern;
        let sig = self.sig_of(member)?;
        let at = |c: &mut Self, slots: &[TypeRef]| -> Vec<TypeRef> {
            slots
                .iter()
                .map(|ty| c.types.substitute(*ty, type_args))
                .collect()
        };
        let params = at(self, &sig.params);
        // A method's receiver is the owner itself (`ref T`, or `T` for
        // `self: T`), as `method_call` requires.
        let of_owner = |first: &TypeRef| self.type_parts(first.id).is_some_and(|(d, _)| d == owner);
        if method && !params.first().is_some_and(of_owner) {
            return Ok(None);
        }
        let returns = at(self, &sig.returns[..successes]);
        // The failure outs, then the hidden flag (`HirFunc::failure`).
        let failures = fails.then(|| at(self, &sig.returns[successes..sig.returns.len() - 1]));
        let id = self.types.intern_function(params, returns, failures, c_abi);
        Ok(Some(TypeRef::value(id)))
    }
}
