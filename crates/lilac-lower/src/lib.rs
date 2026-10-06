//! HIR -> LIR lowering with zero-init, RAII, and conversions.
//!
//! Assumes checked, error-free HIR (the driver only lowers clean programs):
//! internal inconsistencies are `bug!`, never diagnostics — except genuinely
//! unlowerable constructs, which report `E030` naming the milestone that owns
//! them (M3 strings and exotic intrinsics, M5 nested member stores).

use std::collections::{HashMap, HashSet};

use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_hir::{
    HirBlock, HirCallee, HirExpr, HirExprKind, HirFunc, HirPattern, HirProgram, HirStmt, TempId,
};
use lilac_ir::{
    ArithOp, ConvertOp, LirBlock, LirCallee, LirData, LirDataKind, LirFunc, LirInst, LirModule,
    LirOperand, LirTerm,
};
use lilac_resolve::{DefId, ResolvedProject};
use lilac_span::Span;
use lilac_ty::{TypeId, TypeRef, Types};

/// Lowers typed HIR to Lilac IR.
pub fn lower(
    hir: &HirProgram,
    project: &ResolvedProject,
    types: &Types,
    diags: &mut Diags,
    target_triple: &str,
) -> Result<LirModule, ErrorGuaranteed> {
    let mut lower = Lower {
        project,
        types,
        diags,
        module: LirModule::new(target_triple),
        const_src: HashMap::new(),
        consts: HashMap::new(),
        func_names: HashMap::new(),
        tags: HashMap::new(),
        drops: &hir.drops,
        failed: false,
    };
    let enum_names: HashSet<&str> = project
        .all_items()
        .filter(|(_, item)| matches!(item.kind, lilac_resolve::ResKind::Enum { .. }))
        .map(|(name, _)| name)
        .collect();
    for (name, members) in &hir.layouts {
        let is_aggregate = enum_names.contains(name.as_str());
        let mut layout = Vec::with_capacity(members.len() + 1);
        let mut variants = Vec::with_capacity(members.len());
        for (member, ty) in members {
            variants.push(member.clone());
            if let Some(ty) = ty {
                layout.push((member.clone(), lir_ty(lower.types, *ty)));
            }
        }
        if is_aggregate {
            layout.insert(0, ("tag".to_owned(), "#bit8".to_owned()));
            lower.tags.insert(name.clone(), variants);
        }
        lower.module.layouts.insert(format!("#{name}"), layout);
    }
    for constant in &hir.consts {
        lower.const_src.insert(constant.def, constant.value.clone());
    }
    let defs: Vec<DefId> = hir.consts.iter().map(|constant| constant.def).collect();
    for def in defs {
        let mut inlining = HashSet::new();
        let expanded = lower.expand_const(def, &mut inlining);
        lower.consts.insert(def, expanded);
    }
    for func in &hir.funcs {
        let symbol = lower.symbol(func.def, &func.name, func.is_extern);
        lower.func_names.insert(func.def, symbol);
    }
    for func in &hir.funcs {
        lower.lower_func(func);
    }
    if lower.failed {
        Err(ErrorGuaranteed::new())
    } else {
        Ok(lower.module)
    }
}

/// Layout key for a place type: references address their pointee's layout.
fn layout_key(place_ty: &str) -> &str {
    place_ty.strip_prefix("ref ").unwrap_or(place_ty)
}
fn lir_ty(types: &Types, ty: TypeRef) -> String {
    let base = match types.kind(ty.id) {
        lilac_ty::TypeKind::Func { .. } => "#bit64".to_owned(),
        lilac_ty::TypeKind::Error => "#error".to_owned(),
        lilac_ty::TypeKind::Object(_) => format!("#{}", types.display(ty.id)),
        lilac_ty::TypeKind::Instance { .. } => format!("#{}", types.mangle(ty.id)),
        _ => {
            let display = types.display(ty.id);
            if let Some(rest) = display.strip_prefix('@') {
                format!("#{rest}")
            } else {
                format!("#{display}")
            }
        }
    };
    if ty.is_ref {
        format!("ref {base}")
    } else {
        base
    }
}

struct Lower<'a> {
    /// Resolved namespaces, for qualifying emitted symbols (spec §7.1).
    project: &'a ResolvedProject,
    types: &'a Types,
    diags: &'a mut Diags,
    module: LirModule,
    /// Module constant sources by identity (expanded on demand).
    const_src: HashMap<DefId, HirExpr>,
    /// Expanded module constants by identity (cycle-free).
    consts: HashMap<DefId, HirExpr>,
    /// Emitted function names by identity.
    func_names: HashMap<DefId, String>,
    /// Variant names in declaration order by type display name.
    tags: HashMap<String, Vec<String>>,
    /// Drop hooks by type display name (mangled callees).
    drops: &'a HashMap<String, String>,
    failed: bool,
}

/// Per-function emission state (owned; no borrows into HIR).
struct FuncState {
    ins: Vec<(String, String)>,
    outs: Vec<(String, String)>,
    blocks: Vec<LirBlock>,
    label: String,
    insts: Vec<LirInst>,
    labels: u32,
    regs: u32,
    tmps: u32,
    args: u32,
    /// Source name → (LIR place, type).
    locals: HashMap<String, (String, TypeRef)>,
    /// Shadowing counters by source name (`x.1` can't collide: dots don't lex).
    counts: HashMap<String, u32>,
    /// Join-temp places by identity.
    temps: HashMap<TempId, String>,
    /// Entry-block allocas, hoisted past all branches so dead-block
    /// pruning (fold) can never orphan a place's storage.
    allocas: Vec<LirInst>,
    /// Owned places in declaration order (drops fire for these).
    owned: Vec<(String, TypeRef)>,
    /// Place names to LIR types (field layouts resolve through these).
    ptypes: HashMap<String, String>,
    /// Loop-exit labels, innermost last.
    breaks: Vec<String>,
    /// Whether the current block accepts instructions (false drops dead tails).
    open: bool,
    /// Whether this function is an object member (lazy implicit `instance`).
    is_member: bool,
    /// Enclosing object name for member functions.
    member_parent: Option<String>,
}

impl FuncState {
    fn new() -> Self {
        Self {
            ins: Vec::new(),
            outs: Vec::new(),
            blocks: Vec::new(),
            label: "entry".to_owned(),
            insts: Vec::new(),
            labels: 0,
            regs: 0,
            tmps: 0,
            args: 0,
            locals: HashMap::new(),
            counts: HashMap::new(),
            temps: HashMap::new(),
            allocas: Vec::new(),
            owned: Vec::new(),
            ptypes: HashMap::new(),
            breaks: Vec::new(),
            open: true,
            is_member: false,
            member_parent: None,
        }
    }
}

/// Appends an instruction, opening a fresh unreachable block if needed.
fn emit(fs: &mut FuncState, inst: LirInst) {
    if !fs.open {
        fs.labels += 1;
        fs.label = format!("l.{}", fs.labels);
        fs.open = true;
    }
    fs.insts.push(inst);
}

/// Closes the current block with a terminator (skipped when dead: a
/// previous `return`/`break` already terminated it).
fn end(fs: &mut FuncState, term: LirTerm) {
    if !fs.open {
        return;
    }
    let insts = std::mem::take(&mut fs.insts);
    let label = std::mem::take(&mut fs.label);
    fs.blocks.push(LirBlock {
        label,
        insts,
        term,
        span: Span::dummy(),
    });
    fs.open = false;
}

/// Opens a labeled block (branch targets are always reachable).
fn at_label(fs: &mut FuncState, label: String) {
    fs.label = label;
    fs.open = true;
}

/// Queues an entry-block allocation (flushed to the top of the first
/// block at function finalization, ahead of every branch).
fn emit_alloca(fs: &mut FuncState, ty: String, place: String, span: Span) {
    fs.allocas.push(LirInst::Alloca { ty, place, span });
}

/// Binds a source name to a fresh place (shadowing gets `.N` suffixes,
/// which never collide: dots don't lex in Lilac).
fn bind_place(fs: &mut FuncState, base: &str, ty: TypeRef) -> String {
    let place = if fs.locals.contains_key(base) {
        let counter = fs.counts.entry(base.to_owned()).or_insert(0);
        *counter += 1;
        format!("{base}.{counter}")
    } else {
        base.to_owned()
    };
    fs.locals.insert(base.to_owned(), (place.clone(), ty));
    place
}

/// Generates a fresh block label (`l.N`).
fn fresh_label(fs: &mut FuncState) -> String {
    fs.labels += 1;
    format!("l.{}", fs.labels)
}

/// Generates a fresh register (`&N`).
fn fresh_reg(fs: &mut FuncState) -> String {
    fs.regs += 1;
    format!("{}", fs.regs)
}

/// Maps an arithmetic intrinsic name (signed/unsigned integer and float) to
/// its `ArithOp`, or `None` for a non-arithmetic intrinsic.
fn arith_op_for(name: &str) -> Option<ArithOp> {
    Some(match name {
        "@i_add" => ArithOp::IAdd,
        "@i_sub" => ArithOp::ISub,
        "@i_mul" => ArithOp::IMul,
        "@i_div" => ArithOp::IDiv,
        "@i_div_u" => ArithOp::IDivU,
        "@i_mod" => ArithOp::IMod,
        "@i_mod_u" => ArithOp::IModU,
        "@f_add" => ArithOp::FAdd,
        "@f_sub" => ArithOp::FSub,
        "@f_mul" => ArithOp::FMul,
        "@f_div" => ArithOp::FDiv,
        "@f_mod" => ArithOp::FMod,
        "@i_less_than" => ArithOp::ILt,
        "@i_less_than_u" => ArithOp::ILtU,
        "@i_greater_than" => ArithOp::IGt,
        "@i_greater_than_u" => ArithOp::IGtU,
        "@f_less_than" => ArithOp::FLt,
        "@f_greater_than" => ArithOp::FGt,
        _ => return None,
    })
}

/// Converts a layout position to a field index (layouts never approach
/// `u32` limits; overflow is an internal error).
fn field_idx(index: usize, span: Span) -> u32 {
    u32::try_from(index).unwrap_or_else(|_| lilac_error::bug!(span, "field index overflow"))
}

/// The callee for a `lower_builtin_call` name. The byte-buffer, format-args,
/// and byte-access runtime moved into LIR, so those calls target ordinary
/// `.lilac.<name>` functions; only the remaining leaf (`@memcpy`) is a builtin.
fn runtime_callee(name: &str) -> LirCallee {
    match name {
        "@byte_buffer_from" | "@byte_buffer_ptr" | "@byte_buffer_len" | "@format_args_new"
        | "@format_args_push" | "@format_args_count" | "@format_args_piece" | "@load_u8"
        | "@store_u8" => LirCallee::Func(format!(".lilac.{}", name.trim_start_matches('@'))),
        _ => LirCallee::Builtin(name.to_owned()),
    }
}

impl Lower<'_> {
    /// The emitted LIR symbol for an item. Hooks (`@` in the name, top-level or
    /// object) and `extern` functions keep their bare local name — hooks are
    /// unit-global and externs keep their unmangled C name (spec §7.1/§7.2).
    /// Everything else is its namespace path (omitted at the root) plus `.`
    /// and its existing local name; that local name already carries any
    /// object-member and generic-instance suffix.
    fn symbol(&self, def: DefId, local: &str, is_extern: bool) -> String {
        if is_extern || local.contains('@') {
            return local.to_owned();
        }
        match self.project.namespace_of(def) {
            Some(ns) => {
                let path = self.project.namespace_path(ns);
                if path.is_empty() {
                    local.to_owned()
                } else {
                    format!("{path}.{local}")
                }
            }
            None => local.to_owned(),
        }
    }

    fn expand_const(&mut self, def: DefId, inlining: &mut HashSet<DefId>) -> HirExpr {
        let Some(value) = self.const_src.get(&def).cloned() else {
            lilac_error::bug!(Span::dummy(), "unknown constant in expansion");
        };
        if !inlining.insert(def) {
            self.diags
                .emit(Diagnostic::error("E022", "cyclic constant").label(
                    value.span,
                    true,
                    "constants cannot refer to themselves".to_owned(),
                ));
            self.failed = true;
            return HirExpr {
                ty: TypeRef {
                    id: TypeId::ERROR,
                    is_ref: false,
                },
                span: value.span,
                kind: HirExprKind::Poison,
            };
        }
        let expanded = self.expand_expr(&value, inlining);
        inlining.remove(&def);
        expanded
    }

    /// Expands constant references inside one constant value.
    fn expand_expr(&mut self, expr: &HirExpr, inlining: &mut HashSet<DefId>) -> HirExpr {
        match &expr.kind {
            HirExprKind::Ident { def, .. } => {
                if !self.const_src.contains_key(def) {
                    return expr.clone();
                }
                self.expand_const(*def, inlining)
            }
            HirExprKind::Call { callee, args } => HirExpr {
                ty: expr.ty,
                span: expr.span,
                kind: HirExprKind::Call {
                    callee: callee.clone(),
                    args: args
                        .iter()
                        .map(|arg| self.expand_expr(arg, inlining))
                        .collect(),
                },
            },
            HirExprKind::EqEq { lhs, rhs } => HirExpr {
                ty: expr.ty,
                span: expr.span,
                kind: HirExprKind::EqEq {
                    lhs: Box::new(self.expand_expr(lhs, inlining)),
                    rhs: Box::new(self.expand_expr(rhs, inlining)),
                },
            },
            HirExprKind::Member { obj, field } => HirExpr {
                ty: expr.ty,
                span: expr.span,
                kind: HirExprKind::Member {
                    obj: Box::new(self.expand_expr(obj, inlining)),
                    field: field.clone(),
                },
            },
            HirExprKind::RefOf { value } => HirExpr {
                ty: expr.ty,
                span: expr.span,
                kind: HirExprKind::RefOf {
                    value: Box::new(self.expand_expr(value, inlining)),
                },
            },
            HirExprKind::StructLit { fields } => HirExpr {
                ty: expr.ty,
                span: expr.span,
                kind: HirExprKind::StructLit {
                    fields: fields
                        .iter()
                        .map(|field| lilac_hir::HirField {
                            name: field.name.clone(),
                            value: self.expand_expr(&field.value, inlining),
                            span: field.span,
                        })
                        .collect(),
                },
            },
            HirExprKind::Variant {
                def,
                variant,
                payload,
            } => HirExpr {
                ty: expr.ty,
                span: expr.span,
                kind: HirExprKind::Variant {
                    def: *def,
                    variant: variant.clone(),
                    payload: payload
                        .as_ref()
                        .map(|value| Box::new(self.expand_expr(value, inlining))),
                },
            },
            _ => expr.clone(),
        }
    }

    /// Lowers one function (parameters and return slots become places).
    fn lower_func(&mut self, func: &HirFunc) {
        let symbol = self.symbol(func.def, &func.name, func.is_extern);
        let mut fs = FuncState::new();
        if let Some((parent, _)) = func.name.split_once('.') {
            fs.is_member = true;
            fs.member_parent = Some(parent.to_owned());
        }
        for param in &func.params {
            let place = if param.name.is_empty() {
                fs.args += 1;
                format!("arg.{}", fs.args)
            } else {
                param.name.clone()
            };
            let ty = lir_ty(self.types, param.ty);
            fs.ins.push((place.clone(), ty.clone()));
            fs.ptypes.insert(place.clone(), ty);
            fs.locals.insert(param.name.clone(), (place, param.ty));
        }
        for slot in &func.returns {
            let place = if slot.name.is_empty() {
                fs.args += 1;
                format!("out.{}", fs.args)
            } else {
                slot.name.clone()
            };
            let ty = lir_ty(self.types, slot.ty);
            fs.outs.push((place.clone(), ty.clone()));
            fs.ptypes.insert(place.clone(), ty);
            fs.locals.insert(slot.name.clone(), (place, slot.ty));
        }
        if func.is_extern {
            self.module.funcs.push(LirFunc {
                name: symbol,
                ins: fs.ins,
                outs: fs.outs,
                is_extern: true,
                blocks: Vec::new(),
            });
            return;
        }
        self.lower_block(&mut fs, &func.body);
        if fs.open {
            self.emit_drops(&mut fs);
            end(
                &mut fs,
                LirTerm::Return {
                    span: Span::dummy(),
                },
            );
        }
        debug_assert!(!fs.blocks.is_empty(), "lowered function has no blocks");
        if !fs.allocas.is_empty() {
            let mut allocas = std::mem::take(&mut fs.allocas);
            allocas.append(&mut fs.blocks[0].insts);
            fs.blocks[0].insts = allocas;
        }
        self.module.funcs.push(LirFunc {
            name: symbol,
            ins: fs.ins,
            outs: fs.outs,
            is_extern: false,
            blocks: fs.blocks,
        });
    }

    /// Declares a place: binds the name, allocates zeroed storage, tracks
    /// its type and (for `let`s) its drop candidacy.
    fn declare(&mut self, fs: &mut FuncState, base: &str, ty: TypeRef, span: Span) -> String {
        let place = bind_place(fs, base, ty);
        let lir = lir_ty(self.types, ty);
        emit_alloca(fs, lir.clone(), place.clone(), span);
        fs.owned.push((place.clone(), ty));
        fs.ptypes.insert(place.clone(), lir);
        place
    }

    /// Emits `drop` + `@on_drop` for every owned place of a dropping type.
    fn emit_drops(&mut self, fs: &mut FuncState) {
        let mut drops = Vec::new();
        for (place, ty) in &fs.owned {
            // Drop keys follow the layout key: instances use the arity-tagged
            // mangled name (`GenBox.1..bit32`); concrete types use the display
            // name (design §3.4).
            let key = match self.types.kind(ty.id) {
                lilac_ty::TypeKind::Instance { .. } => self.types.mangle(ty.id),
                _ => self.types.display(ty.id),
            };
            if let Some(hook) = self.drops.get(&key).cloned() {
                drops.push((place.clone(), hook));
            }
        }
        for (place, hook) in drops {
            // The hook runs on live memory, so the destructor call comes first;
            // the `drop` marker then records that the place is destroyed.
            emit(
                fs,
                LirInst::Call {
                    func: LirCallee::Func(hook),
                    ins: vec![place.clone()],
                    outs: Vec::new(),
                    span: Span::dummy(),
                },
            );
            emit(
                fs,
                LirInst::Drop {
                    place,
                    span: Span::dummy(),
                },
            );
        }
    }

    /// Lowers a statement list into the current block.
    fn lower_block(&mut self, fs: &mut FuncState, block: &HirBlock) {
        for stmt in &block.stmts {
            self.lower_stmt(fs, stmt);
        }
    }

    /// Lowers an effect statement. A checked effect statement is a call
    /// returning nothing, so it has no output places (its HIR type is `ERROR`
    /// by design); intrinsics materialize through `eval` as before.
    fn lower_expr_stmt(&mut self, fs: &mut FuncState, expr: &HirExpr) {
        match &expr.kind {
            HirExprKind::Call { callee, args }
                if !matches!(callee, HirCallee::Intrinsic { .. }) =>
            {
                let _ = self.emit_call(fs, callee, args, &[], expr.span);
            }
            _ => {
                let _ = self.eval(fs, expr);
            }
        }
    }

    /// Lowers one statement (control flow splits blocks; terminators drop
    /// dead tails via the open flag).
    fn lower_stmt(&mut self, fs: &mut FuncState, stmt: &HirStmt) {
        match stmt {
            HirStmt::Let {
                name,
                ty,
                value,
                span,
            } => {
                let place = self.declare(fs, name, *ty, *span);
                if let Ok(operand) = self.eval(fs, value) {
                    self.store_operand(fs, &place, *ty, operand, value.ty, *span);
                }
            }
            HirStmt::MultiLet {
                bindings,
                value,
                span,
            } => {
                let HirExprKind::Call { callee, args } = &value.kind else {
                    lilac_error::bug!(*span, "multi-`let` value is not a call");
                };
                let mut tmps = Vec::with_capacity(bindings.len());
                for binding in bindings {
                    tmps.push(self.fresh_tmp(fs, binding.ty, *span));
                }
                let _ = self.emit_call(fs, callee, args, &tmps, *span);
                for (binding, tmp) in bindings.iter().zip(tmps.iter()) {
                    let place = self.declare(fs, &binding.name, binding.ty, binding.span);
                    emit(
                        fs,
                        LirInst::CopyPlace {
                            src: LirOperand::Place(tmp.clone()),
                            dst: place,
                            span: *span,
                        },
                    );
                }
            }
            HirStmt::Assign {
                target,
                value,
                span,
            } => {
                if let Ok(operand) = self.eval(fs, value) {
                    self.lower_assign(fs, target, operand, value.ty, *span);
                }
            }
            HirStmt::ExprStmt(expr) => self.lower_expr_stmt(fs, expr),
            HirStmt::Return { span } => {
                self.emit_drops(fs);
                end(fs, LirTerm::Return { span: *span });
                fs.open = false;
            }
            HirStmt::Break { span } => {
                let Some(exit) = fs.breaks.last().cloned() else {
                    lilac_error::bug!(*span, "break with no enclosing loop");
                };
                end(
                    fs,
                    LirTerm::Branch {
                        target: exit,
                        span: *span,
                    },
                );
                fs.open = false;
            }
            HirStmt::If {
                cond,
                then,
                els,
                out: _,
                span,
            } => {
                self.lower_if(fs, cond, then, els, *span);
            }
            HirStmt::Match {
                scrutinee,
                arms,
                els,
                span,
                ..
            } => {
                self.lower_match(fs, scrutinee, arms, els, *span);
            }
            HirStmt::Loop { body, span } => {
                self.lower_loop(fs, body, *span);
            }
        }
    }

    /// Lowers `if` to a branch-join over labels.
    fn lower_if(
        &mut self,
        fs: &mut FuncState,
        cond: &HirExpr,
        then: &HirBlock,
        els: &HirBlock,
        span: Span,
    ) {
        let then_label = fresh_label(fs);
        let els_label = fresh_label(fs);
        let join_label = fresh_label(fs);
        if let Ok(cond_reg) = self.eval_reg(fs, cond) {
            end(
                fs,
                LirTerm::BranchIf {
                    cond: cond_reg,
                    then: then_label.clone(),
                    els: Some(els_label.clone()),
                    span,
                },
            );
        }
        at_label(fs, then_label);
        self.lower_block(fs, then);
        end(
            fs,
            LirTerm::Branch {
                target: join_label.clone(),
                span,
            },
        );
        at_label(fs, els_label);
        self.lower_block(fs, els);
        end(
            fs,
            LirTerm::Branch {
                target: join_label.clone(),
                span,
            },
        );
        at_label(fs, join_label);
    }

    /// Lowers `loop` to a labeled back-edge with a break stack.
    fn lower_loop(&mut self, fs: &mut FuncState, body: &HirBlock, span: Span) {
        let loop_label = fresh_label(fs);
        let exit_label = fresh_label(fs);
        end(
            fs,
            LirTerm::Branch {
                target: loop_label.clone(),
                span,
            },
        );
        at_label(fs, loop_label.clone());
        fs.breaks.push(exit_label.clone());
        self.lower_block(fs, body);
        end(
            fs,
            LirTerm::Branch {
                target: loop_label,
                span,
            },
        );
        fs.breaks.pop();
        at_label(fs, exit_label);
    }

    /// Allocates a fresh temp place (`%tmp.N`) of a known type.
    fn fresh_tmp(&mut self, fs: &mut FuncState, ty: TypeRef, span: Span) -> String {
        fs.tmps += 1;
        let place = format!("tmp.{}", fs.tmps);
        let lir = lir_ty(self.types, ty);
        emit_alloca(fs, lir.clone(), place.clone(), span);
        fs.ptypes.insert(place.clone(), lir);
        place
    }

    /// Writes `operand` (a value of `value_ty`) into `place` (declared as
    /// `dst_ty`), adapting between references and values: a `ref` value stored
    /// into a value place is dereferenced first, a value stored into a `ref`
    /// place is written through it, and matching types are copied directly.
    fn store_operand(
        &mut self,
        fs: &mut FuncState,
        place: &str,
        dst_ty: TypeRef,
        operand: LirOperand,
        value_ty: TypeRef,
        span: Span,
    ) {
        if dst_ty.is_ref && !value_ty.is_ref {
            emit(
                fs,
                LirInst::Store {
                    src: operand,
                    dst: place.to_owned(),
                    span,
                },
            );
        } else if !dst_ty.is_ref && value_ty.is_ref {
            let source = match operand {
                LirOperand::Place(src_place) => src_place,
                LirOperand::Reg(reg) => {
                    let tmp = self.fresh_tmp(fs, value_ty, span);
                    emit(
                        fs,
                        LirInst::CopyPlace {
                            src: LirOperand::Reg(reg),
                            dst: tmp.clone(),
                            span,
                        },
                    );
                    tmp
                }
            };
            let reg = fresh_reg(fs);
            emit(
                fs,
                LirInst::Load {
                    src: source,
                    dst: reg.clone(),
                    ty: lir_ty(self.types, dst_ty),
                    span,
                },
            );
            emit(
                fs,
                LirInst::CopyPlace {
                    src: LirOperand::Reg(reg),
                    dst: place.to_owned(),
                    span,
                },
            );
        } else {
            emit(
                fs,
                LirInst::CopyPlace {
                    src: operand,
                    dst: place.to_owned(),
                    span,
                },
            );
        }
    }

    /// Assigns a value operand to a target (rebinding, store-through,
    /// field store, or temp copy as the target shape demands).
    fn lower_assign(
        &mut self,
        fs: &mut FuncState,
        target: &HirExpr,
        operand: LirOperand,
        value_ty: TypeRef,
        span: Span,
    ) {
        match &target.kind {
            HirExprKind::Ident { name, .. } => {
                let Some((place, ty)) = fs.locals.get(name).cloned() else {
                    lilac_error::bug!(span, "assign to unbound local");
                };
                self.store_operand(fs, &place, ty, operand, value_ty, span);
            }
            HirExprKind::Member { obj, field } => {
                let Ok(obj_operand) = self.eval(fs, obj) else {
                    return;
                };
                let LirOperand::Place(obj_place) = obj_operand else {
                    self.diags.emit(
                        lilac_error::Diagnostic::error(
                            "E030",
                            "nested member stores lower in a later milestone",
                        )
                        .label(
                            span,
                            true,
                            "store through a value, not a place".to_owned(),
                        ),
                    );
                    self.failed = true;
                    return;
                };
                let Some(owner_ty) = fs.ptypes.get(&obj_place).cloned() else {
                    lilac_error::bug!(span, "member store on untyped place");
                };
                let Some(layout) = self.module.layouts.get(layout_key(&owner_ty)) else {
                    lilac_error::bug!(span, "member store with unknown layout");
                };
                let Some(idx) = layout.iter().position(|(name, _)| name == field) else {
                    lilac_error::bug!(span, "member store of unknown field");
                };
                emit(
                    fs,
                    LirInst::StoreField {
                        value: operand,
                        obj: obj_place,
                        name: field.clone(),
                        idx: field_idx(idx, span),
                        span,
                    },
                );
            }
            HirExprKind::Temp(temp) => {
                let place = self.temp_place(fs, *temp, target.ty, span);
                self.store_operand(fs, &place, target.ty, operand, value_ty, span);
            }
            _ => {
                lilac_error::bug!(span, "assignment to non-place expression");
            }
        }
    }

    /// Returns a temp's place, allocating it (zeroed) on first assignment.
    fn temp_place(&mut self, fs: &mut FuncState, temp: TempId, ty: TypeRef, span: Span) -> String {
        if let Some(place) = fs.temps.get(&temp) {
            return place.clone();
        }
        fs.tmps += 1;
        let place = format!("t.{}", fs.tmps);
        let lir = lir_ty(self.types, ty);
        emit_alloca(fs, lir.clone(), place.clone(), span);
        fs.ptypes.insert(place.clone(), lir);
        fs.temps.insert(temp, place.clone());
        place
    }

    /// Lowers `match` to tag-test chains over a join label.
    fn lower_match(
        &mut self,
        fs: &mut FuncState,
        scrutinee: &HirExpr,
        arms: &[lilac_hir::HirArm],
        els: &HirBlock,
        span: Span,
    ) {
        let Ok(scrut_operand) = self.eval(fs, scrutinee) else {
            return;
        };
        let scrut_place = match scrut_operand {
            LirOperand::Place(place) => place,
            LirOperand::Reg(reg) => {
                let tmp = self.fresh_tmp(fs, scrutinee.ty, scrutinee.span);
                emit(
                    fs,
                    LirInst::CopyPlace {
                        src: LirOperand::Reg(reg),
                        dst: tmp.clone(),
                        span: scrutinee.span,
                    },
                );
                tmp
            }
        };
        let owner = fs.ptypes.get(&scrut_place).cloned().unwrap_or_default();
        let scrut_lir_ty = lir_ty(self.types, scrutinee.ty);
        let join_label = fresh_label(fs);
        let els_label = fresh_label(fs);
        let mut next = fresh_label(fs);
        for (index, arm) in arms.iter().enumerate() {
            at_label(fs, next);
            let arm_label = fresh_label(fs);
            next = fresh_label(fs);
            let following = if index + 1 == arms.len() {
                els_label.clone()
            } else {
                next.clone()
            };
            let test_reg =
                self.emit_arm_test(fs, &scrut_place, &scrut_lir_ty, &owner, &arm.pattern, span);
            end(
                fs,
                LirTerm::BranchIf {
                    cond: test_reg,
                    then: arm_label.clone(),
                    els: Some(following),
                    span,
                },
            );
            at_label(fs, arm_label);
            if let Some(capture) = &arm.capture {
                self.emit_arm_capture(fs, &scrut_place, &owner, &arm.pattern, capture, span);
            }
            self.lower_block(fs, &arm.body);
            end(
                fs,
                LirTerm::Branch {
                    target: join_label.clone(),
                    span,
                },
            );
        }
        at_label(fs, els_label);
        self.lower_block(fs, els);
        end(
            fs,
            LirTerm::Branch {
                target: join_label.clone(),
                span,
            },
        );
        at_label(fs, join_label);
    }

    /// Emits one arm test: scrutinee value against the pattern, returning
    /// the boolean register for `branch.if`. Enum scrutinees test
    /// their discriminator tag field; bit containers test the value.
    fn emit_arm_test(
        &mut self,
        fs: &mut FuncState,
        scrut_place: &str,
        scrut_ty: &str,
        owner: &str,
        pattern: &HirPattern,
        span: Span,
    ) -> String {
        let test_reg = fresh_reg(fs);
        let const_reg = fresh_reg(fs);
        let owner_display = owner.strip_prefix('#').unwrap_or(owner);
        if self.tags.contains_key(owner_display) {
            let idx = match pattern {
                HirPattern::Tag { name } => self.variant_index(owner_display, name, span),
                HirPattern::Const { .. } => {
                    lilac_error::bug!(span, "byte pattern on enum scrutinee");
                }
            };
            emit(
                fs,
                LirInst::Field {
                    obj: scrut_place.to_owned(),
                    name: "tag".to_owned(),
                    idx: 0,
                    dst: test_reg.clone(),
                    ty: "#bit8".to_owned(),
                    span,
                },
            );
            emit(
                fs,
                LirInst::Const {
                    dst: const_reg.clone(),
                    lit: format!("0x{idx:x}"),
                    ty: "#bit8".to_owned(),
                    span,
                },
            );
        } else {
            let bytes = match pattern {
                HirPattern::Const { bytes } => bytes.clone(),
                HirPattern::Tag { .. } => {
                    lilac_error::bug!(span, "tag pattern on bit scrutinee");
                }
            };
            emit(
                fs,
                LirInst::CopyReg {
                    src: LirOperand::Place(scrut_place.to_owned()),
                    dst: test_reg.clone(),
                    ty: scrut_ty.to_owned(),
                    span,
                },
            );
            emit(
                fs,
                LirInst::Const {
                    dst: const_reg.clone(),
                    lit: bytes,
                    ty: scrut_ty.to_owned(),
                    span,
                },
            );
        }
        let dst = fresh_reg(fs);
        emit(
            fs,
            LirInst::Arith {
                op: ArithOp::Eq,
                lhs: test_reg,
                rhs: const_reg,
                dst: dst.clone(),
                ty: "#bit8".to_owned(),
                span,
            },
        );
        dst
    }

    /// Emits one arm capture: payload field load into a fresh place.
    fn emit_arm_capture(
        &mut self,
        fs: &mut FuncState,
        scrut_place: &str,
        owner: &str,
        pattern: &HirPattern,
        capture: &lilac_hir::HirParam,
        span: Span,
    ) {
        let HirPattern::Tag { name: variant } = pattern else {
            lilac_error::bug!(span, "capture on non-tag pattern");
        };
        let place = self.declare(fs, &capture.name, capture.ty, capture.span);
        let payload_idx = self.payload_index(owner, variant, span);
        let field_ty = self
            .module
            .layouts
            .get(owner)
            .and_then(|layout| layout.get(payload_idx as usize))
            .map_or("#bit8".to_owned(), |(_, ty)| ty.clone());
        let value_reg = fresh_reg(fs);
        emit(
            fs,
            LirInst::Field {
                obj: scrut_place.to_owned(),
                name: variant.clone(),
                idx: payload_idx,
                dst: value_reg.clone(),
                ty: field_ty,
                span: capture.span,
            },
        );
        emit(
            fs,
            LirInst::CopyPlace {
                src: LirOperand::Reg(value_reg),
                dst: place,
                span: capture.span,
            },
        );
    }

    /// Discriminant of a variant: its declaration-order index.
    fn variant_index(&mut self, owner: &str, variant: &str, span: Span) -> u32 {
        let Some(names) = self.tags.get(owner) else {
            lilac_error::bug!(span, "match on type without variants");
        };
        let Some(idx) = names.iter().position(|name| name == variant) else {
            lilac_error::bug!(span, "match on unknown variant");
        };
        field_idx(idx, span)
    }

    /// Payload field index: position of the variant's entry in the layout.
    fn payload_index(&mut self, owner: &str, variant: &str, span: Span) -> u32 {
        let Some(layout) = self.module.layouts.get(layout_key(owner)) else {
            lilac_error::bug!(span, "payload of type without layout");
        };
        let Some(idx) = layout.iter().position(|(name, _)| name == variant) else {
            lilac_error::bug!(span, "payload of unknown variant");
        };
        field_idx(idx, span)
    }

    /// Evaluates a name: locals and join temps read places, constants
    /// inline, functions materialize addresses. Member functions without a
    /// declared `instance` parameter read a zeroed implicit receiver.
    fn lower_ident(
        &mut self,
        fs: &mut FuncState,
        name: &str,
        def: DefId,
        span: Span,
    ) -> Result<LirOperand, ErrorGuaranteed> {
        if let Some((place, _)) = fs.locals.get(name) {
            return Ok(LirOperand::Place(place.clone()));
        }
        if name == "instance" && fs.is_member {
            // Static-style members read a zeroed implicit receiver.
            let parent = fs.member_parent.clone().unwrap_or_default();
            let place = "instance".to_owned();
            let ty = format!("ref #{parent}");
            emit_alloca(fs, ty.clone(), place.clone(), span);
            fs.ptypes.insert(place.clone(), ty);
            fs.locals.insert(
                name.to_owned(),
                (
                    place.clone(),
                    TypeRef {
                        id: TypeId::ERROR,
                        is_ref: true,
                    },
                ),
            );
            return Ok(LirOperand::Place(place));
        }
        if let Some((place, _)) = fs.locals.get(name) {
            return Ok(LirOperand::Place(place.clone()));
        }
        if let Some(value) = self.consts.get(&def).cloned() {
            return self.eval(fs, &value);
        }
        if self.func_names.contains_key(&def) {
            let reg = fresh_reg(fs);
            let Some(name) = self.func_names.get(&def).cloned() else {
                lilac_error::bug!(span, "function without a name");
            };
            emit(
                fs,
                LirInst::AddrFunc {
                    func: name,
                    dst: reg.clone(),
                    ty: "#bit64".to_owned(),
                    span,
                },
            );
            return Ok(LirOperand::Reg(reg));
        }
        lilac_error::bug!(span, "unbound name in lowering");
    }

    /// Evaluates an expression to an operand (register or place).
    fn eval(&mut self, fs: &mut FuncState, expr: &HirExpr) -> Result<LirOperand, ErrorGuaranteed> {
        match &expr.kind {
            HirExprKind::Ident { name, def } => self.lower_ident(fs, name, *def, expr.span),
            HirExprKind::Tag { name } => Ok(self.lower_tag(fs, expr.ty, name, expr.span)),
            HirExprKind::Number { bytes } => Ok(self.lower_number(fs, expr.ty, bytes, expr.span)),
            HirExprKind::Str { value } => {
                let reg = fresh_reg(fs);
                let name = self.intern_data(LirDataKind::Str(value.clone()), value);
                emit(
                    fs,
                    LirInst::Const {
                        dst: reg.clone(),
                        lit: format!("!{name}"),
                        ty: "#byte_buffer".to_owned(),
                        span: expr.span,
                    },
                );
                Ok(LirOperand::Reg(reg))
            }
            HirExprKind::Char { value } => {
                let reg = fresh_reg(fs);
                emit(
                    fs,
                    LirInst::Const {
                        dst: reg.clone(),
                        lit: format!("0x{:x}", *value as u32),
                        ty: lir_ty(self.types, expr.ty),
                        span: expr.span,
                    },
                );
                Ok(LirOperand::Reg(reg))
            }
            HirExprKind::Call { callee, args } => {
                self.lower_call_value(fs, callee, args, expr.ty, expr.span)
            }
            HirExprKind::Member { obj, field } => {
                let reg = fresh_reg(fs);
                self.lower_member(fs, obj, field, expr.span, reg.clone())?;
                Ok(LirOperand::Reg(reg))
            }
            HirExprKind::RefOf { value } => {
                let reg = fresh_reg(fs);
                self.lower_ref_of(fs, value, expr.span, reg.clone())?;
                Ok(LirOperand::Reg(reg))
            }
            HirExprKind::StructLit { fields } => {
                let tmp = self.fresh_tmp(fs, expr.ty, expr.span);
                self.lower_struct_lit(fs, fields, &tmp)?;
                Ok(LirOperand::Place(tmp))
            }
            HirExprKind::Variant {
                variant, payload, ..
            } => {
                let tmp = self.fresh_tmp(fs, expr.ty, expr.span);
                self.lower_variant(fs, variant, payload.as_deref(), &tmp, expr.span)?;
                Ok(LirOperand::Place(tmp))
            }
            HirExprKind::Temp(temp) => {
                let Some(place) = fs.temps.get(temp).cloned() else {
                    lilac_error::bug!(expr.span, "use of unassigned join temp");
                };
                Ok(LirOperand::Place(place))
            }
            HirExprKind::EqEq { lhs, rhs } => {
                let left = self.eval_reg(fs, lhs)?;
                let right = self.eval_reg(fs, rhs)?;
                let reg = fresh_reg(fs);
                emit(
                    fs,
                    LirInst::Arith {
                        op: ArithOp::Eq,
                        lhs: left,
                        rhs: right,
                        dst: reg.clone(),
                        ty: lir_ty(self.types, expr.ty),
                        span: expr.span,
                    },
                );
                Ok(LirOperand::Reg(reg))
            }
            HirExprKind::CompileError {
                msg,
                msg_span,
                call_span,
            } => Ok(self.lower_compile_error(fs, msg, *msg_span, *call_span, expr.span)),
            HirExprKind::Poison => {
                lilac_error::bug!(expr.span, "lowering poisoned HIR");
            }
        }
    }

    /// Builds a bare tag into an enum struct (discriminant only).
    fn lower_tag(&mut self, fs: &mut FuncState, ty: TypeRef, name: &str, span: Span) -> LirOperand {
        let owner_ty = lir_ty(self.types, ty);
        let tmp = self.fresh_tmp(fs, ty, span);
        let owner = layout_key(&owner_ty);
        let display = owner.strip_prefix('#').unwrap_or(owner);
        let idx = self.variant_index(display, name, span);
        let tag_reg = fresh_reg(fs);
        emit(
            fs,
            LirInst::Const {
                dst: tag_reg.clone(),
                lit: format!("0x{idx:x}"),
                ty: "#bit8".to_owned(),
                span,
            },
        );
        emit(
            fs,
            LirInst::StoreField {
                value: LirOperand::Reg(tag_reg),
                obj: tmp.clone(),
                name: "tag".to_owned(),
                idx: 0,
                span,
            },
        );
        LirOperand::Place(tmp)
    }

    /// Materializes a number literal into a register.
    fn lower_number(
        &mut self,
        fs: &mut FuncState,
        ty: TypeRef,
        bytes: &str,
        span: Span,
    ) -> LirOperand {
        let lir = lir_ty(self.types, ty);
        if lir == "#byte_buffer" {
            let reg = fresh_reg(fs);
            let name = self.intern_data(LirDataKind::NumLit(bytes.to_owned()), bytes);
            emit(
                fs,
                LirInst::Const {
                    dst: reg.clone(),
                    lit: format!("!{name}"),
                    ty: lir,
                    span,
                },
            );
            LirOperand::Reg(reg)
        } else {
            let reg = fresh_reg(fs);
            emit(
                fs,
                LirInst::Const {
                    dst: reg.clone(),
                    lit: bytes.to_owned(),
                    ty: lir,
                    span,
                },
            );
            LirOperand::Reg(reg)
        }
    }

    /// Interns a data const, deduping by `(kind, payload)` and assigning
    /// `c0`, `c1`, … in first-use order.
    fn intern_data(&mut self, kind: LirDataKind, payload: &str) -> String {
        let same = |existing: &LirData| {
            std::mem::discriminant(&existing.kind) == std::mem::discriminant(&kind)
                && match &existing.kind {
                    LirDataKind::Str(value) | LirDataKind::NumLit(value) => value == payload,
                }
        };
        if let Some(existing) = self.module.data.iter().find(|data| same(data)) {
            return existing.name.clone();
        }
        let name = format!("c{}", self.module.data.len());
        self.module.data.push(LirData {
            name: name.clone(),
            kind,
            span: Span::dummy(),
        });
        name
    }

    /// Lowers a call in value position (intrinsics yield registers,
    /// other calls write one fresh out place).
    fn lower_call_value(
        &mut self,
        fs: &mut FuncState,
        callee: &HirCallee,
        args: &[HirExpr],
        result_ty: TypeRef,
        span: Span,
    ) -> Result<LirOperand, ErrorGuaranteed> {
        if let HirCallee::Intrinsic { name } = callee {
            let reg = self.lower_intrinsic(fs, name, args, result_ty, span)?;
            Ok(LirOperand::Reg(reg))
        } else {
            let place = self.fresh_tmp(fs, result_ty, span);
            self.emit_call(fs, callee, args, std::slice::from_ref(&place), span)?;
            Ok(LirOperand::Place(place))
        }
    }

    /// Materializes an expression into a place (copying regs as needed).
    fn eval_place(
        &mut self,
        fs: &mut FuncState,
        expr: &HirExpr,
    ) -> Result<String, ErrorGuaranteed> {
        match self.eval(fs, expr)? {
            LirOperand::Place(place) => Ok(place),
            LirOperand::Reg(reg) => {
                let tmp = self.fresh_tmp(fs, expr.ty, expr.span);
                emit(
                    fs,
                    LirInst::CopyPlace {
                        src: LirOperand::Reg(reg),
                        dst: tmp.clone(),
                        span: expr.span,
                    },
                );
                Ok(tmp)
            }
        }
    }

    /// Materializes an expression into a register (copying places as needed).
    fn eval_reg(&mut self, fs: &mut FuncState, expr: &HirExpr) -> Result<String, ErrorGuaranteed> {
        match self.eval(fs, expr)? {
            LirOperand::Reg(reg) => Ok(reg),
            LirOperand::Place(place) => {
                let reg = fresh_reg(fs);
                emit(
                    fs,
                    LirInst::CopyReg {
                        src: LirOperand::Place(place),
                        dst: reg.clone(),
                        ty: lir_ty(self.types, expr.ty),
                        span: expr.span,
                    },
                );
                Ok(reg)
            }
        }
    }

    /// Emits a call into preallocated out places.
    fn emit_call(
        &mut self,
        fs: &mut FuncState,
        callee: &HirCallee,
        args: &[HirExpr],
        outs: &[String],
        span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        let mut ins = Vec::with_capacity(args.len());
        for arg in args {
            ins.push(self.eval_place(fs, arg)?);
        }
        let func = match callee {
            HirCallee::Item(def) => {
                let Some(name) = self.func_names.get(def).cloned() else {
                    lilac_error::bug!(span, "call to missing function");
                };
                LirCallee::Func(name)
            }
            HirCallee::Local { name } => {
                let Some((place, _)) = fs.locals.get(name).cloned() else {
                    lilac_error::bug!(span, "call through unbound local");
                };
                let reg = fresh_reg(fs);
                emit(
                    fs,
                    LirInst::CopyReg {
                        src: LirOperand::Place(place),
                        dst: reg.clone(),
                        ty: "#bit64".to_owned(),
                        span,
                    },
                );
                LirCallee::Reg(reg)
            }
            HirCallee::Hook { name, .. } => LirCallee::Func(name.clone()),
            HirCallee::Intrinsic { .. } => {
                lilac_error::bug!(span, "intrinsic in general call path");
            }
        };
        emit(
            fs,
            LirInst::Call {
                func,
                ins,
                outs: outs.to_owned(),
                span,
            },
        );
        Ok(())
    }

    /// Lowers an intrinsic to arithmetic, conversion, or `E030` (M3 strings
    /// and exotic intrinsics stay deferred).
    fn lower_intrinsic(
        &mut self,
        fs: &mut FuncState,
        name: &str,
        args: &[HirExpr],
        result_ty: TypeRef,
        span: Span,
    ) -> Result<String, ErrorGuaranteed> {
        if let Some(op) = arith_op_for(name) {
            if args.len() != 2 {
                lilac_error::bug!(span, "arithmetic intrinsic without two operands");
            }
            let lhs = self.eval_reg(fs, &args[0])?;
            let rhs = self.eval_reg(fs, &args[1])?;
            let dst = fresh_reg(fs);
            emit(
                fs,
                LirInst::Arith {
                    op,
                    lhs,
                    rhs,
                    dst: dst.clone(),
                    ty: lir_ty(self.types, result_ty),
                    span,
                },
            );
            return Ok(dst);
        }
        match name {
            "@i_negate" | "@f_negate" => {
                if args.len() != 1 {
                    lilac_error::bug!(span, "negate without one operand");
                }
                let value = self.eval_reg(fs, &args[0])?;
                let ty = lir_ty(self.types, result_ty);
                let zero = fresh_reg(fs);
                emit(
                    fs,
                    LirInst::Const {
                        dst: zero.clone(),
                        lit: "0x0".to_owned(),
                        ty: ty.clone(),
                        span,
                    },
                );
                let dst = fresh_reg(fs);
                let op = if name == "@i_negate" {
                    ArithOp::ISub
                } else {
                    ArithOp::FSub
                };
                emit(
                    fs,
                    LirInst::Arith {
                        op,
                        lhs: zero,
                        rhs: value,
                        dst: dst.clone(),
                        ty,
                        span,
                    },
                );
                Ok(dst)
            }
            "@truncate" => self.lower_convert(fs, args, result_ty, span, ConvertOp::Trunc),
            "@extend_zero" => self.lower_convert(fs, args, result_ty, span, ConvertOp::ExtendZero),
            "@as_int" | "@as_float" | "@size_of" => {
                self.lower_fold_intrinsic(fs, name, args, result_ty, span)
            }
            "@byte_buffer_from" | "@byte_buffer_ptr" | "@byte_buffer_len" | "@format_args_new"
            | "@format_args_count" | "@format_args_piece" | "@load_u8" => {
                self.lower_builtin_call(fs, name, args, result_ty, span, true)
            }
            "@store_u8" | "@memcpy" | "@format_args_push" => {
                self.lower_builtin_call(fs, name, args, result_ty, span, false)
            }
            _ => {
                self.diags.emit(
                    lilac_error::Diagnostic::error("E030", format!("`{name}` lowers in M3")).label(
                        span,
                        true,
                        "lands in a later milestone".to_owned(),
                    ),
                );
                self.failed = true;
                Err(ErrorGuaranteed::new())
            }
        }
    }

    /// Emits a pre-fold marker for `@as_int`/`@as_float`/`@size_of` with
    /// the single argument evaluated to a register.
    fn lower_fold_intrinsic(
        &mut self,
        fs: &mut FuncState,
        name: &str,
        args: &[HirExpr],
        result_ty: TypeRef,
        span: Span,
    ) -> Result<String, ErrorGuaranteed> {
        if args.len() != 1 {
            lilac_error::bug!(span, "fold intrinsic without one operand");
        }
        let arg = self.eval_reg(fs, &args[0])?;
        let dst = fresh_reg(fs);
        emit(
            fs,
            LirInst::FoldIntrinsic {
                name: name.to_owned(),
                arg: LirOperand::Reg(arg),
                dst: dst.clone(),
                ty: lir_ty(self.types, result_ty),
                span,
            },
        );
        Ok(dst)
    }

    /// Emits a call to a compiler builtin. Single-result builtins copy the
    /// temp out place into a fresh register; void builtins return a fresh
    /// (unused) register for statement position.
    fn lower_builtin_call(
        &mut self,
        fs: &mut FuncState,
        name: &str,
        args: &[HirExpr],
        result_ty: TypeRef,
        span: Span,
        has_result: bool,
    ) -> Result<String, ErrorGuaranteed> {
        let mut ins = Vec::with_capacity(args.len());
        for arg in args {
            ins.push(self.eval_place(fs, arg)?);
        }
        let callee = runtime_callee(name);
        if has_result {
            let temp = self.fresh_tmp(fs, result_ty, span);
            emit(
                fs,
                LirInst::Call {
                    func: callee.clone(),
                    ins,
                    outs: vec![temp.clone()],
                    span,
                },
            );
            let reg = fresh_reg(fs);
            emit(
                fs,
                LirInst::CopyReg {
                    src: LirOperand::Place(temp),
                    dst: reg.clone(),
                    ty: lir_ty(self.types, result_ty),
                    span,
                },
            );
            Ok(reg)
        } else {
            emit(
                fs,
                LirInst::Call {
                    func: callee,
                    ins,
                    outs: Vec::new(),
                    span,
                },
            );
            Ok(fresh_reg(fs))
        }
    }

    /// Lowers `@truncate`/`@extend_zero` to a typed conversion.
    fn lower_convert(
        &mut self,
        fs: &mut FuncState,
        args: &[HirExpr],
        result_ty: TypeRef,
        span: Span,
        op: ConvertOp,
    ) -> Result<String, ErrorGuaranteed> {
        if args.len() != 1 {
            lilac_error::bug!(span, "conversion without one operand");
        }
        let src = self.eval_reg(fs, &args[0])?;
        let dst = fresh_reg(fs);
        emit(
            fs,
            LirInst::Convert {
                op,
                src,
                dst: dst.clone(),
                ty: lir_ty(self.types, result_ty),
                span,
            },
        );
        Ok(dst)
    }

    /// Loads `obj.field` into a register (nested objects materialize first).
    fn lower_member(
        &mut self,
        fs: &mut FuncState,
        obj: &HirExpr,
        field: &str,
        span: Span,
        dst: String,
    ) -> Result<(), ErrorGuaranteed> {
        let obj_place = self.eval_place(fs, obj)?;
        let owner_ty = fs.ptypes.get(&obj_place).cloned().unwrap_or_default();
        let layout = self
            .module
            .layouts
            .get(layout_key(&owner_ty))
            .cloned()
            .unwrap_or_default();
        let Some(idx) = layout.iter().position(|(name, _)| name == field) else {
            lilac_error::bug!(span, "member read of unknown field");
        };
        let Some((_, field_ty)) = layout.get(idx).cloned() else {
            lilac_error::bug!(span, "member index out of bounds");
        };
        emit(
            fs,
            LirInst::Field {
                obj: obj_place,
                name: field.to_owned(),
                idx: field_idx(idx, span),
                dst,
                ty: field_ty,
                span,
            },
        );
        Ok(())
    }

    /// Emits a `@compile_error` marker with a dummy `#bit8` result slot:
    /// live sites never reach codegen (the post-DCE check reports `E031`
    /// first); dead ones prune away, leaving only an unused zeroed place
    /// behind.
    fn lower_compile_error(
        &mut self,
        fs: &mut FuncState,
        msg: &str,
        msg_span: Span,
        call_span: Span,
        span: Span,
    ) -> LirOperand {
        emit(
            fs,
            LirInst::CompileError {
                msg: msg.to_owned(),
                msg_span,
                call_span,
            },
        );
        let bit8 = self.types.builtin("@bit8").unwrap_or(TypeId::ERROR);
        let place = self.fresh_tmp(
            fs,
            TypeRef {
                id: bit8,
                is_ref: false,
            },
            span,
        );
        LirOperand::Place(place)
    }

    /// Takes a reference: plain places directly, nested members through a
    /// materialized temporary (writes through those need M5 address math).
    fn lower_ref_of(
        &mut self,
        fs: &mut FuncState,
        value: &HirExpr,
        span: Span,
        dst: String,
    ) -> Result<(), ErrorGuaranteed> {
        match &value.kind {
            HirExprKind::Ident { .. } | HirExprKind::Temp(_) => {
                let place = self.eval_place(fs, value)?;
                let place_ty = fs.ptypes.get(&place).cloned().unwrap_or_default();
                emit(
                    fs,
                    LirInst::Addr {
                        place,
                        dst,
                        ty: format!("ref {place_ty}"),
                        span,
                    },
                );
                Ok(())
            }
            HirExprKind::Member { obj, field } => {
                let obj_place = self.eval_place(fs, obj)?;
                let owner_ty = fs.ptypes.get(&obj_place).cloned().unwrap_or_default();
                let layout = self
                    .module
                    .layouts
                    .get(&owner_ty)
                    .cloned()
                    .unwrap_or_default();
                let Some(idx) = layout.iter().position(|(name, _)| name == field) else {
                    lilac_error::bug!(span, "member address of unknown field");
                };
                let field_ty = layout
                    .get(idx)
                    .map(|(_, ty)| ty.clone())
                    .unwrap_or_default();
                emit(
                    fs,
                    LirInst::FieldAddr {
                        obj: obj_place,
                        name: field.clone(),
                        idx: field_idx(idx, span),
                        dst,
                        ty: format!("ref {field_ty}"),
                        span,
                    },
                );
                Ok(())
            }
            _ => {
                self.diags.emit(
                    lilac_error::Diagnostic::error(
                        "E030",
                        "nested member addresses lower in a later milestone",
                    )
                    .label(span, true, "lands in a later milestone".to_owned()),
                );
                self.failed = true;
                Err(ErrorGuaranteed::new())
            }
        }
    }

    /// Initializes a struct literal into a place, field by field.
    fn lower_struct_lit(
        &mut self,
        fs: &mut FuncState,
        fields: &[lilac_hir::HirField],
        tmp: &str,
    ) -> Result<(), ErrorGuaranteed> {
        let owner_ty = fs.ptypes.get(tmp).cloned().unwrap_or_default();
        for field in fields {
            let value = self.eval(fs, &field.value)?;
            let layout = self
                .module
                .layouts
                .get(&owner_ty)
                .cloned()
                .unwrap_or_default();
            let Some(idx) = layout.iter().position(|(name, _)| name == &field.name) else {
                lilac_error::bug!(field.span, "struct literal of unknown field");
            };
            emit(
                fs,
                LirInst::StoreField {
                    value,
                    obj: tmp.to_owned(),
                    name: field.name.clone(),
                    idx: field_idx(idx, field.span),
                    span: field.span,
                },
            );
        }
        Ok(())
    }

    /// Constructs an enum value: discriminant tag plus payload.
    fn lower_variant(
        &mut self,
        fs: &mut FuncState,
        variant: &str,
        payload: Option<&HirExpr>,
        tmp: &str,
        span: Span,
    ) -> Result<(), ErrorGuaranteed> {
        let owner = fs.ptypes.get(tmp).cloned().unwrap_or_default();
        let display = owner.strip_prefix('#').unwrap_or(&owner);
        let idx = self.variant_index(display, variant, span);
        let tag_reg = fresh_reg(fs);
        emit(
            fs,
            LirInst::Const {
                dst: tag_reg.clone(),
                lit: format!("0x{idx:x}"),
                ty: "#bit8".to_owned(),
                span,
            },
        );
        emit(
            fs,
            LirInst::StoreField {
                value: LirOperand::Reg(tag_reg),
                obj: tmp.to_owned(),
                name: "tag".to_owned(),
                idx: 0,
                span,
            },
        );
        if let Some(payload) = payload {
            let value = self.eval(fs, payload)?;
            let layout = self
                .module
                .layouts
                .get(layout_key(&owner))
                .cloned()
                .unwrap_or_default();
            let Some(idx) = layout.iter().position(|(name, _)| name == variant) else {
                lilac_error::bug!(span, "variant payload without layout entry");
            };
            emit(
                fs,
                LirInst::StoreField {
                    value,
                    obj: tmp.to_owned(),
                    name: variant.to_owned(),
                    idx: field_idx(idx, span),
                    span: payload.span,
                },
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_error::Diags;
    use lilac_lex::Lexer;
    use lilac_parse::Parser;
    use lilac_span::SourceMap;
    use lilac_ty::Types;

    fn lower_src(src: &str) -> (String, Diags) {
        let mut sm = SourceMap::new();
        let file = sm.add_file("test.lilac", src);
        let (tokens, lex_errors) = Lexer::lex_all(src, file);
        let mut diags = Diags::new();
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, file);
        let program = parser
            .parse_program(&mut diags)
            .expect("test source parses");
        let module = lilac_resolve::collect(&program, &mut diags);
        let project = lilac_resolve::ResolvedProject::single(module, file);
        let mut types = Types::new();
        let sigs = lilac_typeck::collect_sigs(&project, &program, &mut types, &mut diags, true);
        let hir = lilac_typeck::check_bodies(&program, &project, &sigs, &mut types, &mut diags);
        let lir = lower(&hir, &project, &types, &mut diags, "x86_64-unknown-none")
            .expect("test source lowers");
        (lir.to_text(), diags)
    }

    #[test]
    fn lower_if_produces_branches() {
        let (text, diags) = lower_src(
            "main: function () nothing {\n    conditional: @bit32 = 0x0\n    if 0x0 {\n        conditional = 0x1\n    } else {\n        conditional = 0x2\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains("branch.if"), "{text}");
        assert!(text.contains("branch $"), "{text}");
    }

    #[test]
    fn lower_unsigned_intrinsics_map_to_unsigned_ops() {
        let (text, diags) = lower_src(
            "main: function () nothing {\n    a: @bit32 = 0x8\n    b: @bit32 = 0x2\n    q: @bit32 = @i_div_u(a, b)\n    r: @bit32 = @i_mod_u(a, b)\n    if @i_less_than_u(a, b) {\n        q = 0x1\n    }\n    if @i_greater_than_u(a, b) {\n        r = 0x1\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        for op in ["i.div.u", "i.mod.u", "i.cmp.lt.u", "i.cmp.gt.u"] {
            assert!(text.contains(op), "missing `{op}` in:\n{text}");
        }
    }

    #[test]
    fn lower_multiret_arity() {
        let (text, diags) = lower_src(
            "get_multi: function () condition: @bit32, result: @bit32 {\n    condition = 0x1\n    result = 0x2\n}\nmain: function () nothing {\n    condition: @bit32, result: @bit32 = get_multi()\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains("call @get_multi"), "{text}");
        assert!(text.contains("(out "), "{text}");
    }

    #[test]
    fn lower_generic_object_instance() {
        let (text, diags) = lower_src(
            "GenObject[T]: object {\n    value: T\n    get: function (self) result: T {\n        result = self.value\n    }\n}\nmain: function () nothing {\n    variable: GenObject[@bit32] = GenObject {\n        value = 0x0\n    }\n    value: @bit32 = variable.get()\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains("#GenObject.1..bit32"), "{text}");
        assert!(text.contains("GenObject.1..bit32.get"), "{text}");
    }

    #[test]
    fn lower_method_own_generics_distinct() {
        let (text, diags) = lower_src(
            "GenObject[T]: object {\n    value: T\n    get[U]: function (x: U) result: @bit32 {\n        result = 0x2\n    }\n}\n\ngen[T]: function () out: @bit32 {\n    out = T.get[@bit32](0x0)\n}\n\ngen2[T]: function () out: @bit32 {\n    out = T.get[@bit64](0x0)\n}\n\nmain: function () nothing {\n    a: @bit32 = gen[GenObject[@bit32]]()\n    b: @bit32 = gen2[GenObject[@bit32]]()\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains("GenObject.1..bit32.get.1..bit32"), "{text}");
        assert!(text.contains("GenObject.1..bit32.get.1..bit64"), "{text}");
    }

    #[test]
    fn lower_structlit_alloca_zeroed() {
        let (text, diags) = lower_src(
            "Object: object {\n    member: @bit32\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains(": zeroed"), "{text}");
        assert!(text.contains(".member"), "{text}");
    }

    #[test]
    fn lower_drop_when_on_drop() {
        let (text, diags) = lower_src(
            "Object: object {\n    member: @bit32\n    @on_drop: function (from: ref Object) nothing {\n    }\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains("drop %"), "{text}");
        // The destructor runs on live memory, so its call precedes the marker
        // that marks the place destroyed.
        let call = text.find("call @Object.@on_drop").expect("on_drop call");
        let drop = text.find("drop %variable").expect("drop marker");
        assert!(call < drop, "@on_drop must precede drop:\n{text}");
    }

    #[test]
    fn drop_emitted_for_instance() {
        let (text, diags) = lower_src(
            "GenBox[T]: object {\n    value: T\n    @on_drop: function (from: ref GenBox[T]) nothing {\n    }\n}\n\nmain: function () nothing {\n    box: GenBox[@bit32] = GenBox {\n        value = 0x0\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains("drop %"), "{text}");
        assert!(text.contains("@GenBox.1..bit32.@on_drop"), "{text}");
        // The destructor runs on live memory, so its call precedes the marker
        // that marks the place destroyed.
        let call = text
            .find("call @GenBox.1..bit32.@on_drop")
            .expect("on_drop call");
        let drop = text.find("drop %box").expect("drop marker");
        assert!(call < drop, "@on_drop must precede drop:\n{text}");
    }

    #[test]
    fn lower_no_drop_without_hook() {
        let (text, diags) = lower_src(
            "Object: object {\n    member: @bit32\n}\nmain: function () nothing {\n    variable: Object = Object {\n        member = 0x1\n    }\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(!text.contains("drop %"), "{text}");
    }

    #[test]
    fn lower_str_emits_data_and_buffer() {
        let (text, diags) =
            lower_src("main: function () nothing {\n    s: @byte_buffer = \"hi\"\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains("str = \"hi\""), "{text}");
        assert!(text.contains("#byte_buffer"), "{text}");
    }

    #[test]
    fn lower_decimal_emits_numlit_data() {
        let (text, diags) =
            lower_src("main: function () nothing {\n    x: @bit8 = @as_int(5)\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains("numlit = \"5\""), "{text}");
        assert!(text.contains("@as_int"), "{text}");
    }

    #[test]
    fn lower_hex_stays_const() {
        let (text, diags) = lower_src("main: function () nothing {\n    x: @bit8 = 0x1\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains("#bit8 = const 0x1"), "{text}");
        assert!(!text.contains("numlit ="), "{text}");
    }

    #[test]
    fn lower_builtin_calls() {
        // The format-args runtime is ordinary LIR now: call sites name the
        // `.lilac.*` function rather than a value-ABI builtin.
        let (text, diags) =
            lower_src("main: function () nothing {\n    x: @format_args = @format_args_new()\n}\n");
        assert!(!diags.has_errors(), "{diags:?}");
        assert!(text.contains("call @.lilac.format_args_new"), "{text}");
    }

    #[test]
    fn identical_strings_share_one_const() {
        let (text, diags) = lower_src(
            "main: function () nothing {\n    a: @byte_buffer = \"hi\"\n    b: @byte_buffer = \"hi\"\n}\n",
        );
        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(text.matches("str = \"hi\"").count(), 1, "{text}");
    }
}
