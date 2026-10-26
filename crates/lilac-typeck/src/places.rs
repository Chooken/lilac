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
//!   place is `E063`, which says why an implicit move happened
//!   ([`Pass::moved_here`]); moving out through a `ref`, out of a global or a
//!   temporary, or out of a type with its own `@on_drop` is `E064`.
//! - Taking a `ref` to a local (or a field of it) borrows it for the rest
//!   of its scope, on the paths that took it ([`Flow::borrowed`]); a
//!   borrowed local moves only by `@move`, and an implicit move of one is
//!   `E079` (`docs/moves-plan.md`).
//! - A copyable local's last by-value use moves it instead of copying
//!   ([`Pass::last_use`], over [`crate::liveness::last_uses`]) when no
//!   `ref` to it was taken.
//! - Every block ends with a [`HirStmt::Drop`] of its full locals, and each
//!   early exit drops the scopes it leaves. A place that may be empty where
//!   it drops gets a flag ([`HirFunc::flags`]), which lowering keeps.
//! - An assignment says what happens to the old value ([`HirOld`]).
//! - Outs start empty. Success outs must be full at every `return` and at
//!   the end of the body, failure outs at every `fail` (`E065`); an out of
//!   the other side that is full there drops.

use std::collections::{HashMap, HashSet};

use lilac_error::{codes, Diagnostic};
use lilac_hir::{
    HirBlock, HirCallee, HirDrop, HirExpr, HirExprKind, HirFunc, HirLocal, HirOld, HirPath,
    HirStmt, Intrinsic, LocalId,
};
use lilac_span::{BytePos, Span};
use lilac_ty::{Builtin, TypeId, TypeKind, TypeRef};

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
    /// An object `@discard` ended (`at`): its `@on_drop` does not run and
    /// each field is a place of its own. `torn` when a path that reaches
    /// here did not discard it (`E066`, reported where it drops).
    Discarded {
        at: Span,
        parts: Vec<Own>,
        torn: bool,
    },
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
            (Own::Parts(a), Own::Parts(b)) => same_parts(a, b),
            (
                Own::Discarded { parts, torn, .. },
                Own::Discarded {
                    parts: other,
                    torn: other_torn,
                    ..
                },
            ) => torn == other_torn && same_parts(parts, other),
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
            Own::Parts(parts) | Own::Discarded { parts, .. } => {
                parts.iter().find_map(Own::moved_at)
            }
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
            Own::Discarded { at, parts, .. } => {
                if parts
                    .iter()
                    .all(|part| matches!(part.summary(), Summary::Empty(_)))
                {
                    Summary::Empty(self.moved_at())
                } else {
                    Summary::Discarded(*at)
                }
            }
        }
    }

    /// One entry per field: the parts, or `n` copies of a leaf.
    fn expand(&self, n: usize) -> Vec<Own> {
        match self {
            Own::Parts(parts) | Own::Discarded { parts, .. } => parts.clone(),
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

/// Whether two field lists say the same, spans aside.
fn same_parts(a: &[Own], b: &[Own]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.same(b))
}

/// [`Own::summary`].
enum Summary {
    Full,
    Empty(Option<Span>),
    Maybe(Option<Span>),
    Partly(Option<Span>),
    /// `@discard`ed (where) with fields left in it.
    Discarded(Span),
}

/// What the pass knows at one point that a path reaches.
#[derive(Clone, Debug, Default)]
struct Flow {
    /// Each tracked local's [`Own`].
    owns: HashMap<LocalId, Own>,
    /// The locals a `ref` was taken to (to them or a field), each with
    /// where the first was: never moved but by `@move` (`E079`). A `ref`
    /// has no lifetime, so a local stays borrowed until its scope ends.
    borrowed: HashMap<LocalId, Span>,
}

impl Flow {
    /// Forgets `local`, whose scope ended.
    fn forget(&mut self, local: LocalId) {
        self.owns.remove(&local);
        self.borrowed.remove(&local);
    }
}

/// The state at one point, or `None` where no path reaches.
type State = Option<Flow>;

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
        let mut state = Flow::default();
        for id in &func.params {
            state.owns.insert(*id, Own::Full);
        }
        for id in &func.returns {
            state.owns.insert(*id, Own::Empty(None));
        }
        let last = crate::liveness::last_uses(&body, &func.returns);
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
            torn: Vec::new(),
            heights: HashMap::new(),
            bound: None,
            implicit: HashMap::new(),
            last,
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

    /// Why a `ty` does not copy: the type in it (`ty` itself, or one of
    /// its slots, however deep) with an `@on_drop` and no `@on_copy`.
    /// `None` when it copies.
    fn copy_blocker(&mut self, ty: TypeRef) -> Option<TypeRef> {
        if self.is_copyable(ty) {
            return None;
        }
        if let TypeKind::Array { elem, .. } = self.types.kind(ty.id).clone() {
            return self.copy_blocker(TypeRef::value(elem));
        }
        if self.raii_hook("@on_drop", ty.id).is_some() {
            return Some(ty);
        }
        self.slot_types(ty)
            .into_iter()
            .find_map(|slot| self.copy_blocker(slot))
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

    /// Whether copying a `ty` runs an `@on_copy`: its own, or a slot's
    /// (lowering's rule).
    pub(crate) fn needs_copy(&mut self, ty: TypeRef) -> bool {
        if ty.is_ref {
            return false;
        }
        if let Some(known) = self.copy_hooked.get(&ty.id) {
            return *known;
        }
        let result = match self.types.kind(ty.id) {
            TypeKind::Object(_) | TypeKind::Instance { .. } => {
                self.raii_hook("@on_copy", ty.id).is_some()
                    || self
                        .slot_types(ty)
                        .into_iter()
                        .any(|slot| self.needs_copy(slot))
            }
            _ => return false,
        };
        if self.out.layouts.contains_key(&ty.id) {
            self.copy_hooked.insert(ty.id, result);
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
}

/// How the ownership state splits a place into fields: the checker's
/// layouts, or a fixed table in the unit tests.
trait FieldTypes {
    /// The field types of an object type (none for anything else, and none
    /// behind a `ref`: an owned path never goes through one).
    fn field_types(&self, ty: TypeRef) -> Vec<TypeRef>;
}

impl FieldTypes for Checker<'_> {
    fn field_types(&self, ty: TypeRef) -> Vec<TypeRef> {
        if ty.is_ref {
            return Vec::new();
        }
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
    /// The `@discard`s reported as torn (`E066`), so each is reported once.
    torn: Vec<Span>,
    /// [`height`] of each type met, so far.
    heights: HashMap<TypeId, usize>,
    /// [`Pass::round_bound`], once worked out.
    bound: Option<usize>,
    /// The implicit moves (a by-value use or a capture of a type that
    /// does not copy), by span, with the type moved: `E063` says why
    /// such a move happened.
    implicit: HashMap<Span, TypeRef>,
    /// The reads of a whole local that are its last on every path from
    /// them ([`crate::liveness::last_uses`]): one that would copy moves
    /// instead ([`Pass::last_use`]).
    last: HashSet<(LocalId, Span)>,
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
                            state.forget(*local);
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
                        arm.moves =
                            self.bind_capture(&scrutinee, path.as_ref(), arm.by_ref, capture);
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
                state.forget(local);
            }
        }
    }

    /// A loop: its body is walked on copies until the state at its head
    /// (what enters, merged with what each `continue` and the body's end
    /// carry round) stops changing, then for real. After it, the state is
    /// the merge of its `break`s.
    ///
    /// The walk ends: each round's head is the merge of the entry with
    /// what the body makes of the last head, so the heads only climb
    /// ([`merge`] is a join and the body monotone), and a state climbs
    /// at most [`Pass::round_bound`] times. A loop still changing past
    /// that broke the argument, so it is an internal error, never a
    /// guess at the drops.
    fn lower_loop(&mut self, body: &mut HirBlock) {
        let entry = self.state.clone();
        let mut head = entry.clone();
        let dry = self.dry;
        self.dry = true;
        let bound = self.round_bound();
        let mut rounds = 0;
        loop {
            rounds += 1;
            if rounds > bound {
                lilac_error::bug!(
                    body.span,
                    "the ownership state of a loop still changes after {bound} rounds"
                );
            }
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

    /// How many rounds a loop's head can take: one more than the times a
    /// state can climb. Each climb adds a tracked local, borrows one, or
    /// raises one's [`Own`], and a local of type `T` rises at most
    /// [`height`]`(T)` times, so the sum over this body's locals bounds it
    /// (locals added later are drop flags, never tracked).
    fn round_bound(&mut self) -> usize {
        if let Some(bound) = self.bound {
            return bound;
        }
        let mut bound = 1usize;
        for index in 0..self.locals.len() {
            let ty = self.locals[index].ty;
            let rises = height(self.c, &mut self.heights, ty).saturating_add(2);
            bound = bound.saturating_add(rises);
        }
        self.bound = Some(bound);
        bound
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
            state.owns.insert(local, own);
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
            // What is left in it drops field by field (a discarded value's
            // `@on_drop` does not run).
            Own::Parts(_) | Own::Discarded { .. } => {
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
            .and_then(|s| s.owns.get(&path.local))
            .cloned()
        else {
            return true;
        };
        let mut ty = self.local_ty(path.local);
        for (depth, field) in path.fields.iter().enumerate() {
            match own {
                Own::Parts(parts) | Own::Discarded { parts, .. } => {
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
                        diagnostic = self.moved_here(diagnostic, at, "moved here", &name);
                    }
                    let shown = self.c.show_id(ty.id);
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
                Summary::Maybe(_) | Summary::Partly(_) | Summary::Discarded(_) => {
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
            format!("the `{}` out", self.c.show(local.ty))
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
                self.unborrowed(&path, expr.span, expr.ty, None);
                self.implicit.insert(expr.span, expr.ty);
                self.move_path(&path, expr.span);
                let inner = std::mem::replace(expr, HirExpr::poison(expr.span));
                *expr = HirExpr {
                    ty: inner.ty,
                    span: inner.span,
                    kind: HirExprKind::Move(Box::new(inner)),
                };
            } else if consume && self.last_use(&path, expr.span, expr.ty) {
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
            HirExprKind::Struct(values) => {
                for value in values {
                    self.expr(value, true);
                }
            }
            // A hole borrows its value: it is read, never moved.
            HirExprKind::Format(format) => {
                for hole in &mut format.holes {
                    self.expr(&mut hole.value, false);
                }
            }
            HirExprKind::Variant { payload, .. } => {
                if let Some(payload) = payload {
                    self.expr(payload, true);
                }
            }
            HirExprKind::Call {
                callee: HirCallee::Intrinsic(Intrinsic::Discard),
                args,
                ..
            } => {
                if let [place] = args.as_slice() {
                    self.discard(place, span);
                }
            }
            HirExprKind::Call { callee, args, .. } => {
                // The callee is read first: a function reference copies.
                if let HirCallee::Value(callee) = callee {
                    self.expr(callee, true);
                }
                for arg in args {
                    self.expr(arg, true);
                }
                // A `@panic` or `@trap` value (`if c then @panic(m) else
                // x`) ends its path like the statement does.
                if matches!(
                    callee,
                    HirCallee::Intrinsic(Intrinsic::Panic | Intrinsic::Trap)
                ) {
                    self.state = None;
                }
            }
            HirExprKind::RefOf(inner) => {
                self.expr(inner, false);
                if let Some(path) = HirPath::of(inner) {
                    self.borrow(path.local, span);
                }
            }
            HirExprKind::Retype(inner) | HirExprKind::Not(inner) => {
                self.expr(inner, false);
            }
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
                        arm.moves =
                            self.bind_capture(scrutinee, path.as_ref(), arm.by_ref, capture);
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
                            state.forget(capture);
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
            | HirExprKind::Int(_)
            | HirExprKind::Float(_)
            | HirExprKind::Str(_)
            | HirExprKind::FuncRef(_)
            | HirExprKind::Const(_)
            | HirExprKind::SizeOf(_)
            | HirExprKind::AlignOf(_)
            | HirExprKind::Unanswered
            | HirExprKind::Assumed { .. }
            | HirExprKind::CompileError { .. }
            | HirExprKind::Poison => {}
        }
    }

    /// Whether the by-value read of `path` (a `ty`, which copies) at
    /// `span` moves instead: it is a whole local, this read is its last on
    /// every path, no `ref` to it was taken, and copying would run a hook
    /// (a plain-data move and copy are the same bytes). A language rule,
    /// the same in every build: hooks run only where a copy is needed.
    fn last_use(&mut self, path: &HirPath, span: Span, ty: TypeRef) -> bool {
        path.fields.is_empty()
            && self.last.contains(&(path.local, span))
            && !self
                .state
                .as_ref()
                .is_some_and(|state| state.borrowed.contains_key(&path.local))
            && (self.c.needs_copy(ty) || self.c.needs_drop(ty))
    }

    /// A value-match capture: full in its arm, owned by no scope.
    fn init_free(&mut self, local: LocalId) {
        if let Some(state) = &mut self.state {
            state.owns.insert(local, Own::Full);
        }
    }

    /// Walks what a place that is not an owned path reads on its way: the
    /// `ref` it goes through, or the temporary it is a field of.
    fn borrowed_place(&mut self, place: &mut HirExpr) {
        match &mut place.kind {
            // A stand-in field reads its object as a `ref` would.
            HirExprKind::Deref(inner)
            | HirExprKind::Assumed {
                base: Some(inner), ..
            } => self.expr(inner, false),
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

    /// Binds an arm's `capture`: whether it moves the payload out. A
    /// by-reference capture moves nothing and borrows the matched place.
    fn bind_capture(
        &mut self,
        scrutinee: &HirExpr,
        path: Option<&HirPath>,
        by_ref: bool,
        capture: LocalId,
    ) -> bool {
        if by_ref {
            if let Some(path) = path {
                let span = self.locals[capture.0 as usize].span;
                self.borrow(path.local, span);
            }
            return false;
        }
        self.capture(scrutinee, path, capture)
    }

    /// Marks `local` borrowed: a `ref` to it (or a field) is taken at `at`.
    fn borrow(&mut self, local: LocalId, at: Span) {
        if let Some(state) = &mut self.state {
            state.borrowed.entry(local).or_insert(at);
        }
    }

    /// `E079` when the owned place `path`, about to move implicitly at
    /// `span` (a `ty` that does not copy), is borrowed: a `ref` may still
    /// point at it. `scrutinee` is the matched value's span when a capture
    /// moves a payload out of it. It still moves after, so one mistake is
    /// one error.
    fn unborrowed(&mut self, path: &HirPath, span: Span, ty: TypeRef, scrutinee: Option<Span>) {
        let local = &self.locals[path.local.0 as usize];
        // A hidden local is the compiler's: it never moves while borrowed.
        if local.name.is_empty() {
            return;
        }
        let local_name = local.name.clone();
        let Some(at) = self
            .state
            .as_ref()
            .and_then(|state| state.borrowed.get(&path.local))
            .copied()
        else {
            return;
        };
        let name = self.path_name(path);
        let shown = self.c.show(ty);
        let mut diagnostic = Diagnostic::error(
            codes::E079,
            format!("cannot move `{name}` implicitly: a reference to `{local_name}` was taken"),
        )
        .label(
            at,
            false,
            format!("a reference to `{local_name}` is taken here"),
        )
        .note(format!(
            "a `ref` may still point at `{local_name}`, so it moves only by `@move`"
        ));
        diagnostic = match scrutinee {
            None => diagnostic
                .label(
                    span,
                    true,
                    format!("`{shown}` cannot be copied, so this would move `{name}`"),
                )
                .fix(
                    span,
                    format!("@move({name})"),
                    format!("write `@move({name})` if no reference to `{local_name}` is used after this"),
                ),
            Some(matched) => diagnostic
                .label(span, true, format!("`{shown}` cannot be copied, so this capture would move it out"))
                .label(matched, false, format!("out of `{name}`"))
                .help(format!(
                    "capture it by reference (`ref {shown}`), or match on `@move({name})` if no reference to `{local_name}` is used after this"
                )),
        };
        self.report(diagnostic);
    }

    /// Binds `capture` from the payload of `scrutinee` (the owned place
    /// `path`, when it is one): whether it moves the payload out, which
    /// uses the matched value up. A payload that copies is copied. A
    /// by-reference capture (`HirArm::by_ref`) never comes here: it moves
    /// nothing.
    fn capture(&mut self, scrutinee: &HirExpr, path: Option<&HirPath>, capture: LocalId) -> bool {
        let ty = self.local_ty(capture);
        if self.c.is_copyable(ty) {
            return false;
        }
        let span = self.locals[capture.0 as usize].span;
        let owner = scrutinee.ty.deref();
        if self.c.has_own_drop(owner) {
            let shown = self.c.show_id(owner.id);
            self.report(
                Diagnostic::error(
                    codes::E064,
                    format!("cannot move a payload out of `{shown}`, which has its own `@on_drop`"),
                )
                .label(span, true, "this capture would move the payload out")
                .label(scrutinee.span, false, format!("a `{shown}`"))
                .help(format!(
                    "`{}` cannot be copied; match through a method of `{shown}` instead",
                    self.c.show(ty)
                )),
            );
            return false;
        }
        match path {
            Some(path) => {
                self.unborrowed(path, span, ty, Some(scrutinee.span));
                self.implicit.insert(span, ty);
                self.set(path, Own::Empty(Some(span)));
                true
            }
            None if !scrutinee.is_place() => true,
            None => {
                let shown = self.c.show(ty);
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
        let mut own = self
            .state
            .as_ref()
            .and_then(|state| state.owns.get(&path.local))
            .cloned();
        for (depth, field) in path.fields.iter().enumerate() {
            // A discarded value's `@on_drop` no longer runs.
            let discarded = matches!(own, Some(Own::Discarded { .. }));
            if let Some(Own::Parts(parts) | Own::Discarded { parts, .. }) = own {
                own = Some(parts[*field as usize].clone());
            }
            if !discarded && self.c.has_own_drop(ty) {
                let shown = self.c.show_id(ty.id);
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
        let shown = self.c.show(place.ty);
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

    /// Labels the move at `at` that emptied `name` (`label`), and, when it
    /// was implicit, says why it moved and how to keep the value.
    fn moved_here(
        &mut self,
        diagnostic: Diagnostic,
        at: Span,
        label: &str,
        name: &str,
    ) -> Diagnostic {
        let Some(ty) = self.implicit.get(&at).copied() else {
            return diagnostic.label(at, false, label);
        };
        let shown = self.c.show(ty);
        let diagnostic =
            diagnostic.label(at, false, format!("{label}: `{shown}` cannot be copied"));
        let Some(blocker) = self.c.copy_blocker(ty) else {
            return diagnostic;
        };
        let cause = self.c.show(blocker);
        let why = if blocker == ty {
            format!("`{shown}` has an `@on_drop` and no `@on_copy`, so a use by value moves it")
        } else {
            format!(
                "`{shown}` holds a `{cause}`, which has an `@on_drop` and no `@on_copy`, so a use by value moves it"
            )
        };
        diagnostic.note(why).help(format!(
            "to keep using `{name}`, pass a `ref` to it where it moved, or give `{cause}` an `@on_copy` so it copies"
        ))
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
            Summary::Discarded(at) => (
                format!("`{name}` was discarded"),
                "used here as a whole",
                Some((at, "discarded here")),
            ),
        };
        let discarded = matches!(own.summary(), Summary::Discarded(_));
        let mut diagnostic = Diagnostic::error(codes::E063, message).label(span, true, used);
        diagnostic = match moved {
            Some((at, label)) => self.moved_here(diagnostic, at, label, &name),
            None => diagnostic.help("an out starts empty: set it before reading it"),
        };
        if discarded {
            diagnostic = diagnostic.help("after `@discard`, use its fields one by one");
        }
        self.report(diagnostic);
        // Reported once: treat it as full from here.
        self.set(path, Own::Full);
    }

    // ---- state ---------------------------------------------------------

    /// What the owned place `path` holds (a leaf above it covers it), or
    /// `None` when its local is not tracked here.
    fn own_at(&self, path: &HirPath) -> Option<Own> {
        let mut own = self.state.as_ref()?.owns.get(&path.local)?;
        for field in &path.fields {
            match own {
                Own::Parts(parts) | Own::Discarded { parts, .. } => own = &parts[*field as usize],
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
        let root = state.owns.entry(path.local).or_insert(Own::Full);
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
    /// maybe-full one drops when its flag is set, and a partly moved or
    /// discarded object drops the fields still in it, in declaration order.
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
            Own::Parts(parts) | Own::Discarded { parts, .. } => {
                if let Own::Discarded { at, torn: true, .. } = own {
                    self.torn(&path, *at);
                }
                let tys = self.c.field_types(ty);
                for (index, (part, ty)) in parts.iter().zip(tys).enumerate() {
                    let mut field = path.clone();
                    field.fields.push(u32::try_from(index).unwrap_or(u32::MAX));
                    self.drops_of(field, ty, part, out);
                }
            }
        }
    }

    /// `E066` (once) for `path`, discarded at `at` on some paths only.
    fn torn(&mut self, path: &HirPath, at: Span) {
        if self.dry || self.torn.contains(&at) {
            return;
        }
        self.torn.push(at);
        let name = self.path_name(path);
        self.report(
            Diagnostic::error(
                codes::E066,
                format!("`{name}` is discarded on some paths only"),
            )
            .label(at, true, "discarded here")
            .note(format!(
                "where the paths join, `{name}`'s `@on_drop` would run on some and not on others"
            ))
            .help("discard it on every path that keeps it, or on none"),
        );
    }

    /// `@discard(place)`: the place's `@on_drop` will not run, and its
    /// fields become places of their own. (Typeck checked it is an owned
    /// object of this function's type.)
    fn discard(&mut self, place: &HirExpr, span: Span) {
        let Some(path) = HirPath::of(place) else {
            return;
        };
        self.require_full(&path, place.span);
        if !self.c.has_own_drop(place.ty) {
            // Nothing to skip: its fields already drop one by one.
            return;
        }
        let count = self.c.field_types(place.ty).len();
        self.set(
            &path,
            Own::Discarded {
                at: span,
                parts: vec![Own::Full; count],
                torn: false,
            },
        );
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
fn set_in(c: &impl FieldTypes, node: &mut Own, ty: TypeRef, fields: &[u32], value: Own) {
    let Some((first, rest)) = fields.split_first() else {
        *node = value;
        return;
    };
    if !matches!(node, Own::Parts(_) | Own::Discarded { .. }) {
        if rest.is_empty() && node.same(&value) {
            return;
        }
        let tys = c.field_types(ty);
        *node = Own::Parts(node.expand(tys.len()));
    }
    let tys = c.field_types(ty);
    if let (Own::Parts(parts) | Own::Discarded { parts, .. }, Some(field_ty)) =
        (&mut *node, tys.get(*first as usize))
    {
        set_in(c, &mut parts[*first as usize], *field_ty, rest, value);
    }
    *node = std::mem::replace(node, Own::Full).normalize();
}

/// How many times the [`Own`] of a place of type `ty` can rise under
/// [`merge`] before it can rise no more: once per leaf (full, empty or a
/// call's out, to maybe), and three times per object node (a whole value,
/// to parts, to discarded, to torn). `heights` keeps each type's answer.
fn height(c: &impl FieldTypes, heights: &mut HashMap<TypeId, usize>, ty: TypeRef) -> usize {
    let fields = c.field_types(ty);
    if fields.is_empty() {
        return 1;
    }
    if let Some(known) = heights.get(&ty.id) {
        return *known;
    }
    // A type is never inside itself by value (`E053`); the entry only
    // stops the walk if that broke.
    heights.insert(ty.id, 1);
    let mut total = 3usize;
    for field in fields {
        total = total.saturating_add(height(c, heights, field));
    }
    heights.insert(ty.id, total);
    total
}

/// Where two paths join: per local, per field.
///
/// This is the join of a lattice of finite height (a loop's walk relies
/// on it, [`Pass::lower_loop`]): commutative, associative and idempotent
/// (spans aside), so `a` is below `b` when `merge(a, b)` is `b`. Full,
/// empty and a call's out are at the bottom and rise to maybe; a whole
/// object rises to parts, parts to discarded, and `torn` only turns on.
fn merge(c: &impl FieldTypes, ty: TypeRef, a: &Own, b: &Own) -> Own {
    match (a, b) {
        (Own::Full, Own::Full) => Own::Full,
        (Own::Empty(at), Own::Empty(other)) => Own::Empty(at.or(*other)),
        (Own::Cond { .. }, Own::Cond { .. }) if a.same(b) => a.clone(),
        // A path that neither discarded it nor moved it out still runs its
        // `@on_drop`: torn.
        (Own::Discarded { at, parts, torn }, other)
        | (other, Own::Discarded { at, parts, torn }) => {
            let tys = c.field_types(ty);
            let other_torn = match other {
                Own::Discarded { torn, .. } => *torn,
                Own::Empty(_) => false,
                _ => true,
            };
            let parts = parts
                .iter()
                .zip(other.expand(tys.len()).iter())
                .zip(&tys)
                .map(|((one, other), ty)| merge(c, *ty, one, other))
                .collect();
            Own::Discarded {
                at: *at,
                parts,
                torn: *torn || other_torn,
            }
        }
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
fn merge_states(c: &impl FieldTypes, locals: &[HirLocal], a: State, b: State) -> State {
    match (a, b) {
        (None, other) | (other, None) => other,
        (Some(mut a), Some(b)) => {
            for (id, own) in b.owns {
                let merged = match a.owns.get(&id) {
                    Some(mine) => merge(c, locals[id.0 as usize].ty, mine, &own),
                    None => own,
                };
                a.owns.insert(id, merged);
            }
            // Borrowed on either path is borrowed.
            for (id, at) in b.borrowed {
                a.borrowed.entry(id).or_insert(at);
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
            a.owns.len() == b.owns.len()
                && a.owns
                    .iter()
                    .all(|(id, own)| b.owns.get(id).is_some_and(|other| own.same(other)))
                && a.borrowed.len() == b.borrowed.len()
                && a.borrowed.keys().all(|id| b.borrowed.contains_key(id))
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
    for own in state.owns.values_mut() {
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

#[cfg(test)]
mod tests {
    //! The loop walk ends because [`merge`] is the join of a lattice of
    //! finite height. These tests check that over representative states:
    //! the join laws, monotonicity, and a rank that every strict rise
    //! raises and that never passes [`height`].

    use super::*;

    const LEAF: TypeRef = TypeRef {
        id: TypeId(100),
        is_ref: false,
    };
    /// `structure { a: Leaf  b: Leaf }`.
    const PAIR: TypeRef = TypeRef {
        id: TypeId(101),
        is_ref: false,
    };
    /// `structure { a: Leaf  b: Pair }`.
    const OUTER: TypeRef = TypeRef {
        id: TypeId(102),
        is_ref: false,
    };

    struct Table;

    impl FieldTypes for Table {
        fn field_types(&self, ty: TypeRef) -> Vec<TypeRef> {
            match ty.id {
                TypeId(101) => vec![LEAF, LEAF],
                TypeId(102) => vec![LEAF, PAIR],
                _ => Vec::new(),
            }
        }
    }

    fn le(ty: TypeRef, a: &Own, b: &Own) -> bool {
        merge(&Table, ty, a, b).same(b)
    }

    /// Each strict rise raises it by at least one ([`height`]'s count).
    fn rank(ty: TypeRef, own: &Own) -> usize {
        let fields = Table.field_types(ty);
        match own {
            Own::Parts(parts) => {
                1 + fields
                    .iter()
                    .zip(parts)
                    .map(|(ty, part)| rank(*ty, part))
                    .sum::<usize>()
            }
            Own::Discarded { parts, torn, .. } => {
                2 + usize::from(*torn)
                    + fields
                        .iter()
                        .zip(parts)
                        .map(|(ty, part)| rank(*ty, part))
                        .sum::<usize>()
            }
            leaf if fields.is_empty() => usize::from(matches!(leaf, Own::Maybe(_))),
            leaf => fields.iter().map(|ty| rank(*ty, leaf)).sum(),
        }
    }

    fn leaves(spans: bool) -> Vec<Own> {
        let mut leaves = vec![
            Own::Full,
            Own::Empty(None),
            Own::Maybe(None),
            Own::Cond {
                flag: LocalId(0),
                full_when: true,
            },
            Own::Cond {
                flag: LocalId(0),
                full_when: false,
            },
            Own::Cond {
                flag: LocalId(1),
                full_when: true,
            },
        ];
        if spans {
            leaves.push(Own::Empty(Some(Span::dummy())));
            leaves.push(Own::Maybe(Some(Span::dummy())));
        }
        leaves
    }

    /// Every state of an object whose fields take `fields` (one list per
    /// field): the leaves, each split into parts (normalized), and each
    /// discarded, torn or not.
    fn objects(fields: &[Vec<Own>]) -> Vec<Own> {
        let mut splits: Vec<Vec<Own>> = vec![Vec::new()];
        for options in fields {
            splits = splits
                .into_iter()
                .flat_map(|prefix| {
                    options.iter().map(move |option| {
                        let mut next = prefix.clone();
                        next.push(option.clone());
                        next
                    })
                })
                .collect();
        }
        let mut all = leaves(false);
        for parts in splits {
            for torn in [false, true] {
                all.push(Own::Discarded {
                    at: Span::dummy(),
                    parts: parts.clone(),
                    torn,
                });
            }
            if !parts.iter().all(|part| matches!(part, Own::Full)) {
                all.push(Own::Parts(parts));
            }
        }
        all
    }

    fn pair_states(spans: bool) -> Vec<Own> {
        objects(&[leaves(spans), leaves(spans)])
    }

    fn outer_states() -> Vec<Own> {
        let few = vec![
            Own::Full,
            Own::Empty(None),
            Own::Maybe(None),
            Own::Cond {
                flag: LocalId(0),
                full_when: true,
            },
        ];
        let pairs = vec![
            Own::Full,
            Own::Empty(None),
            Own::Maybe(None),
            Own::Parts(vec![Own::Empty(None), Own::Full]),
            Own::Parts(vec![Own::Maybe(None), Own::Empty(None)]),
            Own::Discarded {
                at: Span::dummy(),
                parts: vec![Own::Full, Own::Full],
                torn: false,
            },
            Own::Discarded {
                at: Span::dummy(),
                parts: vec![Own::Empty(None), Own::Full],
                torn: true,
            },
        ];
        objects(&[few, pairs])
    }

    fn check_laws(ty: TypeRef, states: &[Own]) {
        for a in states {
            assert!(merge(&Table, ty, a, a).same(a), "idempotent: {a:?}");
            for b in states {
                let ab = merge(&Table, ty, a, b);
                assert!(
                    ab.same(&merge(&Table, ty, b, a)),
                    "commutative: {a:?} {b:?}"
                );
                assert!(le(ty, a, &ab), "an upper bound: {a:?} {b:?}");
                if le(ty, a, b) && !a.same(b) {
                    assert!(rank(ty, a) < rank(ty, b), "a rise: {a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn merge_is_a_join() {
        check_laws(LEAF, &leaves(true));
        check_laws(PAIR, &pair_states(true));
        check_laws(OUTER, &outer_states());
    }

    #[test]
    fn merge_is_associative() {
        for (ty, states) in [(PAIR, pair_states(false)), (OUTER, outer_states())] {
            for a in &states {
                for b in &states {
                    let ab = merge(&Table, ty, a, b);
                    for c in &states {
                        let left = merge(&Table, ty, &ab, c);
                        let right = merge(&Table, ty, a, &merge(&Table, ty, b, c));
                        assert!(left.same(&right), "associative: {a:?} {b:?} {c:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn merge_is_monotone() {
        for (ty, states) in [
            (LEAF, leaves(true)),
            (PAIR, pair_states(true)),
            (OUTER, outer_states()),
        ] {
            for a in &states {
                for higher in states.iter().filter(|higher| le(ty, a, higher)) {
                    for b in &states {
                        let low = merge(&Table, ty, a, b);
                        let high = merge(&Table, ty, higher, b);
                        assert!(le(ty, &low, &high), "monotone: {a:?} {higher:?} {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn rank_never_passes_the_height() {
        let mut heights = HashMap::new();
        for (ty, states) in [
            (LEAF, leaves(true)),
            (PAIR, pair_states(true)),
            (OUTER, outer_states()),
        ] {
            let height = height(&Table, &mut heights, ty);
            for state in &states {
                assert!(rank(ty, state) <= height, "{state:?} under {height}");
            }
        }
        assert_eq!(height(&Table, &mut heights, LEAF), 1);
        assert_eq!(height(&Table, &mut heights, PAIR), 5);
        assert_eq!(height(&Table, &mut heights, OUTER), 9);
        // The top of a pair: torn, both fields maybe.
        let top = Own::Discarded {
            at: Span::dummy(),
            parts: vec![Own::Maybe(None), Own::Maybe(None)],
            torn: true,
        };
        assert_eq!(rank(PAIR, &top), 5);
    }
}
