//! Last uses: which reads of a whole local are the last on every path
//! from them, so the ownership pass may move a copyable value there
//! instead of copying it (`docs/moves-plan.md`, phase 3).
//!
//! It is a backward walk over the body, before the ownership pass: the
//! live set holds the locals some path from the current point still
//! reads. A whole assignment, a declaration or a capture ends a local's
//! value; `return` and `fail` read the outs, and the body's end the
//! success outs (all outs, to keep it simple). A loop is walked until the
//! set at its head stops changing; the sets only grow, so it ends.
//!
//! A read is named by its local and span. Typeck may give one span to
//! reads at different points (a desugaring), so a read is a last use only
//! when no read of that local with that span, in any round, has the local
//! live after it. Counting too many reads only keeps a copy, so anything
//! unsure counts as a read.

use std::collections::HashSet;

use lilac_hir::{HirBlock, HirCallee, HirExpr, HirExprKind, HirStmt, Intrinsic, LocalId};
use lilac_span::Span;

/// The reads of a whole local (by local and span) that are its last on
/// every path from them.
pub(crate) fn last_uses(body: &HirBlock, outs: &[LocalId]) -> HashSet<(LocalId, Span)> {
    let mut walk = Walk {
        outs: outs.iter().copied().collect(),
        loops: Vec::new(),
        seen: HashSet::new(),
        live_after: HashSet::new(),
    };
    let end = walk.outs.clone();
    walk.block(body, end);
    walk.seen.difference(&walk.live_after).copied().collect()
}

type Live = HashSet<LocalId>;

/// One enclosing loop: what is live after it (at a `break`) and at its
/// head (at a `continue`).
struct LoopLive {
    breaks: Live,
    continues: Live,
}

struct Walk {
    outs: Live,
    loops: Vec<LoopLive>,
    /// Every read of a whole local met.
    seen: HashSet<(LocalId, Span)>,
    /// The reads met with their local live after them.
    live_after: HashSet<(LocalId, Span)>,
}

impl Walk {
    /// The live set at the start of `block`, given `live` at its end.
    fn block(&mut self, block: &HirBlock, mut live: Live) -> Live {
        for stmt in block.stmts.iter().rev() {
            live = self.stmt(stmt, live);
        }
        live
    }

    fn stmt(&mut self, stmt: &HirStmt, mut live: Live) -> Live {
        match stmt {
            HirStmt::Let { local, value } => {
                live.remove(local);
                self.expr(value, live)
            }
            HirStmt::MultiLet { locals, call } => {
                for local in locals {
                    live.remove(local);
                }
                self.expr(call, live)
            }
            HirStmt::Assign { place, value, .. } => {
                let live = match place.kind {
                    HirExprKind::Local(local) => {
                        live.remove(&local);
                        live
                    }
                    _ => self.expr(place, live),
                };
                self.expr(value, live)
            }
            HirStmt::Drop { drops, .. } => {
                live.extend(drops.iter().map(|drop| drop.path.local));
                live
            }
            HirStmt::Expr(expr) => {
                let ends = matches!(
                    expr.kind,
                    HirExprKind::Call {
                        callee: HirCallee::Intrinsic(Intrinsic::Panic | Intrinsic::Trap),
                        ..
                    } | HirExprKind::CompileError { .. }
                );
                if ends {
                    live.clear();
                }
                self.expr(expr, live)
            }
            HirStmt::Return(_) | HirStmt::Fail(_) => self.outs.clone(),
            HirStmt::Break(_) => self.loops.last().map_or(live, |frame| frame.breaks.clone()),
            HirStmt::Continue(_) => self
                .loops
                .last()
                .map_or(live, |frame| frame.continues.clone()),
            HirStmt::If { cond, then, els } => {
                let mut start = self.block(then, live.clone());
                start.extend(self.block(els, live));
                self.expr(cond, start)
            }
            HirStmt::Loop(body) => {
                let mut head = Live::new();
                loop {
                    self.loops.push(LoopLive {
                        breaks: live.clone(),
                        continues: head.clone(),
                    });
                    let start = self.block(body, head.clone());
                    self.loops.pop();
                    if start.is_subset(&head) {
                        return head;
                    }
                    head.extend(start);
                }
            }
            HirStmt::Match {
                scrutinee,
                arms,
                els,
            } => {
                let mut start = self.block(els, live.clone());
                for arm in arms {
                    let mut arm_start = self.block(&arm.body, live.clone());
                    if let Some(capture) = arm.capture {
                        arm_start.remove(&capture);
                    }
                    start.extend(arm_start);
                }
                self.expr(scrutinee, start)
            }
            HirStmt::Block(block) => self.block(block, live),
        }
    }

    /// The live set before `expr`, given `live` after it. Operands are
    /// walked in reverse of the order the ownership pass reads them.
    fn expr(&mut self, expr: &HirExpr, mut live: Live) -> Live {
        match &expr.kind {
            HirExprKind::Local(local) => {
                let key = (*local, expr.span);
                self.seen.insert(key);
                if live.contains(local) {
                    self.live_after.insert(key);
                }
                live.insert(*local);
                live
            }
            HirExprKind::Field { base: inner, .. }
            | HirExprKind::Deref(inner)
            | HirExprKind::Move(inner)
            | HirExprKind::RefOf(inner)
            | HirExprKind::Retype(inner)
            | HirExprKind::Not(inner)
            | HirExprKind::Assumed {
                base: Some(inner), ..
            } => self.expr(inner, live),
            HirExprKind::Struct(values) => {
                for value in values.iter().rev() {
                    live = self.expr(value, live);
                }
                live
            }
            HirExprKind::Format(format) => {
                for hole in format.holes.iter().rev() {
                    live = self.expr(&hole.value, live);
                }
                live
            }
            HirExprKind::Variant { payload, .. } => match payload {
                Some(payload) => self.expr(payload, live),
                None => live,
            },
            HirExprKind::Call { callee, args, .. } => {
                for arg in args.iter().rev() {
                    live = self.expr(arg, live);
                }
                match callee {
                    HirCallee::Value(callee) => self.expr(callee, live),
                    HirCallee::Func(_) | HirCallee::Intrinsic(_) | HirCallee::Assumed { .. } => {
                        live
                    }
                }
            }
            HirExprKind::Eq(lhs, rhs) => {
                let live = self.expr(rhs, live);
                self.expr(lhs, live)
            }
            HirExprKind::If { cond, then, els } => {
                let mut start = self.expr(then, live.clone());
                start.extend(self.expr(els, live));
                self.expr(cond, start)
            }
            HirExprKind::Logic { lhs, rhs, .. } => {
                let mut start = self.expr(rhs, live.clone());
                start.extend(live);
                self.expr(lhs, start)
            }
            HirExprKind::Match {
                scrutinee,
                arms,
                els,
            } => {
                let mut start = self.expr(els, live.clone());
                for arm in arms {
                    let mut arm_start = self.expr(&arm.value, live.clone());
                    if let Some(capture) = arm.capture {
                        arm_start.remove(&capture);
                    }
                    start.extend(arm_start);
                }
                self.expr(scrutinee, start)
            }
            HirExprKind::Int(_)
            | HirExprKind::Float(_)
            | HirExprKind::Str(_)
            | HirExprKind::FuncRef(_)
            | HirExprKind::Const(_)
            | HirExprKind::Global(_)
            | HirExprKind::SizeOf(_)
            | HirExprKind::AlignOf(_)
            | HirExprKind::Unanswered
            | HirExprKind::Assumed { base: None, .. }
            | HirExprKind::CompileError { .. }
            | HirExprKind::Poison => live,
        }
    }
}
