//! The ownership pass: which owned places are full and which are empty at
//! each point of a checked body, and what follows from it.
//!
//! It runs on each instance body once the body checked without an error
//! ([`Checker::check_places`]), since whether a read moves depends on the
//! concrete types. Control flow is structured, so it is a tree walk:
//!
//! - The state maps each local to an [`Own`] tree: full, empty, maybe
//!   (full on some paths), or per field when fields moved out one by one.
//!   Branches merge; `return`, `fail`, `break`, `continue`, `@panic` and
//!   `@trap` end a path. A loop body is walked until the state at its head
//!   stops changing, then once more for real.
//! - Reading a place into a new owner copies it when its type copies
//!   ([`Checker::is_copyable`]) and moves it otherwise: the read is wrapped
//!   in [`HirExprKind::Move`] and the place becomes empty. Reading an empty
//!   place is `E063`; moving out through a `ref`, out of a global or a
//!   temporary, or out of a type with its own `@on_drop` is `E064`.
//! - Every block ends with a [`HirStmt::Drop`] of its full locals, and each
//!   early exit drops the scopes it leaves. A place that may be empty where
//!   it drops gets a flag ([`HirFunc::flags`]), which lowering keeps.
//! - An assignment says what happens to the old value ([`HirOld`]).
//! - Outs start empty. Success outs must be full at every `return` and at
//!   the end of the body, failure outs at every `fail` (`E065`); an out of
//!   the other side that is full there drops.

use std::collections::HashMap;

use lilac_error::{codes, Diagnostic};
use lilac_hir::{
    HirBlock, HirCallee, HirDrop, HirExpr, HirExprKind, HirFunc, HirLocal, HirOld, HirPath,
    HirStmt, Intrinsic, LocalId,
};
use lilac_span::{BytePos, Span};
use lilac_ty::{Builtin, TypeKind, TypeRef};

use crate::{Checker, Requirement};

/// What one place holds.
#[derive(Clone, Debug)]
enum Own {
    /// A live value.
    Full,
    /// Nothing: never set, or moved out (where).
    Empty(Option<Span>),
    /// Full on some paths and empty on others (where it moved, if it did).
    Maybe(Option<Span>),
    /// An object some of whose fields moved out: one entry per field.
    Parts(Vec<Own>),
    /// An out of a call that can fail, bound to a hidden local: full when
    /// the call's flag (a local) is `full_when`.
    Cond { flag: LocalId, full_when: bool },
}

impl Own {
    /// Whether `self` and `other` say the same, spans aside.
    fn same(&self, other: &Own) -> bool {
        match (self, other) {
            (Own::Full, Own::Full)
            | (Own::Empty(_), Own::Empty(_))
            | (Own::Maybe(_), Own::Maybe(_)) => true,
            (Own::Parts(a), Own::Parts(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.same(b))
            }
            (
                Own::Cond { flag, full_when },
                Own::Cond {
                    flag: other_flag,
                    full_when: other_when,
                },
            ) => flag == other_flag && full_when == other_when,
            _ => false,
        }
    }

    /// The span a move left, if any.
    fn moved_at(&self) -> Option<Span> {
        match self {
            Own::Empty(at) | Own::Maybe(at) => *at,
            Own::Parts(parts) => parts.iter().find_map(Own::moved_at),
            Own::Full | Own::Cond { .. } => None,
        }
    }

    /// How a whole read sees the place.
    fn summary(&self) -> Summary {
        match self {
            Own::Full => Summary::Full,
            Own::Empty(at) => Summary::Empty(*at),
            Own::Maybe(at) => Summary::Maybe(*at),
            Own::Cond { .. } => Summary::Maybe(None),
            Own::Parts(parts) => {
                let summaries: Vec<Summary> = parts.iter().map(Own::summary).collect();
                if summaries.iter().all(|s| matches!(s, Summary::Full)) {
                    Summary::Full
                } else if summaries.iter().all(|s| matches!(s, Summary::Empty(_))) {
                    Summary::Empty(self.moved_at())
                } else {
                    Summary::Partly(self.moved_at())
                }
            }
        }
    }

    /// One entry per field: the parts, or `n` copies of a leaf.
    fn expand(&self, n: usize) -> Vec<Own> {
        match self {
            Own::Parts(parts) => parts.clone(),
            leaf => vec![leaf.clone(); n],
        }
    }

    /// Parts that are all full are the whole full again. (Parts that are
    /// all empty stay parts: the whole's drop flag, if any, was not
    /// cleared by the field moves.)
    fn normalize(self) -> Own {
        match self {
            Own::Parts(parts) if parts.iter().all(|part| matches!(part, Own::Full)) => Own::Full,
            other => other,
        }
    }
}

/// [`Own::summary`].
enum Summary {
    Full,
    Empty(Option<Span>),
    Maybe(Option<Span>),
    Partly(Option<Span>),
}

/// The state at one point: each local's [`Own`], or `None` where no path
/// reaches.
type State = Option<HashMap<LocalId, Own>>;

/// One enclosing loop.
struct LoopFrame {
    /// How many scopes were open outside it.
    depth: usize,
    /// The states at its `break`s, merged.
    breaks: State,
    /// The states at its `continue`s, merged.
    continues: State,
}

/// Where a place that is not an owned path lives.
enum Origin {
    /// Behind a `ref`.
    Ref,
    /// A global.
    Global,
    /// A field of a temporary.
    Temporary,
}

impl Checker<'_> {
    /// Runs the ownership pass over `func`'s body (see the module docs).
    pub(crate) fn check_places(&mut self, func: &mut HirFunc) {
        let Some(mut body) = func.body.take() else {
            return;
        };
        let (successes, failures) = match func.failure {
            Some(failure) => {
                let outs = &func.returns[..func.returns.len() - 1];
                (
                    outs[..failure.success].to_vec(),
                    outs[failure.success..].to_vec(),
                )
            }
            None => (func.returns.clone(), Vec::new()),
        };
        let mut state = HashMap::new();
        for id in &func.params {
            state.insert(*id, Own::Full);
        }
        for id in &func.returns {
            state.insert(*id, Own::Empty(None));
        }
        let mut pass = Pass {
            c: self,
            locals: &mut func.locals,
            flags: &mut func.flags,
            successes,
            failures,
            state: Some(state),
            frames: vec![func.params.clone()],
            loops: Vec::new(),
            dry: false,
        };
        let mut stmts = Vec::new();
        for stmt in std::mem::take(&mut body.stmts) {
            pass.stmt(stmt, &mut stmts);
        }
        if pass.state.is_some() {
            let end = end_of(body.span);
            let mut drops = pass.frame_drops(0);
            pass.check_outs(false, end, &mut drops);
            push_drops(&mut stmts, drops, end);
        }
        body.stmts = stmts;
        func.body = Some(body);
    }

    /// Whether reading a place of type `ty` copies it (else it moves): its
    /// own `@on_copy` copies, its own `@on_drop` moves, and otherwise it
    /// copies when every field (or payload) does.
    pub(crate) fn is_copyable(&mut self, ty: TypeRef) -> bool {
        if ty.is_ref {
            return true;
        }
        if let Some(known) = self.copyable.get(&ty.id) {
            return *known;
        }
        let result = match self.types.kind(ty.id).clone() {
            TypeKind::Array { elem, .. } => self.is_copyable(TypeRef::value(elem)),
            TypeKind::Object(_) | TypeKind::Instance { .. } => {
                if self.raii_hook("@on_copy", ty.id).is_some() {
                    true
                } else if self.raii_hook("@on_drop", ty.id).is_some() {
                    false
                } else {
                    self.slot_types(ty)
                        .into_iter()
                        .all(|slot| self.is_copyable(slot))
                }
            }
            _ => return true,
        };
        if self.out.layouts.contains_key(&ty.id) {
            self.copyable.insert(ty.id, result);
        }
        result
    }

    /// Whether dropping a `ty` does anything: its own `@on_drop`, or a slot
    /// that needs a drop (lowering's rule).
    pub(crate) fn needs_drop(&mut self, ty: TypeRef) -> bool {
        if ty.is_ref {
            return false;
        }
        if let Some(known) = self.droppable.get(&ty.id) {
            return *known;
        }
        let result = match self.types.kind(ty.id) {
            TypeKind::Object(_) | TypeKind::Instance { .. } => {
                self.raii_hook("@on_drop", ty.id).is_some()
                    || self
                        .slot_types(ty)
                        .into_iter()
                        .any(|slot| self.needs_drop(slot))
            }
            _ => return false,
        };
        if self.out.layouts.contains_key(&ty.id) {
            self.droppable.insert(ty.id, result);
        }
        result
    }

    /// Whether `ty` has its own `@on_drop`.
    fn has_own_drop(&self, ty: TypeRef) -> bool {
        !ty.is_ref && self.raii_hook("@on_drop", ty.id).is_some()
    }

    /// The field types of an object, or the payload types of an enum.
    fn slot_types(&self, ty: TypeRef) -> Vec<TypeRef> {
        match self.out.layouts.get(&ty.id) {
            Some(lilac_hir::Layout::Object { fields }) => {
                fields.iter().map(|(_, ty)| *ty).collect()
            }
            Some(lilac_hir::Layout::Enum { variants, .. }) => {
                variants.iter().filter_map(|(_, ty)| *ty).collect()
            }
            None => Vec::new(),
        }
    }

    /// The field types of an object type (none for anything else).
    fn field_types(&self, ty: TypeRef) -> Vec<TypeRef> {
        self.fields_of(ty.id)
            .map(|fields| fields.into_iter().map(|(_, ty)| ty).collect())
            .unwrap_or_default()
    }
}

/// The ownership pass over one body.
struct Pass<'p, 'a> {
    c: &'p mut Checker<'a>,
    locals: &'p mut Vec<HirLocal>,
    flags: &'p mut Vec<(HirPath, LocalId)>,
    /// The outs set on success.
    successes: Vec<LocalId>,
    /// The outs set on failure.
    failures: Vec<LocalId>,
    state: State,
    /// The locals each open scope declared, outermost first (the
    /// function's scope holds the parameters).
    frames: Vec<Vec<LocalId>>,
    loops: Vec<LoopFrame>,
    /// A trial walk of a loop body's copy: no diagnostics and no flags.
    dry: bool,
}

impl Pass<'_, '_> {
    // ---- statements ----------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn stmt(&mut self, stmt: HirStmt, out: &mut Vec<HirStmt>) {
        if self.state.is_none() {
            // No path reaches it: lowered as it is, never run.
            out.push(stmt);
            return;
        }
        match stmt {
            HirStmt::Let { local, mut value } => {
                self.expr(&mut value, true);
                self.init(local, Own::Full);
                out.push(HirStmt::Let { local, value });
            }
            HirStmt::MultiLet { locals, mut call } => {
                self.expr(&mut call, true);
                let conds = self.failing_outs(&call, &locals);
                for (index, local) in locals.iter().enumerate() {
                    let own = match conds {
                        Some((flag, success)) if index + 1 < locals.len() => Own::Cond {
                            flag,
                            full_when: index < success,
                        },
                        _ => Own::Full,
                    };
                    self.init(*local, own);
                }
                out.push(HirStmt::MultiLet { locals, call });
            }
            HirStmt::Assign {
                mut place,
                mut value,
                ..
            } => {
                self.expr(&mut value, true);
                let old = self.assign(&mut place);
                out.push(HirStmt::Assign { place, value, old });
            }
            HirStmt::Expr(mut expr) => {
                self.expr(&mut expr, false);
                // `@panic` and `@trap` never return, and a `@compile_error`
                // that is reached fails the build.
                let ends = matches!(
                    expr.kind,
                    HirExprKind::Call {
                        callee: HirCallee::Intrinsic(Intrinsic::Panic | Intrinsic::Trap),
                        ..
                    } | HirExprKind::CompileError { .. }
                );
                out.push(HirStmt::Expr(expr));
                if ends {
                    self.state = None;
                }
            }
            HirStmt::Return(span) | HirStmt::Fail(span) => {
                let fails = matches!(stmt, HirStmt::Fail(_));
                let mut drops = Vec::new();
                for index in (0..self.frames.len()).rev() {
                    drops.extend(self.frame_drops(index));
                }
                self.check_outs(fails, span, &mut drops);
                push_drops(out, drops, span);
                out.push(stmt);
                self.state = None;
            }
            HirStmt::Break(span) | HirStmt::Continue(span) => {
                let depth = self
                    .loops
                    .last()
                    .map_or(self.frames.len(), |frame| frame.depth);
                let mut drops = Vec::new();
                for index in (depth..self.frames.len()).rev() {
                    drops.extend(self.frame_drops(index));
                }
                push_drops(out, drops, span);
                let mut state = self.state.take();
                if let Some(state) = &mut state {
                    for frame in &self.frames[depth..] {
                        for local in frame {
                            state.remove(local);
                        }
                    }
                }
                let breaks = matches!(stmt, HirStmt::Break(_));
                out.push(stmt);
                if let Some(frame) = self.loops.last_mut() {
                    let slot = if breaks {
                        &mut frame.breaks
                    } else {
                        &mut frame.continues
                    };
                    let merged = merge_states(self.c, self.locals, slot.take(), state);
                    *slot = merged;
                }
            }
            HirStmt::If {
                mut cond,
                mut then,
                mut els,
            } => {
                self.expr(&mut cond, false);
                let flag = match cond.kind {
                    HirExprKind::Local(id) => Some(id),
                    _ => None,
                };
                let start = self.state.clone();
                self.state = refine(start.clone(), flag, true);
                self.block(&mut then, Vec::new());
                let after_then = self.state.take();
                self.state = refine(start, flag, false);
                self.block(&mut els, Vec::new());
                self.state = merge_states(self.c, self.locals, after_then, self.state.take());
                out.push(HirStmt::If { cond, then, els });
            }
            HirStmt::Loop(mut body) => {
                self.lower_loop(&mut body);
                out.push(HirStmt::Loop(body));
            }
            HirStmt::Match {
                mut scrutinee,
                mut arms,
                mut els,
            } => {
                let path = self.scrutinee(&mut scrutinee);
                let start = self.state.clone();
                let mut end: State = None;
                for arm in &mut arms {
                    self.state = start.clone();
                    if let Some(capture) = arm.capture {
                        arm.moves = self.capture(&scrutinee, path.as_ref(), capture);
                    }
                    self.block(&mut arm.body, arm.capture.into_iter().collect());
                    end = merge_states(self.c, self.locals, end, self.state.take());
                }
                self.state = start;
                if els.stmts.is_empty() && self.names_every_variant(&scrutinee, &arms) {
                    // Every variant has an arm, so the `else` is never taken.
                    self.state = None;
                }
                self.block(&mut els, Vec::new());
                self.state = merge_states(self.c, self.locals, end, self.state.take());
                out.push(HirStmt::Match {
                    scrutinee,
                    arms,
                    els,
                });
            }
            HirStmt::Block(mut block) => {
                self.block(&mut block, Vec::new());
                out.push(HirStmt::Block(block));
            }
            HirStmt::Drop { .. } => out.push(stmt),
        }
    }

    /// Walks `block` as a scope that first declares `seed` (full), and
    /// drops what it still owns at its end.
    fn block(&mut self, block: &mut HirBlock, seed: Vec<LocalId>) {
        self.frames.push(Vec::new());
        for local in seed {
            self.init(local, Own::Full);
        }
        let mut stmts = Vec::new();
        for stmt in std::mem::take(&mut block.stmts) {
            self.stmt(stmt, &mut stmts);
        }
        if self.state.is_some() {
            let drops = self.frame_drops(self.frames.len() - 1);
            push_drops(&mut stmts, drops, end_of(block.span));
        }
        block.stmts = stmts;
        if let (Some(state), Some(frame)) = (&mut self.state, self.frames.pop()) {
            for local in frame {
                state.remove(&local);
            }
        }
    }

    /// A loop: its body is walked on copies until the state at its head
    /// (what enters, merged with what each `continue` and the body's end
    /// carry round) stops changing, then for real. After it, the state is
    /// the merge of its `break`s.
    fn lower_loop(&mut self, body: &mut HirBlock) {
        let entry = self.state.clone();
        let mut head = entry.clone();
        let dry = self.dry;
        self.dry = true;
        for _ in 0..16 {
            let mut copy = body.clone();
            self.state = head.clone();
            let (end, _, continues) = self.loop_body(&mut copy);
            let next = merge_states(self.c, self.locals, entry.clone(), end);
            let next = merge_states(self.c, self.locals, next, continues);
            if same_states(&next, &head) {
                break;
            }
            head = next;
        }
        self.dry = dry;
        self.state = head;
        let (_, breaks, _) = self.loop_body(body);
        self.state = breaks;
    }

    /// Walks a loop body once: the state at its end, its `break`s and its
    /// `continue`s.
    fn loop_body(&mut self, body: &mut HirBlock) -> (State, State, State) {
        self.loops.push(LoopFrame {
            depth: self.frames.len(),
            breaks: None,
            continues: None,
        });
        self.block(body, Vec::new());
        let frame = self.loops.pop().expect("pushed above");
        (self.state.take(), frame.breaks, frame.continues)
    }

    /// Whether `arms` name every variant of the enum `scrutinee` holds.
    fn names_every_variant(&self, scrutinee: &HirExpr, arms: &[lilac_hir::HirArm]) -> bool {
        let Some(variants) = self.c.variants_of(scrutinee.ty.id) else {
            return false;
        };
        (0..variants.len()).all(|index| {
            arms.iter().any(|arm| {
                matches!(arm.pattern, lilac_hir::HirPattern::Variant(named) if named as usize == index)
            })
        })
    }

    /// The flag local and success count when `call` is a call that can
    /// fail binding every out (its flag last) to `locals`.
    fn failing_outs(&self, call: &HirExpr, locals: &[LocalId]) -> Option<(LocalId, usize)> {
        let HirExprKind::Call { callee, rest, .. } = &call.kind else {
            return None;
        };
        let success = self.c.success_outs(callee)?;
        let flag = *locals.last()?;
        (locals.len() == rest.len() + 1).then_some((flag, success))
    }

    /// Declares `local` in the innermost scope, holding `own`.
    fn init(&mut self, local: LocalId, own: Own) {
        if let Some(state) = &mut self.state {
            state.insert(local, own);
        }
        if let Some(frame) = self.frames.last_mut() {
            frame.push(local);
        }
    }

    /// `place = …`, after the value: what happens to the old value, and the
    /// place is full after.
    fn assign(&mut self, place: &mut HirExpr) -> HirOld {
        let Some(path) = HirPath::of(place) else {
            // Behind a `ref`, or a global: that memory is full.
            self.borrowed_place(place);
            return HirOld::Full;
        };
        if !self.parent_present(&path, place.span) {
            return HirOld::Empty(Vec::new());
        }
        let ty = place.ty;
        let own = self.own_at(&path).unwrap_or(Own::Empty(None));
        let old = match &own {
            Own::Full => HirOld::Full,
            Own::Empty(_) => HirOld::Empty(Vec::new()),
            Own::Maybe(_) | Own::Cond { .. } => {
                if self.c.needs_drop(ty) || self.c.raii_hook("@on_override", ty.id).is_some() {
                    HirOld::Maybe(self.flag(&path))
                } else {
                    HirOld::Empty(Vec::new())
                }
            }
            Own::Parts(_) => {
                let mut drops = Vec::new();
                self.drops_of(path.clone(), ty, &own, &mut drops);
                HirOld::Empty(drops)
            }
        };
        self.set(&path, Own::Full);
        old
    }

    /// Whether every place holding `path` (its local and the fields on the
    /// way) is there to write a field into; `E063` when one is empty.
    fn parent_present(&mut self, path: &HirPath, span: Span) -> bool {
        let Some(mut own) = self
            .state
            .as_ref()
            .and_then(|s| s.get(&path.local))
            .cloned()
        else {
            return true;
        };
        let mut ty = self.local_ty(path.local);
        for (depth, field) in path.fields.iter().enumerate() {
            match own {
                Own::Parts(parts) => {
                    own = parts[*field as usize].clone();
                    ty = self.c.field_types(ty)[*field as usize];
                }
                Own::Full => return true,
                empty => {
                    let parent = HirPath {
                        local: path.local,
                        fields: path.fields[..depth].to_vec(),
                    };
                    let name = self.path_name(&parent);
                    let mut diagnostic = Diagnostic::error(
                        codes::E063,
                        format!("`{name}` is empty, so its fields cannot be set"),
                    )
                    .label(span, true, format!("sets a field of `{name}`"));
                    if let Some(at) = empty.moved_at() {
                        diagnostic = diagnostic.label(at, false, "moved here");
                    }
                    let shown = self.c.types.display(ty.id);
                    self.report(diagnostic.help(format!(
                        "set the whole value first: `{name} = {shown} {{ … }}`"
                    )));
                    self.set(&parent, Own::Full);
                    return false;
                }
            }
        }
        true
    }

    /// Checks the outs where the function leaves (`fails`: through `fail`)
    /// at `span`: the side it leaves by must be full (`E065`), and what the
    /// other side holds drops.
    fn check_outs(&mut self, fails: bool, span: Span, drops: &mut Vec<HirDrop>) {
        let (set, other) = if fails {
            (self.failures.clone(), self.successes.clone())
        } else {
            (self.successes.clone(), self.failures.clone())
        };
        for out in set {
            let Some(own) = self.own_at(&HirPath::local(out)) else {
                continue;
            };
            let (what, label) = match own.summary() {
                Summary::Full => continue,
                Summary::Empty(_) => ("is not set", "not set"),
                Summary::Maybe(_) | Summary::Partly(_) => {
                    ("may not be set", "not set on every path")
                }
            };
            let name = self.out_name(out);
            let how = if fails { "fails" } else { "returns" };
            let mut diagnostic = Diagnostic::error(
                codes::E065,
                format!("{name} {what} when the function {how}"),
            )
            .label(span, true, format!("{how} here with {name} {label}"));
            if let Some(at) = own.moved_at() {
                diagnostic = diagnostic.label(at, false, "moved out here");
            }
            let side = if fails { "failure" } else { "success" };
            if !self.dry {
                self.c.diags.emit(diagnostic.help(format!(
                    "set every {side} out before the function {how}: an out starts empty"
                )));
            }
            // Reported once: treat it as set from here.
            self.set(&HirPath::local(out), Own::Full);
        }
        for out in other {
            let path = HirPath::local(out);
            if let Some(own) = self.own_at(&path) {
                let ty = self.local_ty(out);
                self.drops_of(path, ty, &own, drops);
            }
        }
    }

    /// How a diagnostic names an out.
    fn out_name(&self, out: LocalId) -> String {
        let local = &self.locals[out.0 as usize];
        if local.name.is_empty() {
            format!("the `{}` out", self.c.types.display_ref(local.ty))
        } else {
            format!("`{}`", local.name)
        }
    }

    // ---- expressions ---------------------------------------------------

    /// Walks `expr`; `consume` when its value moves into a new owner (else
    /// it is only read).
    #[allow(clippy::too_many_lines)]
    fn expr(&mut self, expr: &mut HirExpr, consume: bool) {
        if let Some(path) = HirPath::of(expr) {
            if consume && !self.c.is_copyable(expr.ty) {
                self.move_path(&path, expr.span);
                let inner = std::mem::replace(expr, HirExpr::poison(expr.span));
                *expr = HirExpr {
                    ty: inner.ty,
                    span: inner.span,
                    kind: HirExprKind::Move(Box::new(inner)),
                };
            } else {
                self.require_full(&path, expr.span);
            }
            return;
        }
        let span = expr.span;
        let ty = expr.ty;
        if expr.is_place() {
            // Behind a `ref`, a global, or a field of a temporary.
            self.borrowed_place(expr);
            if consume && !self.c.is_copyable(ty) {
                self.cannot_move(expr, span);
            }
            return;
        }
        match &mut expr.kind {
            HirExprKind::Move(inner) => {
                if let Some(path) = HirPath::of(inner) {
                    self.move_path(&path, span);
                } else {
                    self.borrowed_place(inner);
                    self.cannot_move(inner, span);
                }
            }
            HirExprKind::Struct(values) | HirExprKind::FormatArgs(values) => {
                for value in values {
                    self.expr(value, true);
                }
            }
            HirExprKind::Variant { payload, .. } => {
                if let Some(payload) = payload {
                    self.expr(payload, true);
                }
            }
            HirExprKind::Call { callee, args, .. } => {
                if let HirCallee::Local(id) = callee {
                    let id = *id;
                    self.require_full(&HirPath::local(id), span);
                }
                for arg in args {
                    self.expr(arg, true);
                }
            }
            HirExprKind::RefOf(inner) | HirExprKind::Retype(inner) => self.expr(inner, false),
            HirExprKind::Eq(lhs, rhs) => {
                self.expr(lhs, false);
                self.expr(rhs, false);
            }
            HirExprKind::If { cond, then, els } => {
                self.expr(cond, false);
                let start = self.state.clone();
                self.expr(then, true);
                let after_then = self.state.take();
                self.state = start;
                self.expr(els, true);
                self.state = merge_states(self.c, self.locals, after_then, self.state.take());
            }
            HirExprKind::Logic { lhs, rhs, .. } => {
                self.expr(lhs, false);
                let start = self.state.clone();
                self.expr(rhs, false);
                self.state = merge_states(self.c, self.locals, start, self.state.take());
            }
            HirExprKind::Match {
                scrutinee,
                arms,
                els,
            } => {
                let path = self.scrutinee(scrutinee);
                let start = self.state.clone();
                let mut end: State = None;
                for arm in arms {
                    self.state = start.clone();
                    if let Some(capture) = arm.capture {
                        arm.moves = self.capture(scrutinee, path.as_ref(), capture);
                        self.init_free(capture);
                    }
                    self.expr(&mut arm.value, true);
                    if let Some(capture) = arm.capture {
                        let own = self.own_at(&HirPath::local(capture));
                        let mut drops = Vec::new();
                        if let Some(own) = own {
                            let ty = self.local_ty(capture);
                            self.drops_of(HirPath::local(capture), ty, &own, &mut drops);
                        }
                        arm.drops = drops;
                        if let Some(state) = &mut self.state {
                            state.remove(&capture);
                        }
                    }
                    end = merge_states(self.c, self.locals, end, self.state.take());
                }
                self.state = start;
                self.expr(els, true);
                self.state = merge_states(self.c, self.locals, end, self.state.take());
            }
            HirExprKind::Local(_)
            | HirExprKind::Deref(_)
            | HirExprKind::Global(_)
            | HirExprKind::Field { .. }
            | HirExprKind::Number(_)
            | HirExprKind::Str(_)
            | HirExprKind::FuncRef(_)
            | HirExprKind::Const(_)
            | HirExprKind::SizeOf(_)
            | HirExprKind::AlignOf(_)
            | HirExprKind::CompileError { .. }
            | HirExprKind::Poison => {}
        }
    }

    /// A value-match capture: full in its arm, owned by no scope.
    fn init_free(&mut self, local: LocalId) {
        if let Some(state) = &mut self.state {
            state.insert(local, Own::Full);
        }
    }

    /// Walks what a place that is not an owned path reads on its way: the
    /// `ref` it goes through, or the temporary it is a field of.
    fn borrowed_place(&mut self, place: &mut HirExpr) {
        match &mut place.kind {
            HirExprKind::Deref(inner) => self.expr(inner, false),
            HirExprKind::Field { base, .. } => {
                if base.is_place() && HirPath::of(base).is_none() {
                    self.borrowed_place(base);
                } else {
                    self.expr(base, false);
                }
            }
            HirExprKind::Global(_) => {}
            _ => self.expr(place, false),
        }
    }

    /// A `match`'s scrutinee: an owned place is read in place (it must be
    /// full); anything else is walked. Returns the owned place, if it is one.
    fn scrutinee(&mut self, scrutinee: &mut HirExpr) -> Option<HirPath> {
        match HirPath::of(scrutinee) {
            Some(path) => {
                self.require_full(&path, scrutinee.span);
                Some(path)
            }
            None if scrutinee.is_place() => {
                self.borrowed_place(scrutinee);
                None
            }
            None => {
                self.expr(scrutinee, false);
                None
            }
        }
    }

    /// Binds `capture` from the payload of `scrutinee` (the owned place
    /// `path`, when it is one): whether it moves the payload out, which
    /// uses the matched value up. A payload that copies is copied.
    fn capture(&mut self, scrutinee: &HirExpr, path: Option<&HirPath>, capture: LocalId) -> bool {
        let ty = self.local_ty(capture);
        if self.c.is_copyable(ty) {
            return false;
        }
        let span = self.locals[capture.0 as usize].span;
        let owner = scrutinee.ty.deref();
        if self.c.has_own_drop(owner) {
            let shown = self.c.types.display(owner.id);
            self.report(
                Diagnostic::error(
                    codes::E064,
                    format!("cannot move a payload out of `{shown}`, which has its own `@on_drop`"),
                )
                .label(span, true, "this capture would move the payload out")
                .label(scrutinee.span, false, format!("a `{shown}`"))
                .help(format!(
                    "`{}` cannot be copied; match through a method of `{shown}` instead",
                    self.c.types.display_ref(ty)
                )),
            );
            return false;
        }
        match path {
            Some(path) => {
                self.set(path, Own::Empty(Some(span)));
                true
            }
            None if !scrutinee.is_place() => true,
            None => {
                let shown = self.c.types.display_ref(ty);
                let (message, label) = match origin(scrutinee) {
                    Origin::Ref => (
                        "cannot move a payload out of a reference",
                        "a `ref` does not own what it points at",
                    ),
                    Origin::Global => (
                        "cannot move a payload out of a global",
                        "a global is never moved out",
                    ),
                    Origin::Temporary => (
                        "cannot move a payload out of a field of a temporary",
                        "the rest of the temporary drops",
                    ),
                };
                self.report(
                    Diagnostic::error(codes::E064, message)
                        .label(
                            span,
                            true,
                            format!("`{shown}` cannot be copied, so this would move it"),
                        )
                        .label(scrutinee.span, false, label)
                        .help("match on a value this function owns"),
                );
                false
            }
        }
    }

    /// Moves the owned place `path` out at `span`.
    fn move_path(&mut self, path: &HirPath, span: Span) {
        self.require_full(path, span);
        let mut ty = self.local_ty(path.local);
        for (depth, field) in path.fields.iter().enumerate() {
            if self.c.has_own_drop(ty) {
                let shown = self.c.types.display(ty.id);
                let parent = HirPath {
                    local: path.local,
                    fields: path.fields[..depth].to_vec(),
                };
                let name = self.path_name(&parent);
                self.report(
                    Diagnostic::error(
                        codes::E064,
                        format!(
                            "cannot move a field out of `{shown}`, which has its own `@on_drop`"
                        ),
                    )
                    .label(span, true, format!("moves `{}` out", self.path_name(path)))
                    .help(format!(
                        "its `@on_drop` needs every field; move the whole `{name}` instead"
                    )),
                );
                return;
            }
            ty = self.c.field_types(ty)[*field as usize];
        }
        self.set(path, Own::Empty(Some(span)));
    }

    /// `E064` for moving out of `place`, which is not an owned path.
    fn cannot_move(&mut self, place: &HirExpr, span: Span) {
        let shown = self.c.types.display_ref(place.ty);
        let (message, label, help) = match origin(place) {
            Origin::Ref => (
                format!("cannot move `{shown}` out of a reference"),
                "a `ref` does not own what it points at",
                "take a `ref` to it instead, or move the value its owner holds",
            ),
            Origin::Global => (
                format!("cannot move `{shown}` out of a global"),
                "a global is never moved out",
                "take a `ref` to it instead",
            ),
            Origin::Temporary => (
                format!("cannot move `{shown}` out of a field of a temporary"),
                "the rest of the temporary drops at the end of the statement",
                "bind the value to a variable first, then move the field out of it",
            ),
        };
        let what = if self.c.is_copyable(place.ty) {
            "moved here".to_owned()
        } else {
            format!("`{shown}` cannot be copied, so this moves it")
        };
        self.report(
            Diagnostic::error(codes::E064, message)
                .label(span, true, what)
                .note(label)
                .help(help),
        );
    }

    /// Requires the owned place `path` to be full at a read (`E063`).
    fn require_full(&mut self, path: &HirPath, span: Span) {
        let Some(own) = self.own_at(path) else {
            return;
        };
        let name = self.path_name(path);
        let (message, used, moved) = match own.summary() {
            Summary::Full => return,
            Summary::Empty(None) => (
                format!("`{name}` is read before it is set"),
                "read here while empty",
                None,
            ),
            Summary::Empty(Some(at)) => (
                format!("`{name}` is used after it was moved"),
                "used here while empty",
                Some((at, "moved here")),
            ),
            Summary::Maybe(None) => (
                format!("`{name}` may not be set"),
                "read here, where it may be empty",
                None,
            ),
            Summary::Maybe(Some(at)) => (
                format!("`{name}` may have been moved"),
                "used here, where it may be empty",
                Some((at, "moved here, on some path or in an earlier iteration")),
            ),
            Summary::Partly(at) => (
                format!("part of `{name}` was moved"),
                "used here as a whole",
                at.map(|at| (at, "moved here")),
            ),
        };
        let mut diagnostic = Diagnostic::error(codes::E063, message).label(span, true, used);
        diagnostic = match moved {
            Some((at, label)) => diagnostic.label(at, false, label),
            None => diagnostic.help("an out starts empty: set it before reading it"),
        };
        self.report(diagnostic);
        // Reported once: treat it as full from here.
        self.set(path, Own::Full);
    }

    // ---- state ---------------------------------------------------------

    /// What the owned place `path` holds (a leaf above it covers it), or
    /// `None` when its local is not tracked here.
    fn own_at(&self, path: &HirPath) -> Option<Own> {
        let mut own = self.state.as_ref()?.get(&path.local)?;
        for field in &path.fields {
            match own {
                Own::Parts(parts) => own = &parts[*field as usize],
                leaf => return Some(leaf.clone()),
            }
        }
        Some(own.clone())
    }

    /// Makes the owned place `path` hold `value`.
    fn set(&mut self, path: &HirPath, value: Own) {
        let ty = self.local_ty(path.local);
        let Some(state) = &mut self.state else {
            return;
        };
        let root = state.entry(path.local).or_insert(Own::Full);
        set_in(self.c, root, ty, &path.fields, value);
    }

    /// The drops of what scope `index` still owns, newest local first.
    fn frame_drops(&mut self, index: usize) -> Vec<HirDrop> {
        let mut drops = Vec::new();
        for local in self.frames[index].clone().into_iter().rev() {
            let path = HirPath::local(local);
            if let Some(own) = self.own_at(&path) {
                let ty = self.local_ty(local);
                self.drops_of(path, ty, &own, &mut drops);
            }
        }
        drops
    }

    /// The drops of what `path` (of type `ty`) holds: a full place drops, a
    /// maybe-full one drops when its flag is set, and a partly moved object
    /// drops the fields still in it, in declaration order.
    fn drops_of(&mut self, path: HirPath, ty: TypeRef, own: &Own, out: &mut Vec<HirDrop>) {
        if !self.c.needs_drop(ty) {
            return;
        }
        match own {
            Own::Full => out.push(HirDrop {
                path,
                ty,
                flag: None,
            }),
            // A call's out bound to a hidden local is always moved out on
            // both sides of the flag's test, so none is left to drop.
            Own::Empty(_) | Own::Cond { .. } => {}
            Own::Maybe(_) => {
                let flag = self.flag(&path);
                out.push(HirDrop {
                    path,
                    ty,
                    flag: Some(flag),
                });
            }
            Own::Parts(parts) => {
                let tys = self.c.field_types(ty);
                for (index, (part, ty)) in parts.iter().zip(tys).enumerate() {
                    let mut field = path.clone();
                    field.fields.push(u32::try_from(index).unwrap_or(u32::MAX));
                    self.drops_of(field, ty, part, out);
                }
            }
        }
    }

    /// The drop flag of `path`, added the first time it is needed.
    fn flag(&mut self, path: &HirPath) -> LocalId {
        if self.dry {
            return LocalId(u32::MAX);
        }
        if let Some((_, flag)) = self.flags.iter().find(|(flagged, _)| flagged == path) {
            return *flag;
        }
        let id = LocalId(u32::try_from(self.locals.len()).unwrap_or(u32::MAX));
        let span = self.locals[path.local.0 as usize].span;
        self.locals.push(HirLocal {
            name: String::new(),
            ty: Builtin::Bit8.value(),
            span,
        });
        self.flags.push((path.clone(), id));
        id
    }

    fn local_ty(&self, local: LocalId) -> TypeRef {
        self.locals[local.0 as usize].ty
    }

    /// How a diagnostic names `path`: `a.b.c`.
    fn path_name(&self, path: &HirPath) -> String {
        let local = &self.locals[path.local.0 as usize];
        let mut name = if local.name.is_empty() {
            "this value".to_owned()
        } else {
            local.name.clone()
        };
        let mut ty = local.ty;
        for field in &path.fields {
            let Some(fields) = self.c.fields_of(ty.id) else {
                break;
            };
            let (field_name, field_ty) = &fields[*field as usize];
            name.push('.');
            name.push_str(field_name);
            ty = *field_ty;
        }
        name
    }

    /// Reports an ownership error (none on a trial walk). It records a
    /// requirement, so a failing generic instance folds it into `E042`.
    fn report(&mut self, diagnostic: Diagnostic) {
        if !self.dry {
            self.c
                .emit_requirement(diagnostic, Requirement::Ownership, &[]);
        }
    }
}

/// Sets the place `fields` inside `node` (of type `ty`) to `value`,
/// splitting leaves into fields on the way.
fn set_in(c: &Checker, node: &mut Own, ty: TypeRef, fields: &[u32], value: Own) {
    let Some((first, rest)) = fields.split_first() else {
        *node = value;
        return;
    };
    if !matches!(node, Own::Parts(_)) {
        if rest.is_empty() && node.same(&value) {
            return;
        }
        let tys = c.field_types(ty);
        *node = Own::Parts(node.expand(tys.len()));
    }
    let tys = c.field_types(ty);
    if let (Own::Parts(parts), Some(field_ty)) = (&mut *node, tys.get(*first as usize)) {
        set_in(c, &mut parts[*first as usize], *field_ty, rest, value);
    }
    *node = std::mem::replace(node, Own::Full).normalize();
}

/// Where two paths join: per local, per field.
fn merge(c: &Checker, ty: TypeRef, a: &Own, b: &Own) -> Own {
    match (a, b) {
        (Own::Full, Own::Full) => Own::Full,
        (Own::Empty(at), Own::Empty(other)) => Own::Empty(at.or(*other)),
        (Own::Cond { .. }, Own::Cond { .. }) if a.same(b) => a.clone(),
        (Own::Parts(_), _) | (_, Own::Parts(_)) => {
            let tys = c.field_types(ty);
            let parts = a
                .expand(tys.len())
                .iter()
                .zip(b.expand(tys.len()).iter())
                .zip(&tys)
                .map(|((one, other), ty)| merge(c, *ty, one, other))
                .collect();
            Own::Parts(parts).normalize()
        }
        _ => Own::Maybe(a.moved_at().or(b.moved_at())),
    }
}

/// Merges the states of two paths that join.
fn merge_states(c: &Checker, locals: &[HirLocal], a: State, b: State) -> State {
    match (a, b) {
        (None, other) | (other, None) => other,
        (Some(mut a), Some(b)) => {
            for (id, own) in b {
                let merged = match a.get(&id) {
                    Some(mine) => merge(c, locals[id.0 as usize].ty, mine, &own),
                    None => own,
                };
                a.insert(id, merged);
            }
            Some(a)
        }
    }
}

/// Whether two states say the same, spans aside.
fn same_states(a: &State, b: &State) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(id, own)| b.get(id).is_some_and(|other| own.same(other)))
        }
        _ => false,
    }
}

/// The state on the side of `if flag` that is `taken`: the outs bound
/// under that call's flag are full or empty there.
fn refine(state: State, flag: Option<LocalId>, taken: bool) -> State {
    let (Some(mut state), Some(flag)) = (state.clone(), flag) else {
        return state;
    };
    for own in state.values_mut() {
        if let Own::Cond {
            flag: of,
            full_when,
        } = own
        {
            if *of == flag {
                *own = if *full_when == taken {
                    Own::Full
                } else {
                    Own::Empty(None)
                };
            }
        }
    }
    Some(state)
}

/// Where a place that is not an owned path lives.
fn origin(place: &HirExpr) -> Origin {
    match &place.kind {
        HirExprKind::Global(_) => Origin::Global,
        HirExprKind::Field { base, .. } if !base.ty.is_ref && base.is_place() => origin(base),
        HirExprKind::Field { base, .. } if !base.ty.is_ref => Origin::Temporary,
        _ => Origin::Ref,
    }
}

/// Appends a `Drop` of `drops` at `span`, when there are any.
fn push_drops(out: &mut Vec<HirStmt>, drops: Vec<HirDrop>, span: Span) {
    if !drops.is_empty() {
        out.push(HirStmt::Drop { drops, span });
    }
}

/// The last byte of `span`: a block's closing `}`.
fn end_of(span: Span) -> Span {
    if span.is_dummy() || span.is_empty() {
        return span;
    }
    Span {
        lo: BytePos(span.hi.0 - 1),
        ..span
    }
}
