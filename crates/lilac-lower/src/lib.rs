//! HIR -> LIR lowering: places, ownership, and control flow.
//!
//! Ownership is decided here, uniformly:
//!
//! - Reading a *place* (local, field, dereference) into a new owner copies it:
//!   the type's `@on_copy` hook, or copy glue when only its fields have hooks,
//!   or a plain bit copy. Every other expression is a fresh value and moves.
//! - Every `let` local is dropped when its block exits (reverse declaration
//!   order); `return` and `break` drop the scopes they leave. By-value
//!   parameters belong to the callee and are dropped on exit; return slots
//!   move to the caller. A fresh value nobody takes is dropped at the end of
//!   its statement.
//! - Dropping runs the type's `@on_drop`, then drops every field (drop glue).
//! - Assignment evaluates the new value, runs `@on_override(ref old, ref new)`,
//!   drops the old value, then moves the new one in.
//!
//! Arguments always reach the callee in a fresh place (the callee owns and may
//! modify its parameters), and every function zeroes its return slots on
//! entry, so a slot starts zeroed on each call.

mod constant;

use std::collections::{HashMap, HashSet};

use lilac_error::{Diagnostic, Diags};
use lilac_hir::intrinsics::{Intrinsic, NumOp};
use lilac_hir::{
    FuncKey, HirArm, HirBlock, HirCallee, HirExpr, HirExprKind, HirFunc, HirLocal, HirPattern,
    HirProgram, HirStmt, HirValueArm, Layout, LocalId,
};
use lilac_ir::{
    ArithOp, ConvertOp, LirBlock, LirCallee, LirData, LirFunc, LirInst, LirModule, LirOperand,
    LirTerm, LirTy,
};
use lilac_resolve::{DefKind, Resolved};
use lilac_span::{BytePos, SourceMap, Span};
use lilac_ty::{TypeId, TypeKind, TypeRef, Types};

/// Lowered LIR plus each function's source name.
pub struct Lowered {
    /// The module.
    pub module: LirModule,
    /// LIR symbol -> name as written in source (`Holder[@bit32].make`);
    /// stack traces print these.
    pub names: HashMap<String, String>,
}

/// Lowers checked, error-free HIR to LIR. `ptr_bytes` is the target's pointer
/// size (for `@size_of`); `sources` locates `@panic` sites.
#[must_use]
pub fn lower(
    hir: &HirProgram,
    res: &Resolved,
    types: &Types,
    sources: &SourceMap,
    triple: &str,
    ptr_bytes: u64,
) -> Lowered {
    let mut lower = Lower {
        hir,
        res,
        types,
        sources,
        names: HashMap::new(),
        ptr_bytes,
        module: LirModule::new(triple),
        symbols: HashMap::new(),
        externs: HashSet::new(),
        drop_glue: HashMap::new(),
        copy_glue: HashMap::new(),
        needs_drop: HashMap::new(),
        needs_copy: HashMap::new(),
        funcs: hir
            .funcs
            .iter()
            .map(|func| (func.key.clone(), func))
            .collect(),
    };
    for func in &hir.funcs {
        let symbol = lower.symbol_of(func);
        if func.body.is_none() {
            lower.externs.insert(func.key.clone());
        } else {
            let name = lower.display_of(func);
            lower.names.insert(symbol.clone(), name);
        }
        lower.symbols.insert(func.key.clone(), symbol);
    }
    let mut layouts: Vec<(&TypeId, &Layout)> = hir.layouts.iter().collect();
    layouts.sort_by_key(|(id, _)| id.0);
    for (id, layout) in layouts {
        let name = lower.type_name(*id);
        let fields = match layout {
            Layout::Object { fields } => fields
                .iter()
                .map(|(name, ty)| (name.clone(), lower.lir_ty(*ty)))
                .collect(),
            Layout::Enum { variants, tag } => {
                let mut fields = vec![("tag".to_owned(), lower.lir_ty(TypeRef::value(*tag)))];
                for (name, payload) in variants {
                    if let Some(payload) = payload {
                        fields.push((name.clone(), lower.lir_ty(*payload)));
                    }
                }
                fields
            }
        };
        lower.module.layouts.insert(name, fields);
    }
    for func in &hir.funcs {
        let lowered = lower.lower_func(func);
        lower.module.funcs.push(lowered);
    }
    Lowered {
        module: lower.module,
        names: lower.names,
    }
}

/// Diagnoses `extern` signatures the C ABI cannot carry (`E028`): parameters
/// and returns must be bit integers, floats, `ref T`, or an object wrapping
/// exactly one such field; at most one return; never named `main`.
pub fn check_extern_signatures(hir: &HirProgram, res: &Resolved, types: &Types, diags: &mut Diags) {
    let sources = SourceMap::new();
    let lower = Lower {
        hir,
        res,
        types,
        sources: &sources,
        names: HashMap::new(),
        ptr_bytes: 8,
        module: LirModule::new(""),
        symbols: HashMap::new(),
        externs: HashSet::new(),
        drop_glue: HashMap::new(),
        copy_glue: HashMap::new(),
        needs_drop: HashMap::new(),
        needs_copy: HashMap::new(),
        funcs: HashMap::new(),
    };
    for func in hir.funcs.iter().filter(|func| func.body.is_none()) {
        let name = &res.def(func.key.def).name;
        if name == "main" {
            diags.emit(
                Diagnostic::error(
                    "E028",
                    "`extern` function `main` conflicts with the program entry point",
                )
                .label(func.span, true, "`main` is reserved for the entry point")
                .help("rename the `extern`; an `extern` symbol is unmangled and cannot be `main`"),
            );
        }
        let mut anonymous = 0;
        let mut place = |id: LocalId, prefix: &str| {
            let local = func.local(id);
            if local.name.is_empty() {
                anonymous += 1;
                format!("{prefix}.{anonymous}")
            } else {
                local.name.clone()
            }
        };
        for id in &func.params {
            let local = func.local(*id);
            let ty = lower.abi_ty(local.ty).0;
            if !lilac_ir::is_c_abi_type(&ty) {
                let place = place(*id, "arg");
                diags.emit(
                    Diagnostic::error("E028", format!("`extern` parameter `{place}` has unsupported C type `{ty}`"))
                        .label(local.span, true, "not a C ABI type")
                        .help("`extern` parameters must be a bit integer, a float, `ref T`, or an object wrapping one of those"),
                );
            }
        }
        if func.returns.len() > 1 {
            let slot = func.local(func.returns[1]).span;
            diags.emit(
                Diagnostic::error(
                    "E028",
                    format!(
                        "`extern` function `{name}` declares {} out places; at most one is allowed",
                        func.returns.len()
                    ),
                )
                .label(slot, true, "extra out place")
                .help("an `extern` may return at most one C value"),
            );
        } else if let Some(id) = func.returns.first() {
            let local = func.local(*id);
            let ty = lower.abi_ty(local.ty).0;
            if !lilac_ir::is_c_abi_type(&ty) {
                let place = place(*id, "out");
                diags.emit(
                    Diagnostic::error("E028", format!("`extern` return `{place}` has unsupported C type `{ty}`"))
                        .label(local.span, true, "not a C ABI type")
                        .help("`extern` returns must be a bit integer, a float, `ref T`, or an object wrapping one of those"),
                );
            }
        }
    }
}

struct Lower<'a> {
    hir: &'a HirProgram,
    res: &'a Resolved,
    types: &'a Types,
    /// Locates `@panic` sites.
    sources: &'a SourceMap,
    /// Source name of every function with a body, by LIR symbol.
    names: HashMap<String, String>,
    ptr_bytes: u64,
    module: LirModule,
    /// LIR symbol of every function instance.
    symbols: HashMap<FuncKey, String>,
    /// `extern` instances (C ABI calls).
    externs: HashSet<FuncKey>,
    /// Drop routine per type (a hook or generated glue).
    drop_glue: HashMap<TypeId, String>,
    /// Copy routine per type (a hook or generated glue).
    copy_glue: HashMap<TypeId, String>,
    needs_drop: HashMap<TypeId, bool>,
    needs_copy: HashMap<TypeId, bool>,
    /// Every function instance by key (constant `=>` calls).
    funcs: HashMap<FuncKey, &'a HirFunc>,
}

/// A place: a named LIR place, or a `ref` place holding the address of one.
#[derive(Clone, Debug)]
enum Place {
    Direct(String),
    Indirect(String),
}

impl Place {
    fn name(&self) -> &str {
        match self {
            Self::Direct(name) | Self::Indirect(name) => name,
        }
    }
}

/// Owned places of one block (or expression region).
#[derive(Default)]
struct Scope {
    /// `let` locals and by-value parameters needing a drop, in order.
    locals: Vec<(String, TypeRef)>,
    /// Fresh values nobody took yet (dropped at the end of their statement).
    temps: Vec<(String, TypeRef)>,
}

/// Per-function emission state.
struct Fs {
    ins: Vec<(String, LirTy)>,
    outs: Vec<(String, LirTy)>,
    blocks: Vec<LirBlock>,
    label: String,
    insts: Vec<LirInst>,
    open: bool,
    labels: u32,
    regs: u32,
    tmps: u32,
    /// LIR place of every declared local.
    places: HashMap<LocalId, String>,
    /// Place names in use (shadowing gets `.N` suffixes).
    used: HashSet<String>,
    /// Hoisted entry-block allocations.
    allocas: Vec<LirInst>,
    /// Place types.
    place_tys: HashMap<String, LirTy>,
    scopes: Vec<Scope>,
    /// `(head label, exit label, scope depth)` per enclosing loop.
    loops: Vec<(String, String, usize)>,
}

impl Fs {
    fn tmp_lir(&mut self, lir: LirTy, span: Span) -> String {
        self.tmps += 1;
        let place = format!("tmp.{}", self.tmps);
        self.used.insert(place.clone());
        self.alloca(&place, lir, span);
        place
    }

    fn read_place(&mut self, place: &Place, lir: &LirTy, span: Span) -> LirOperand {
        match place {
            Place::Direct(name) => LirOperand::Place(name.clone()),
            Place::Indirect(name) => {
                let reg = self.reg();
                self.emit(LirInst::Load {
                    src: name.clone(),
                    dst: reg.clone(),
                    ty: lir.clone(),
                    span,
                });
                LirOperand::Reg(reg)
            }
        }
    }

    fn write(&mut self, place: &Place, value: LirOperand, span: Span) {
        match place {
            Place::Direct(name) => self.emit(LirInst::CopyPlace {
                src: value,
                dst: name.clone(),
                span,
            }),
            Place::Indirect(name) => self.emit(LirInst::Store {
                src: value,
                dst: name.clone(),
                span,
            }),
        }
    }

    fn reg_of(&mut self, operand: LirOperand, lir: &LirTy, span: Span) -> String {
        match operand {
            LirOperand::Reg(reg) => reg,
            LirOperand::Place(place) => {
                let reg = self.reg();
                self.emit(LirInst::CopyReg {
                    src: LirOperand::Place(place),
                    dst: reg.clone(),
                    ty: lir.clone(),
                    span,
                });
                reg
            }
        }
    }

    fn new() -> Self {
        Self {
            ins: Vec::new(),
            outs: Vec::new(),
            blocks: Vec::new(),
            label: "entry".to_owned(),
            insts: Vec::new(),
            open: true,
            labels: 0,
            regs: 0,
            tmps: 0,
            places: HashMap::new(),
            used: HashSet::new(),
            allocas: Vec::new(),
            place_tys: HashMap::new(),
            scopes: vec![Scope::default()],
            loops: Vec::new(),
        }
    }

    fn emit(&mut self, inst: LirInst) {
        if !self.open {
            self.labels += 1;
            self.label = format!("l.{}", self.labels);
            self.open = true;
        }
        self.insts.push(inst);
    }

    fn end(&mut self, term: LirTerm) {
        if !self.open {
            return;
        }
        self.blocks.push(LirBlock {
            label: std::mem::take(&mut self.label),
            insts: std::mem::take(&mut self.insts),
            span: term.span(),
            term,
        });
        self.open = false;
    }

    fn at(&mut self, label: String) {
        self.label = label;
        self.open = true;
    }

    fn label(&mut self) -> String {
        self.labels += 1;
        format!("l.{}", self.labels)
    }

    fn reg(&mut self) -> String {
        self.regs += 1;
        self.regs.to_string()
    }

    fn branch(&mut self, target: &str, span: Span) {
        self.end(LirTerm::Branch {
            target: target.to_owned(),
            span,
        });
    }

    /// A fresh name for a place, avoiding collisions.
    fn place_name(&mut self, base: &str) -> String {
        let mut name = base.to_owned();
        let mut counter = 0;
        while !self.used.insert(name.clone()) {
            counter += 1;
            name = format!("{base}.{counter}");
        }
        name
    }

    fn alloca(&mut self, place: &str, ty: LirTy, span: Span) {
        self.place_tys.insert(place.to_owned(), ty.clone());
        self.allocas.push(LirInst::Alloca {
            ty,
            place: place.to_owned(),
            span,
        });
    }

    /// Closes the function; an open block falls off the end at `span`.
    fn finish(mut self, name: String, inline: bool, span: Span) -> LirFunc {
        self.end(LirTerm::Return { span });
        if let Some(first) = self.blocks.first_mut() {
            let mut insts = std::mem::take(&mut self.allocas);
            insts.append(&mut first.insts);
            first.insts = insts;
        }
        LirFunc {
            name,
            ins: self.ins,
            outs: self.outs,
            is_extern: false,
            inline,
            blocks: self.blocks,
        }
    }
}

/// The last byte of `span`: a block's closing `}`, where its scope ends.
fn end_of(span: Span) -> Span {
    if span.is_dummy() || span.is_empty() {
        return span;
    }
    Span {
        lo: BytePos(span.hi.0 - 1),
        ..span
    }
}

/// Where a statement's temporaries die: the statement's own expression.
fn stmt_span(stmt: &HirStmt) -> Span {
    match stmt {
        HirStmt::MultiLet { call, .. } => call.span,
        HirStmt::Assign { place, value } if place.span.file == value.span.file => Span {
            lo: place.span.lo.min(value.span.lo),
            hi: place.span.hi.max(value.span.hi),
            file: place.span.file,
        },
        HirStmt::Let { value, .. } | HirStmt::Assign { value, .. } => value.span,
        HirStmt::Expr(expr) => expr.span,
        HirStmt::Return(span) | HirStmt::Break(span) | HirStmt::Continue(span) => *span,
        HirStmt::If { cond, .. } => cond.span,
        HirStmt::Loop(block) | HirStmt::Block(block) => block.span,
        HirStmt::Match { scrutinee, .. } => scrutinee.span,
    }
}

fn is_fresh(place: &str) -> bool {
    place.starts_with("tmp.")
}

impl Lower<'_> {
    // ---- names and types ---------------------------------------------------

    /// The LIR name of a concrete object/enum type (without `#`).
    fn type_name(&self, id: TypeId) -> String {
        match self.types.kind(id) {
            TypeKind::Instance { .. } => self.types.mangle(id),
            _ => self.types.display(id),
        }
    }

    fn lir_ty(&self, ty: TypeRef) -> LirTy {
        let base = match self.types.kind(ty.id) {
            TypeKind::Func { .. } => return LirTy::opaque_ref(),
            TypeKind::Bit8 => LirTy::Bit(8),
            TypeKind::Bit16 => LirTy::Bit(16),
            TypeKind::Bit32 => LirTy::Bit(32),
            TypeKind::Bit64 => LirTy::Bit(64),
            TypeKind::BitNative => LirTy::BitNative,
            TypeKind::FBit16 => LirTy::FBit(16),
            TypeKind::FBit32 => LirTy::FBit(32),
            TypeKind::FBit64 => LirTy::FBit(64),
            TypeKind::FBit128 => LirTy::FBit(128),
            TypeKind::ByteBuffer => LirTy::byte_buffer(),
            TypeKind::FormatArgs => LirTy::format_args(),
            TypeKind::Unknown => LirTy::Named("unknown".to_owned()),
            TypeKind::Object(_) | TypeKind::Instance { .. } => LirTy::Named(self.type_name(ty.id)),
            TypeKind::Param { .. } | TypeKind::Error => LirTy::Named("error".to_owned()),
        };
        if ty.is_ref {
            base.to_ref()
        } else {
            base
        }
    }

    fn mangle_ref(&self, ty: TypeRef) -> String {
        if ty.is_ref {
            format!("ref.{}", self.types.mangle(ty.id))
        } else {
            self.types.mangle(ty.id)
        }
    }

    /// A function's name as written in source, with generic arguments:
    /// `Holder[@bit32].make` for the symbol `Holder.1..bit32.make`. Stack
    /// traces print it.
    fn display_of(&self, func: &HirFunc) -> String {
        let def = self.res.def(func.key.def);
        let generics = |args: &[TypeRef]| {
            if args.is_empty() {
                String::new()
            } else {
                let args: Vec<String> = args
                    .iter()
                    .map(|arg| self.types.display_ref(*arg))
                    .collect();
                format!("[{}]", args.join(", "))
            }
        };
        let (owner, own) = match def.parent {
            Some(parent) => {
                let count = self.res.def(parent).generic_params.len();
                let owner = self.res.qualified_name(parent);
                (
                    format!("{owner}{}", generics(&func.key.args[..count])),
                    &func.key.args[count..],
                )
            }
            None if matches!(def.kind, DefKind::Hook(_)) => (String::new(), &func.key.args[..]),
            None => (
                self.res.namespace_path(def.ns).to_owned(),
                &func.key.args[..],
            ),
        };
        let name = if owner.is_empty() {
            def.name.clone()
        } else {
            format!("{owner}.{}", def.name)
        };
        format!("{name}{}", generics(own))
    }

    /// The LIR symbol of a function instance: namespace or owning type, name,
    /// own generic arguments (arity-tagged), and for hooks the signature.
    fn symbol_of(&self, func: &HirFunc) -> String {
        let def = self.res.def(func.key.def);
        if func.body.is_none() {
            return def.name.clone();
        }
        let is_hook = matches!(def.kind, DefKind::Hook(_));
        let (prefix, own) = match def.parent {
            Some(parent) => {
                let count = self.res.def(parent).generic_params.len();
                let owner = self.res.qualified_name(parent);
                let owner = if count == 0 {
                    owner
                } else {
                    let mut name = format!("{owner}.{count}");
                    for arg in &func.key.args[..count] {
                        name.push('.');
                        name.push_str(&self.mangle_ref(*arg));
                    }
                    name
                };
                (owner, &func.key.args[count..])
            }
            None if is_hook => (String::new(), &func.key.args[..]),
            None => (
                self.res.namespace_path(def.ns).to_owned(),
                &func.key.args[..],
            ),
        };
        let mut name = if prefix.is_empty() {
            def.name.clone()
        } else {
            format!("{prefix}.{}", def.name)
        };
        if !own.is_empty() {
            name.push('.');
            name.push_str(&own.len().to_string());
            for arg in own {
                name.push('.');
                name.push_str(&self.mangle_ref(*arg));
            }
        }
        if is_hook {
            for id in func.params.iter().chain(&func.returns) {
                let display = self.types.display_ref(func.local(*id).ty);
                name.push(':');
                name.push_str(&display.replace('@', "").replace(' ', "_"));
            }
        }
        name
    }

    fn symbol(&self, key: &FuncKey) -> String {
        match self.symbols.get(key) {
            Some(symbol) => symbol.clone(),
            None => lilac_error::bug!(Span::dummy(), "call to an unchecked instance {key:?}"),
        }
    }

    /// The C type an `extern` slot uses: the type itself, or for an object
    /// wrapping exactly one C-representable field, that field (`true`).
    fn abi_ty(&self, ty: TypeRef) -> (LirTy, bool) {
        let lir = self.lir_ty(ty);
        if !ty.is_ref {
            if let Some(Layout::Object { fields }) = self.hir.layouts.get(&ty.id) {
                if let [(_, field)] = fields.as_slice() {
                    let inner = self.lir_ty(*field);
                    if lilac_ir::is_c_abi_type(&inner) {
                        return (inner, true);
                    }
                }
            }
        }
        (lir, false)
    }

    fn layout(&self, ty: TypeId) -> Option<&Layout> {
        self.hir.layouts.get(&ty)
    }

    /// LIR field index and name of a payload variant.
    fn payload_field(&self, ty: TypeId, variant: u32) -> (u32, String) {
        let Some(Layout::Enum { variants, .. }) = self.layout(ty) else {
            lilac_error::bug!(Span::dummy(), "payload of a non-enum type");
        };
        let before = variants[..variant as usize]
            .iter()
            .filter(|(_, payload)| payload.is_some())
            .count();
        (
            u32::try_from(before + 1).unwrap_or(u32::MAX),
            variants[variant as usize].0.clone(),
        )
    }

    fn field_name(&self, ty: TypeId, index: u32) -> (String, TypeRef) {
        match self.layout(ty) {
            Some(Layout::Object { fields }) => fields[index as usize].clone(),
            _ => lilac_error::bug!(Span::dummy(), "field of a non-object type"),
        }
    }

    // ---- ownership facts -----------------------------------------------------

    fn needs_drop(&mut self, ty: TypeRef) -> bool {
        if ty.is_ref {
            return false;
        }
        if let Some(known) = self.needs_drop.get(&ty.id) {
            return *known;
        }
        self.needs_drop.insert(ty.id, false);
        let hooked = self
            .hir
            .hooks
            .get(&ty.id)
            .is_some_and(|hooks| hooks.drop.is_some());
        let result = hooked
            || self
                .slot_types(ty.id)
                .into_iter()
                .any(|slot| self.needs_drop(slot));
        self.needs_drop.insert(ty.id, result);
        result
    }

    fn needs_copy(&mut self, ty: TypeRef) -> bool {
        if ty.is_ref {
            return false;
        }
        if let Some(known) = self.needs_copy.get(&ty.id) {
            return *known;
        }
        self.needs_copy.insert(ty.id, false);
        let hooked = self
            .hir
            .hooks
            .get(&ty.id)
            .is_some_and(|hooks| hooks.copy.is_some());
        let result = hooked
            || self
                .slot_types(ty.id)
                .into_iter()
                .any(|slot| self.needs_copy(slot));
        self.needs_copy.insert(ty.id, result);
        result
    }

    /// Field or payload types of an aggregate.
    fn slot_types(&self, ty: TypeId) -> Vec<TypeRef> {
        match self.layout(ty) {
            Some(Layout::Object { fields }) => fields.iter().map(|(_, ty)| *ty).collect(),
            Some(Layout::Enum { variants, .. }) => {
                variants.iter().filter_map(|(_, ty)| *ty).collect()
            }
            None => Vec::new(),
        }
    }

    // ---- functions -------------------------------------------------------------

    fn lower_func(&mut self, func: &HirFunc) -> LirFunc {
        let symbol = self.symbol(&func.key);
        let mut fs = Fs::new();
        let extern_ = func.body.is_none();
        let mut anonymous = 0;
        for (list, prefix) in [(&func.params, "arg"), (&func.returns, "out")] {
            for id in list {
                let local = func.local(*id);
                let base = if local.name.is_empty() {
                    anonymous += 1;
                    format!("{prefix}.{anonymous}")
                } else {
                    local.name.clone()
                };
                let place = fs.place_name(&base);
                let ty = if extern_ {
                    self.abi_ty(local.ty).0
                } else {
                    self.lir_ty(local.ty)
                };
                fs.place_tys.insert(place.clone(), ty.clone());
                fs.places.insert(*id, place.clone());
                if prefix == "arg" {
                    fs.ins.push((place, ty));
                } else {
                    fs.outs.push((place, ty));
                }
            }
        }
        let Some(body) = &func.body else {
            return LirFunc {
                name: symbol,
                ins: fs.ins,
                outs: fs.outs,
                is_extern: true,
                inline: false,
                blocks: Vec::new(),
            };
        };
        for (place, _) in fs.outs.clone() {
            fs.emit(LirInst::Zero {
                place,
                span: func.span,
            });
        }
        for id in &func.params {
            let ty = func.local(*id).ty;
            if self.needs_drop(ty) {
                let place = fs.places[id].clone();
                fs.scopes[0].locals.push((place, ty));
            }
        }
        for stmt in &body.stmts {
            self.lower_stmt(&mut fs, func, stmt);
        }
        let end = end_of(body.span);
        if fs.open {
            self.drop_scope(&mut fs, 0, end);
        }
        fs.finish(symbol, func.inline, end)
    }

    /// Declares a local's place (zeroed alloca) and returns it.
    fn declare(&mut self, fs: &mut Fs, func: &HirFunc, id: LocalId) -> String {
        let local = func.local(id);
        let base = if local.name.is_empty() {
            format!("l.{}", id.0)
        } else {
            local.name.clone()
        };
        let place = fs.place_name(&base);
        let ty = self.lir_ty(local.ty);
        fs.alloca(&place, ty, local.span);
        fs.places.insert(id, place.clone());
        place
    }

    /// Registers a declared local as owned by the current scope.
    fn own(&mut self, fs: &mut Fs, place: &str, ty: TypeRef) {
        if self.needs_drop(ty) {
            if let Some(scope) = fs.scopes.last_mut() {
                scope.locals.push((place.to_owned(), ty));
            }
        }
    }

    /// A fresh temporary place of type `ty`.
    fn tmp(&mut self, fs: &mut Fs, ty: TypeRef, span: Span) -> String {
        let lir = self.lir_ty(ty);
        fs.tmp_lir(lir, span)
    }

    /// Registers a fresh value as owned until the end of the statement.
    fn own_temp(&mut self, fs: &mut Fs, place: &str, ty: TypeRef) {
        if self.needs_drop(ty) {
            if let Some(scope) = fs.scopes.last_mut() {
                scope.temps.push((place.to_owned(), ty));
            }
        }
    }

    /// Marks a fresh value as taken (it moved into a new owner).
    fn take_temp(fs: &mut Fs, place: &str) {
        for scope in fs.scopes.iter_mut().rev() {
            if let Some(index) = scope.temps.iter().position(|(name, _)| name == place) {
                scope.temps.remove(index);
                return;
            }
        }
    }

    /// Drops everything scope `index` owns (temps, then locals, newest
    /// first); `span` is where the scope ends (its closing `}`, or the
    /// `return`/`break` leaving it).
    fn drop_scope(&mut self, fs: &mut Fs, index: usize, span: Span) {
        let owned: Vec<(String, TypeRef)> = fs.scopes[index]
            .temps
            .iter()
            .rev()
            .chain(fs.scopes[index].locals.iter().rev())
            .cloned()
            .collect();
        for (place, ty) in owned {
            self.drop_place(fs, &Place::Direct(place), ty, span);
        }
    }

    // ---- statements --------------------------------------------------------------

    fn lower_block(&mut self, fs: &mut Fs, func: &HirFunc, block: &HirBlock) {
        fs.scopes.push(Scope::default());
        for stmt in &block.stmts {
            self.lower_stmt(fs, func, stmt);
        }
        if fs.open {
            let index = fs.scopes.len() - 1;
            self.drop_scope(fs, index, end_of(block.span));
        }
        fs.scopes.pop();
    }

    #[allow(clippy::too_many_lines)]
    fn lower_stmt(&mut self, fs: &mut Fs, func: &HirFunc, stmt: &HirStmt) {
        let scope = fs.scopes.len() - 1;
        let mark = fs.scopes[scope].temps.len();
        match stmt {
            HirStmt::Let { local, value } => {
                let place = self.declare(fs, func, *local);
                let operand = self.read(fs, func, value);
                fs.emit(LirInst::CopyPlace {
                    src: operand,
                    dst: place.clone(),
                    span: value.span,
                });
                self.own(fs, &place, func.local(*local).ty);
            }
            HirStmt::MultiLet { locals, call } => {
                let places: Vec<String> = locals
                    .iter()
                    .map(|id| self.declare(fs, func, *id))
                    .collect();
                if let HirExprKind::Call {
                    callee: callee @ (HirCallee::Func(_) | HirCallee::Local(_)),
                    args,
                } = &call.kind
                {
                    self.emit_call(fs, func, callee, args, &places, call.span);
                } else {
                    let operand = self.read(fs, func, call);
                    fs.emit(LirInst::CopyPlace {
                        src: operand,
                        dst: places[0].clone(),
                        span: call.span,
                    });
                }
                for (place, id) in places.iter().zip(locals) {
                    self.own(fs, place, func.local(*id).ty);
                }
            }
            HirStmt::Assign { place, value } => self.lower_assign(fs, func, place, value),
            HirStmt::Expr(expr) => match &expr.kind {
                HirExprKind::Call { callee, args } if expr.ty.is_error() => {
                    if let HirCallee::Intrinsic(intrinsic) = callee {
                        let _ =
                            self.lower_intrinsic(fs, func, *intrinsic, args, expr.ty, expr.span);
                    } else {
                        self.emit_call(fs, func, callee, args, &[], expr.span);
                    }
                }
                HirExprKind::CompileError { msg, msg_span } => {
                    fs.emit(LirInst::CompileError {
                        msg: msg.clone(),
                        msg_span: *msg_span,
                        call_span: expr.span,
                    });
                }
                _ => {
                    let _ = self.value(fs, func, expr);
                }
            },
            HirStmt::Return(span) => {
                for index in (0..fs.scopes.len()).rev() {
                    self.drop_scope(fs, index, *span);
                }
                fs.end(LirTerm::Return { span: *span });
            }
            HirStmt::Break(span) | HirStmt::Continue(span) => {
                let Some((head, exit, depth)) = fs.loops.last().cloned() else {
                    lilac_error::bug!(*span, "break or continue outside a loop");
                };
                for index in (depth..fs.scopes.len()).rev() {
                    self.drop_scope(fs, index, *span);
                }
                let target = if matches!(stmt, HirStmt::Break(_)) {
                    exit
                } else {
                    head
                };
                fs.branch(&target, *span);
            }
            HirStmt::If { cond, then, els } => {
                // A constant condition lowers only its live arm.
                if let Some(taken) = self.constant(cond).and_then(|value| value.truthy()) {
                    self.lower_block(fs, func, if taken { then } else { els });
                } else {
                    self.lower_if(fs, func, cond, then, els);
                }
            }
            HirStmt::Loop(body) => {
                let (head, exit) = (fs.label(), fs.label());
                fs.branch(&head, body.span);
                fs.at(head.clone());
                fs.loops.push((head.clone(), exit.clone(), fs.scopes.len()));
                self.lower_block(fs, func, body);
                fs.loops.pop();
                fs.branch(&head, body.span);
                fs.at(exit);
            }
            HirStmt::Match {
                scrutinee,
                arms,
                els,
            } => self.lower_match(fs, func, scrutinee, arms, els),
            HirStmt::Block(block) => self.lower_block(fs, func, block),
        }
        if fs.open {
            let temps: Vec<(String, TypeRef)> = fs.scopes[scope].temps[mark..]
                .iter()
                .rev()
                .cloned()
                .collect();
            let span = stmt_span(stmt);
            for (place, ty) in temps {
                self.drop_place(fs, &Place::Direct(place), ty, span);
            }
        }
        fs.scopes[scope].temps.truncate(mark);
    }

    fn lower_if(
        &mut self,
        fs: &mut Fs,
        func: &HirFunc,
        cond: &HirExpr,
        then: &HirBlock,
        els: &HirBlock,
    ) {
        let cond = self.value_reg(fs, func, cond);
        let (then_label, els_label, join) = (fs.label(), fs.label(), fs.label());
        fs.end(LirTerm::BranchIf {
            cond,
            then: then_label.clone(),
            els: Some(els_label.clone()),
            span: then.span,
        });
        fs.at(then_label);
        self.lower_block(fs, func, then);
        fs.branch(&join, then.span);
        fs.at(els_label);
        self.lower_block(fs, func, els);
        fs.branch(&join, els.span);
        fs.at(join);
    }

    fn lower_assign(&mut self, fs: &mut Fs, func: &HirFunc, place: &HirExpr, value: &HirExpr) {
        let ty = place.ty;
        let operand = self.read(fs, func, value);
        let target = self.place_of(fs, func, place);
        let override_ = self
            .hir
            .hooks
            .get(&ty.id)
            .and_then(|hooks| hooks.override_.clone());
        let drops = self.needs_drop(ty);
        if override_.is_none() && !drops {
            fs.write(&target, operand, value.span);
            return;
        }
        let new = self.fresh_place(fs, operand, ty, value.span);
        // Assigning an out slot initializes the result, so `@on_override` is
        // skipped (a guard type could otherwise never be returned); the old
        // value is still dropped.
        let is_out = matches!(place.kind, HirExprKind::Local(id) if func.returns.contains(&id));
        if let Some(hook) = override_.filter(|_| !ty.is_ref && !is_out) {
            let old_ref = self.ref_place(fs, &target, ty, place.span);
            let new_ref = self.ref_place(fs, &Place::Direct(new.clone()), ty, place.span);
            let symbol = self.symbol(&hook);
            fs.emit(LirInst::Call {
                func: LirCallee::Func(symbol),
                ins: vec![old_ref, new_ref],
                outs: Vec::new(),
                span: place.span,
            });
        }
        if drops {
            self.drop_place(fs, &target, ty, place.span);
        }
        fs.write(&target, LirOperand::Place(new), value.span);
    }

    fn lower_match(
        &mut self,
        fs: &mut Fs,
        func: &HirFunc,
        scrutinee: &HirExpr,
        arms: &[HirArm],
        els: &HirBlock,
    ) {
        if let Some((choice, _)) =
            self.constant_choice(scrutinee, arms.iter().map(|arm| &arm.pattern))
        {
            // A constant scrutinee lowers only the chosen arm.
            let Some(arm) = choice.map(|index| &arms[index]) else {
                self.lower_block(fs, func, els);
                return;
            };
            fs.scopes.push(Scope::default());
            if let Some(capture) = arm.capture {
                let (subject, _) = self.subject(fs, func, scrutinee);
                self.bind_capture(fs, func, &subject, scrutinee.ty, &arm.pattern, capture);
            }
            for stmt in &arm.body.stmts {
                self.lower_stmt(fs, func, stmt);
            }
            if fs.open {
                let index = fs.scopes.len() - 1;
                self.drop_scope(fs, index, end_of(arm.body.span));
            }
            fs.scopes.pop();
            return;
        }
        let (subject, is_enum) = self.subject(fs, func, scrutinee);
        let join = fs.label();
        let els_label = fs.label();
        let mut next = fs.label();
        fs.branch(&next, scrutinee.span);
        for (index, arm) in arms.iter().enumerate() {
            fs.at(next);
            let body = fs.label();
            next = if index + 1 == arms.len() {
                els_label.clone()
            } else {
                fs.label()
            };
            let test = self.pattern_test(
                fs,
                &subject,
                scrutinee.ty,
                is_enum,
                &arm.pattern,
                scrutinee.span,
            );
            fs.end(LirTerm::BranchIf {
                cond: test,
                then: body.clone(),
                els: Some(next.clone()),
                span: arm.body.span,
            });
            fs.at(body);
            fs.scopes.push(Scope::default());
            if let Some(capture) = arm.capture {
                self.bind_capture(fs, func, &subject, scrutinee.ty, &arm.pattern, capture);
            }
            for stmt in &arm.body.stmts {
                self.lower_stmt(fs, func, stmt);
            }
            if fs.open {
                let index = fs.scopes.len() - 1;
                self.drop_scope(fs, index, end_of(arm.body.span));
            }
            fs.scopes.pop();
            fs.branch(&join, arm.body.span);
        }
        if arms.is_empty() {
            fs.at(next);
            fs.branch(&els_label, els.span);
        }
        fs.at(els_label);
        self.lower_block(fs, func, els);
        fs.branch(&join, els.span);
        fs.at(join);
    }

    /// The place a `match` inspects (fresh values spill into a temp).
    fn subject(&mut self, fs: &mut Fs, func: &HirFunc, scrutinee: &HirExpr) -> (Place, bool) {
        let is_enum = matches!(self.layout(scrutinee.ty.id), Some(Layout::Enum { .. }));
        if scrutinee.is_place() {
            return (self.place_of(fs, func, scrutinee), is_enum);
        }
        let operand = self.value(fs, func, scrutinee);
        let place = self.spill(fs, operand, scrutinee.ty, scrutinee.span);
        (Place::Direct(place), is_enum)
    }

    /// `&test: #bit8` for one arm pattern.
    fn pattern_test(
        &mut self,
        fs: &mut Fs,
        subject: &Place,
        ty: TypeRef,
        is_enum: bool,
        pattern: &HirPattern,
        span: Span,
    ) -> String {
        let (value, constant, lir) = if is_enum {
            let HirPattern::Variant(index) = pattern else {
                lilac_error::bug!(span, "byte pattern on an enum");
            };
            let Some(Layout::Enum { tag, .. }) = self.layout(ty.id) else {
                lilac_error::bug!(span, "enum without a layout");
            };
            let tag = self.lir_ty(TypeRef::value(*tag));
            let value = fs.reg();
            fs.emit(LirInst::Field {
                obj: subject.name().to_owned(),
                name: "tag".to_owned(),
                idx: 0,
                dst: value.clone(),
                ty: tag.clone(),
                span,
            });
            (value, format!("0x{index:x}"), tag)
        } else {
            let HirPattern::Const(bytes) = pattern else {
                lilac_error::bug!(span, "tag pattern on a bit scrutinee");
            };
            let lir = self.lir_ty(ty);
            let value = fs.read_place(subject, &lir, span);
            let value = fs.reg_of(value, &lir, span);
            (value, bytes.clone(), lir)
        };
        let constant_reg = fs.reg();
        fs.emit(LirInst::Const {
            dst: constant_reg.clone(),
            lit: constant,
            ty: lir,
            span,
        });
        let test = fs.reg();
        fs.emit(LirInst::Arith {
            op: ArithOp::Eq,
            lhs: value,
            rhs: constant_reg,
            dst: test.clone(),
            ty: LirTy::Bit(8),
            span,
        });
        test
    }

    /// Initializes a capture local with a copy of the matched payload.
    fn bind_capture(
        &mut self,
        fs: &mut Fs,
        func: &HirFunc,
        subject: &Place,
        ty: TypeRef,
        pattern: &HirPattern,
        capture: LocalId,
    ) {
        let HirPattern::Variant(index) = pattern else {
            lilac_error::bug!(Span::dummy(), "capture on a byte pattern");
        };
        let place = self.declare(fs, func, capture);
        let HirLocal {
            ty: capture_ty,
            span,
            ..
        } = *func.local(capture);
        let (field, name) = self.payload_field(ty.id, *index);
        let payload = self.field_place(fs, subject, field, &name, capture_ty, span);
        let value = self.copy_out(fs, &payload, capture_ty, span);
        fs.emit(LirInst::CopyPlace {
            src: value,
            dst: place.clone(),
            span,
        });
        self.own(fs, &place, capture_ty);
    }

    // ---- places ------------------------------------------------------------------

    fn place_of(&mut self, fs: &mut Fs, func: &HirFunc, expr: &HirExpr) -> Place {
        match &expr.kind {
            HirExprKind::Local(id) => match fs.places.get(id) {
                Some(place) => Place::Direct(place.clone()),
                None => lilac_error::bug!(expr.span, "use of an undeclared local"),
            },
            HirExprKind::Deref(inner) => {
                if let HirExprKind::Local(id) = inner.kind {
                    if let Some(place) = fs.places.get(&id) {
                        return Place::Indirect(place.clone());
                    }
                }
                let pointer = self.value(fs, func, inner);
                Place::Indirect(self.fresh_place(fs, pointer, inner.ty, expr.span))
            }
            HirExprKind::Field { base, index } => {
                let base_place = if base.is_place() {
                    self.place_of(fs, func, base)
                } else {
                    let operand = self.value(fs, func, base);
                    Place::Direct(self.spill(fs, operand, base.ty, base.span))
                };
                let (name, _) = self.field_name(base.ty.id, *index);
                self.field_place(fs, &base_place, *index, &name, expr.ty, expr.span)
            }
            _ => {
                let operand = self.value(fs, func, expr);
                Place::Direct(self.spill(fs, operand, expr.ty, expr.span))
            }
        }
    }

    /// The place of field `index` of the aggregate at `base`.
    fn field_place(
        &mut self,
        fs: &mut Fs,
        base: &Place,
        index: u32,
        name: &str,
        ty: TypeRef,
        span: Span,
    ) -> Place {
        // A field that is itself a reference has a pointer-to-pointer address,
        // which LIR spells as the opaque `ref #unknown`.
        let pointer_ty = if ty.is_ref {
            LirTy::opaque_ref()
        } else {
            self.lir_ty(ty).to_ref()
        };
        let reg = fs.reg();
        fs.emit(LirInst::FieldAddr {
            obj: base.name().to_owned(),
            name: name.to_owned(),
            idx: index,
            dst: reg.clone(),
            ty: pointer_ty.clone(),
            span,
        });
        let holder = fs.tmp_lir(pointer_ty, span);
        fs.emit(LirInst::CopyPlace {
            src: LirOperand::Reg(reg),
            dst: holder.clone(),
            span,
        });
        Place::Indirect(holder)
    }

    /// A fresh `ref T` place holding the address of `place`.
    fn ref_place(&mut self, fs: &mut Fs, place: &Place, ty: TypeRef, span: Span) -> String {
        let pointer = self.lir_ty(ty).to_ref();
        let reg = fs.reg();
        match place {
            Place::Direct(name) => fs.emit(LirInst::Addr {
                place: name.clone(),
                dst: reg.clone(),
                ty: pointer.clone(),
                span,
            }),
            Place::Indirect(name) => fs.emit(LirInst::CopyReg {
                src: LirOperand::Place(name.clone()),
                dst: reg.clone(),
                ty: pointer.clone(),
                span,
            }),
        }
        let holder = fs.tmp_lir(pointer, span);
        fs.emit(LirInst::CopyPlace {
            src: LirOperand::Reg(reg),
            dst: holder.clone(),
            span,
        });
        holder
    }

    /// Stores an operand into a new temp place (registered as owned).
    fn spill(&mut self, fs: &mut Fs, operand: LirOperand, ty: TypeRef, span: Span) -> String {
        if let LirOperand::Place(place) = &operand {
            if is_fresh(place) {
                return place.clone();
            }
        }
        let place = self.tmp(fs, ty, span);
        fs.emit(LirInst::CopyPlace {
            src: operand,
            dst: place.clone(),
            span,
        });
        place
    }

    /// An operand as a fresh place the caller owns (not registered).
    fn fresh_place(&mut self, fs: &mut Fs, operand: LirOperand, ty: TypeRef, span: Span) -> String {
        if let LirOperand::Place(place) = &operand {
            if is_fresh(place) {
                Self::take_temp(fs, place);
                return place.clone();
            }
        }
        let place = self.tmp(fs, ty, span);
        fs.emit(LirInst::CopyPlace {
            src: operand,
            dst: place.clone(),
            span,
        });
        place
    }

    // ---- copies and drops ---------------------------------------------------------

    /// A copy of the value at `place` (copy hooks applied).
    fn copy_out(&mut self, fs: &mut Fs, place: &Place, ty: TypeRef, span: Span) -> LirOperand {
        let lir = self.lir_ty(ty);
        if !self.needs_copy(ty) {
            return fs.read_place(place, &lir, span);
        }
        let source = self.ref_place(fs, place, ty, span);
        let out = self.tmp(fs, ty, span);
        let routine = self.copy_routine(ty);
        fs.emit(LirInst::Call {
            func: LirCallee::Func(routine),
            ins: vec![source],
            outs: vec![out.clone()],
            span,
        });
        LirOperand::Place(out)
    }

    /// Drops the value at `place`.
    fn drop_place(&mut self, fs: &mut Fs, place: &Place, ty: TypeRef, span: Span) {
        if !self.needs_drop(ty) {
            return;
        }
        let pointer = self.ref_place(fs, place, ty, span);
        let routine = self.drop_routine(ty);
        fs.emit(LirInst::Call {
            func: LirCallee::Func(routine),
            ins: vec![pointer],
            outs: Vec::new(),
            span,
        });
        if let Place::Direct(name) = place {
            fs.emit(LirInst::Drop {
                place: name.clone(),
                span,
            });
        }
    }

    /// The function dropping a `T` through a `ref T`: its `@on_drop` when no
    /// field needs dropping, else generated glue.
    fn drop_routine(&mut self, ty: TypeRef) -> String {
        if let Some(name) = self.drop_glue.get(&ty.id) {
            return name.clone();
        }
        let hook = self
            .hir
            .hooks
            .get(&ty.id)
            .and_then(|hooks| hooks.drop.clone());
        let slots = self.slot_types(ty.id);
        let fields_drop = slots.iter().any(|slot| self.needs_drop(*slot));
        if let (Some(hook), false) = (&hook, fields_drop) {
            let symbol = self.symbol(hook);
            self.drop_glue.insert(ty.id, symbol.clone());
            return symbol;
        }
        let name = format!(".lilac.drop.{}", self.type_name(ty.id));
        self.drop_glue.insert(ty.id, name.clone());
        let mut fs = Fs::new();
        let lir = self.lir_ty(ty);
        fs.ins.push(("self".to_owned(), lir.clone().to_ref()));
        fs.place_tys.insert("self".to_owned(), lir.clone().to_ref());
        fs.used.insert("self".to_owned());
        let span = Span::dummy();
        let this = Place::Indirect("self".to_owned());
        if let Some(hook) = hook {
            let symbol = self.symbol(&hook);
            fs.emit(LirInst::Call {
                func: LirCallee::Func(symbol),
                ins: vec!["self".to_owned()],
                outs: Vec::new(),
                span,
            });
        }
        self.for_each_slot(&mut fs, &this, ty, |lower, fs, slot, slot_ty| {
            if lower.needs_drop(slot_ty) {
                lower.drop_place(fs, slot, slot_ty, span);
            }
        });
        let func = fs.finish(name.clone(), false, span);
        self.module.funcs.push(func);
        name
    }

    /// The function copying a `T` from a `ref T` into an out place: its
    /// `@on_copy`, else glue copying field by field.
    fn copy_routine(&mut self, ty: TypeRef) -> String {
        if let Some(name) = self.copy_glue.get(&ty.id) {
            return name.clone();
        }
        if let Some(hook) = self
            .hir
            .hooks
            .get(&ty.id)
            .and_then(|hooks| hooks.copy.clone())
        {
            let symbol = self.symbol(&hook);
            self.copy_glue.insert(ty.id, symbol.clone());
            return symbol;
        }
        let name = format!(".lilac.copy.{}", self.type_name(ty.id));
        self.copy_glue.insert(ty.id, name.clone());
        let mut fs = Fs::new();
        let lir = self.lir_ty(ty);
        for (place, slot_ty) in [("from", lir.clone().to_ref()), ("to", lir.clone())] {
            fs.place_tys.insert(place.to_owned(), slot_ty.clone());
            fs.used.insert(place.to_owned());
        }
        fs.ins.push(("from".to_owned(), lir.clone().to_ref()));
        fs.outs.push(("to".to_owned(), lir.clone()));
        let span = Span::dummy();
        let whole = fs.read_place(&Place::Indirect("from".to_owned()), &lir, span);
        fs.emit(LirInst::CopyPlace {
            src: whole,
            dst: "to".to_owned(),
            span,
        });
        let from = Place::Indirect("from".to_owned());
        let to = Place::Direct("to".to_owned());
        let pairs = self.slot_pairs(ty);
        for (field, name, slot_ty, variant) in pairs {
            if !self.needs_copy(slot_ty) {
                continue;
            }
            let skip = variant.map(|variant| self.variant_guard(&mut fs, &from, ty, variant, span));
            let source = self.field_place(&mut fs, &from, field, &name, slot_ty, span);
            let copied = self.copy_out(&mut fs, &source, slot_ty, span);
            let target = self.field_place(&mut fs, &to, field, &name, slot_ty, span);
            fs.write(&target, copied, span);
            if let Some(skip) = skip {
                fs.branch(&skip, span);
                fs.at(skip);
            }
        }
        let func = fs.finish(name.clone(), false, span);
        self.module.funcs.push(func);
        name
    }

    /// Every slot of an aggregate: `(field index, name, type, variant)` where
    /// `variant` is the enum variant that slot belongs to.
    fn slot_pairs(&self, ty: TypeRef) -> Vec<(u32, String, TypeRef, Option<u32>)> {
        match self.layout(ty.id) {
            Some(Layout::Object { fields }) => fields
                .iter()
                .enumerate()
                .map(|(index, (name, ty))| {
                    (
                        u32::try_from(index).unwrap_or(u32::MAX),
                        name.clone(),
                        *ty,
                        None,
                    )
                })
                .collect(),
            Some(Layout::Enum { variants, .. }) => {
                let mut out = Vec::new();
                for (variant, (name, payload)) in variants.iter().enumerate() {
                    if let Some(payload) = payload {
                        let variant = u32::try_from(variant).unwrap_or(u32::MAX);
                        let (field, _) = self.payload_field(ty.id, variant);
                        out.push((field, name.clone(), *payload, Some(variant)));
                    }
                }
                out
            }
            None => Vec::new(),
        }
    }

    /// Branches past the following code unless `place` holds `variant`;
    /// returns the label to continue at.
    fn variant_guard(
        &mut self,
        fs: &mut Fs,
        place: &Place,
        ty: TypeRef,
        variant: u32,
        span: Span,
    ) -> String {
        let test = self.pattern_test(fs, place, ty, true, &HirPattern::Variant(variant), span);
        let (body, skip) = (fs.label(), fs.label());
        fs.end(LirTerm::BranchIf {
            cond: test,
            then: body.clone(),
            els: Some(skip.clone()),
            span,
        });
        fs.at(body);
        skip
    }

    /// Runs `f` on every slot place of the aggregate at `place` (enum payloads
    /// only when live).
    fn for_each_slot(
        &mut self,
        fs: &mut Fs,
        place: &Place,
        ty: TypeRef,
        mut f: impl FnMut(&mut Self, &mut Fs, &Place, TypeRef),
    ) {
        for (field, name, slot_ty, variant) in self.slot_pairs(ty) {
            if !self.needs_drop(slot_ty) {
                continue;
            }
            let skip =
                variant.map(|variant| self.variant_guard(fs, place, ty, variant, Span::dummy()));
            let slot = self.field_place(fs, place, field, &name, slot_ty, Span::dummy());
            f(self, fs, &slot, slot_ty);
            if let Some(skip) = skip {
                fs.branch(&skip, Span::dummy());
                fs.at(skip);
            }
        }
    }

    // ---- values --------------------------------------------------------------------

    /// Evaluates an expression into a value that moves into a new owner:
    /// places are copied (hooks applied), fresh values are taken.
    fn read(&mut self, fs: &mut Fs, func: &HirFunc, expr: &HirExpr) -> LirOperand {
        if expr.is_place() && self.needs_copy(expr.ty) {
            let place = self.place_of(fs, func, expr);
            return self.copy_out(fs, &place, expr.ty, expr.span);
        }
        let operand = self.value(fs, func, expr);
        if let LirOperand::Place(place) = &operand {
            if is_fresh(place) {
                Self::take_temp(fs, place);
            }
        }
        operand
    }

    /// `read` into a fresh place (call arguments).
    fn arg(&mut self, fs: &mut Fs, func: &HirFunc, expr: &HirExpr) -> String {
        let operand = self.read(fs, func, expr);
        self.fresh_place(fs, operand, expr.ty, expr.span)
    }

    fn value_reg(&mut self, fs: &mut Fs, func: &HirFunc, expr: &HirExpr) -> String {
        let operand = self.value(fs, func, expr);
        let lir = self.lir_ty(expr.ty);
        fs.reg_of(operand, &lir, expr.span)
    }

    /// Registers for several operands, spilling earlier ones when a later one
    /// branches (registers do not cross blocks).
    fn regs(&mut self, fs: &mut Fs, func: &HirFunc, exprs: &[HirExpr]) -> Vec<String> {
        let mut operands = Vec::with_capacity(exprs.len());
        for (index, expr) in exprs.iter().enumerate() {
            let operand = self.value(fs, func, expr);
            let later_branches = exprs[index + 1..].iter().any(|later| self.branches(later));
            let operand = match operand {
                LirOperand::Reg(_) if later_branches => {
                    let place = self.tmp(fs, expr.ty, expr.span);
                    fs.emit(LirInst::CopyPlace {
                        src: operand,
                        dst: place.clone(),
                        span: expr.span,
                    });
                    LirOperand::Place(place)
                }
                other => other,
            };
            operands.push(operand);
        }
        operands
            .into_iter()
            .zip(exprs)
            .map(|(operand, expr)| {
                let lir = self.lir_ty(expr.ty);
                fs.reg_of(operand, &lir, expr.span)
            })
            .collect()
    }

    /// Whether lowering `expr` creates blocks.
    fn branches(&self, expr: &HirExpr) -> bool {
        let mut found = false;
        expr.walk(&mut |e| match &e.kind {
            HirExprKind::If { .. } | HirExprKind::Match { .. } | HirExprKind::Logic { .. } => {
                found = true;
            }
            HirExprKind::Const(def) => {
                if let Some(value) = self.hir.consts.get(def) {
                    found |= self.branches(value);
                }
            }
            _ => {}
        });
        found
    }

    /// Evaluates an expression without taking ownership: a fresh aggregate
    /// value stays registered for its statement's drop.
    #[allow(clippy::too_many_lines)]
    fn value(&mut self, fs: &mut Fs, func: &HirFunc, expr: &HirExpr) -> LirOperand {
        let span = expr.span;
        let lir = self.lir_ty(expr.ty);
        match &expr.kind {
            HirExprKind::Local(id) => match fs.places.get(id) {
                Some(place) => LirOperand::Place(place.clone()),
                None => lilac_error::bug!(span, "use of an undeclared local"),
            },
            HirExprKind::Field { base, index } if base.is_place() => {
                let base_place = self.place_of(fs, func, base);
                let (name, _) = self.field_name(base.ty.id, *index);
                let reg = fs.reg();
                fs.emit(LirInst::Field {
                    obj: base_place.name().to_owned(),
                    name,
                    idx: *index,
                    dst: reg.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            HirExprKind::Field { .. } | HirExprKind::Deref(_) => {
                let place = self.place_of(fs, func, expr);
                fs.read_place(&place, &lir, span)
            }
            HirExprKind::Number(bytes) => {
                let reg = fs.reg();
                fs.emit(LirInst::Const {
                    dst: reg.clone(),
                    lit: bytes.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            HirExprKind::Str(bytes) => {
                let name = self.intern_data(bytes);
                let reg = fs.reg();
                fs.emit(LirInst::Const {
                    dst: reg.clone(),
                    lit: format!("!{name}"),
                    ty: LirTy::byte_buffer(),
                    span,
                });
                LirOperand::Reg(reg)
            }
            HirExprKind::Struct(values) => {
                let place = self.tmp(fs, expr.ty, span);
                fs.emit(LirInst::Zero {
                    place: place.clone(),
                    span,
                });
                for (index, value) in values.iter().enumerate() {
                    let index = u32::try_from(index).unwrap_or(u32::MAX);
                    let (name, _) = self.field_name(expr.ty.id, index);
                    let operand = self.read(fs, func, value);
                    fs.emit(LirInst::StoreField {
                        value: operand,
                        obj: place.clone(),
                        name,
                        idx: index,
                        span: value.span,
                    });
                }
                self.own_temp(fs, &place, expr.ty);
                LirOperand::Place(place)
            }
            HirExprKind::Variant { index, payload } => {
                let place = self.tmp(fs, expr.ty, span);
                fs.emit(LirInst::Zero {
                    place: place.clone(),
                    span,
                });
                let Some(Layout::Enum { tag, .. }) = self.layout(expr.ty.id) else {
                    lilac_error::bug!(span, "variant of a non-enum");
                };
                let tag_ty = self.lir_ty(TypeRef::value(*tag));
                let reg = fs.reg();
                fs.emit(LirInst::Const {
                    dst: reg.clone(),
                    lit: format!("0x{index:x}"),
                    ty: tag_ty,
                    span,
                });
                fs.emit(LirInst::StoreField {
                    value: LirOperand::Reg(reg),
                    obj: place.clone(),
                    name: "tag".to_owned(),
                    idx: 0,
                    span,
                });
                if let Some(payload) = payload {
                    let (field, name) = self.payload_field(expr.ty.id, *index);
                    let operand = self.read(fs, func, payload);
                    fs.emit(LirInst::StoreField {
                        value: operand,
                        obj: place.clone(),
                        name,
                        idx: field,
                        span,
                    });
                }
                self.own_temp(fs, &place, expr.ty);
                LirOperand::Place(place)
            }
            HirExprKind::Call { callee, args } => {
                if let HirCallee::Intrinsic(intrinsic) = callee {
                    return self.lower_intrinsic(fs, func, *intrinsic, args, expr.ty, span);
                }
                if expr.ty.is_error() {
                    self.emit_call(fs, func, callee, args, &[], span);
                    return LirOperand::Reg(String::new());
                }
                let out = self.tmp(fs, expr.ty, span);
                self.emit_call(fs, func, callee, args, std::slice::from_ref(&out), span);
                self.own_temp(fs, &out, expr.ty);
                LirOperand::Place(out)
            }
            HirExprKind::FuncRef(key) => {
                let reg = fs.reg();
                fs.emit(LirInst::AddrFunc {
                    func: self.symbol(key),
                    dst: reg.clone(),
                    ty: LirTy::opaque_ref(),
                    span,
                });
                LirOperand::Reg(reg)
            }
            HirExprKind::RefOf(inner) => {
                let place = self.place_of(fs, func, inner);
                let reg = fs.reg();
                match &place {
                    Place::Direct(name) => fs.emit(LirInst::Addr {
                        place: name.clone(),
                        dst: reg.clone(),
                        ty: lir,
                        span,
                    }),
                    Place::Indirect(name) => fs.emit(LirInst::CopyReg {
                        src: LirOperand::Place(name.clone()),
                        dst: reg.clone(),
                        ty: lir,
                        span,
                    }),
                }
                LirOperand::Reg(reg)
            }
            HirExprKind::Retype(inner) => {
                let operand = self.value(fs, func, inner);
                let reg = fs.reg();
                fs.emit(LirInst::CopyReg {
                    src: operand,
                    dst: reg.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            HirExprKind::Eq(lhs, rhs) => {
                let regs = self.regs(fs, func, &[(**lhs).clone(), (**rhs).clone()]);
                let reg = fs.reg();
                fs.emit(LirInst::Arith {
                    op: ArithOp::Eq,
                    lhs: regs[0].clone(),
                    rhs: regs[1].clone(),
                    dst: reg.clone(),
                    ty: LirTy::Bit(8),
                    span,
                });
                LirOperand::Reg(reg)
            }
            HirExprKind::If { cond, then, els } => {
                let result = self.tmp(fs, expr.ty, span);
                if let Some(taken) = self.constant(cond).and_then(|value| value.truthy()) {
                    self.region(fs, func, if taken { then } else { els }, &result);
                    self.own_temp(fs, &result, expr.ty);
                    return LirOperand::Place(result);
                }
                let cond = self.value_reg(fs, func, cond);
                let (then_label, els_label, join) = (fs.label(), fs.label(), fs.label());
                fs.end(LirTerm::BranchIf {
                    cond,
                    then: then_label.clone(),
                    els: Some(els_label.clone()),
                    span,
                });
                for (label, branch) in [(then_label, then), (els_label, els)] {
                    fs.at(label);
                    self.region(fs, func, branch, &result);
                    fs.branch(&join, span);
                }
                fs.at(join);
                self.own_temp(fs, &result, expr.ty);
                LirOperand::Place(result)
            }
            HirExprKind::Match {
                scrutinee,
                arms,
                els,
            } => self.lower_match_value(fs, func, expr, scrutinee, arms, els),
            HirExprKind::Logic { and, lhs, rhs } => {
                let result = self.tmp(fs, expr.ty, span);
                if let Some(left) = self.constant(lhs).and_then(|value| value.truthy()) {
                    if left == *and {
                        self.region(fs, func, rhs, &result);
                    } else {
                        let reg = fs.reg();
                        fs.emit(LirInst::Const {
                            dst: reg.clone(),
                            lit: if *and { "0x0" } else { "0x1" }.to_owned(),
                            ty: LirTy::Bit(8),
                            span,
                        });
                        fs.emit(LirInst::CopyPlace {
                            src: LirOperand::Reg(reg),
                            dst: result.clone(),
                            span,
                        });
                    }
                    return LirOperand::Place(result);
                }
                let left = self.value_reg(fs, func, lhs);
                let (rhs_label, short_label, join) = (fs.label(), fs.label(), fs.label());
                let (then, els) = if *and {
                    (rhs_label.clone(), short_label.clone())
                } else {
                    (short_label.clone(), rhs_label.clone())
                };
                fs.end(LirTerm::BranchIf {
                    cond: left,
                    then,
                    els: Some(els),
                    span,
                });
                fs.at(rhs_label);
                self.region(fs, func, rhs, &result);
                fs.branch(&join, span);
                fs.at(short_label);
                let reg = fs.reg();
                fs.emit(LirInst::Const {
                    dst: reg.clone(),
                    lit: if *and { "0x0" } else { "0x1" }.to_owned(),
                    ty: LirTy::Bit(8),
                    span,
                });
                fs.emit(LirInst::CopyPlace {
                    src: LirOperand::Reg(reg),
                    dst: result.clone(),
                    span,
                });
                fs.branch(&join, span);
                fs.at(join);
                LirOperand::Place(result)
            }
            HirExprKind::FormatArgs(pieces) => {
                let place = self.tmp(fs, expr.ty, span);
                fs.emit(LirInst::Call {
                    func: LirCallee::Func(".lilac.format_args_new".to_owned()),
                    ins: Vec::new(),
                    outs: vec![place.clone()],
                    span,
                });
                for piece in pieces {
                    let piece_place = self.arg(fs, func, piece);
                    let table = self.ref_place(fs, &Place::Direct(place.clone()), expr.ty, span);
                    fs.emit(LirInst::Call {
                        func: LirCallee::Func(".lilac.format_args_push".to_owned()),
                        ins: vec![table, piece_place],
                        outs: Vec::new(),
                        span: piece.span,
                    });
                }
                LirOperand::Place(place)
            }
            HirExprKind::Const(def) => match self.hir.consts.get(def) {
                Some(value) => self.value(fs, func, value),
                None => lilac_error::bug!(span, "use of an unchecked constant"),
            },
            HirExprKind::SizeOf(ty) => {
                let lir_ty = self.lir_ty(*ty);
                let (size, _) = lilac_ir::size_align(&self.module.layouts, &lir_ty, self.ptr_bytes);
                let reg = fs.reg();
                fs.emit(LirInst::Const {
                    dst: reg.clone(),
                    lit: format!("0x{size:x}"),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            HirExprKind::CompileError { msg, msg_span } => {
                fs.emit(LirInst::CompileError {
                    msg: msg.clone(),
                    msg_span: *msg_span,
                    call_span: span,
                });
                let ty = if expr.ty.is_error() {
                    TypeRef::value(self.types.builtin("@bit8").unwrap_or(TypeId::ERROR))
                } else {
                    expr.ty
                };
                LirOperand::Place(self.tmp(fs, ty, span))
            }
            HirExprKind::Poison => lilac_error::bug!(span, "lowering poisoned HIR"),
        }
    }

    /// Evaluates `value` in its own scope and moves it into `result`.
    fn region(&mut self, fs: &mut Fs, func: &HirFunc, value: &HirExpr, result: &str) {
        fs.scopes.push(Scope::default());
        let operand = self.read(fs, func, value);
        fs.emit(LirInst::CopyPlace {
            src: operand,
            dst: result.to_owned(),
            span: value.span,
        });
        let index = fs.scopes.len() - 1;
        self.drop_scope(fs, index, value.span);
        fs.scopes.pop();
    }

    fn lower_match_value(
        &mut self,
        fs: &mut Fs,
        func: &HirFunc,
        expr: &HirExpr,
        scrutinee: &HirExpr,
        arms: &[HirValueArm],
        els: &HirExpr,
    ) -> LirOperand {
        let span = expr.span;
        let result = self.tmp(fs, expr.ty, span);
        if let Some((choice, _)) =
            self.constant_choice(scrutinee, arms.iter().map(|arm| &arm.pattern))
        {
            // A constant scrutinee lowers only the chosen arm.
            match choice.map(|index| &arms[index]) {
                Some(arm) => {
                    fs.scopes.push(Scope::default());
                    if let Some(capture) = arm.capture {
                        let (subject, _) = self.subject(fs, func, scrutinee);
                        self.bind_capture(fs, func, &subject, scrutinee.ty, &arm.pattern, capture);
                    }
                    let operand = self.read(fs, func, &arm.value);
                    fs.emit(LirInst::CopyPlace {
                        src: operand,
                        dst: result.clone(),
                        span,
                    });
                    let scope = fs.scopes.len() - 1;
                    self.drop_scope(fs, scope, arm.value.span);
                    fs.scopes.pop();
                }
                None => self.region(fs, func, els, &result),
            }
            self.own_temp(fs, &result, expr.ty);
            return LirOperand::Place(result);
        }
        let (subject, is_enum) = self.subject(fs, func, scrutinee);
        let join = fs.label();
        let els_label = fs.label();
        let mut next = fs.label();
        fs.branch(&next, span);
        for (index, arm) in arms.iter().enumerate() {
            fs.at(next);
            let body = fs.label();
            next = if index + 1 == arms.len() {
                els_label.clone()
            } else {
                fs.label()
            };
            let test = self.pattern_test(fs, &subject, scrutinee.ty, is_enum, &arm.pattern, span);
            fs.end(LirTerm::BranchIf {
                cond: test,
                then: body.clone(),
                els: Some(next.clone()),
                span,
            });
            fs.at(body);
            fs.scopes.push(Scope::default());
            if let Some(capture) = arm.capture {
                self.bind_capture(fs, func, &subject, scrutinee.ty, &arm.pattern, capture);
            }
            let operand = self.read(fs, func, &arm.value);
            fs.emit(LirInst::CopyPlace {
                src: operand,
                dst: result.clone(),
                span,
            });
            let scope = fs.scopes.len() - 1;
            self.drop_scope(fs, scope, arm.value.span);
            fs.scopes.pop();
            fs.branch(&join, span);
        }
        if arms.is_empty() {
            fs.at(next);
            fs.branch(&els_label, span);
        }
        fs.at(els_label);
        self.region(fs, func, els, &result);
        fs.branch(&join, span);
        fs.at(join);
        self.own_temp(fs, &result, expr.ty);
        LirOperand::Place(result)
    }

    fn intern_data(&mut self, bytes: &[u8]) -> String {
        if let Some(existing) = self.module.data.iter().find(|data| data.bytes == bytes) {
            return existing.name.clone();
        }
        let name = format!("c{}", self.module.data.len());
        self.module.data.push(LirData {
            name: name.clone(),
            bytes: bytes.to_vec(),
            span: Span::dummy(),
        });
        name
    }

    // ---- calls ----------------------------------------------------------------------

    fn emit_call(
        &mut self,
        fs: &mut Fs,
        func: &HirFunc,
        callee: &HirCallee,
        args: &[HirExpr],
        outs: &[String],
        span: Span,
    ) {
        if let HirCallee::Func(key) = callee {
            if self.externs.contains(key) {
                self.emit_extern_call(fs, func, key, args, outs, span);
                return;
            }
        }
        let ins: Vec<String> = args.iter().map(|arg| self.arg(fs, func, arg)).collect();
        let target = match callee {
            HirCallee::Func(key) => LirCallee::Func(self.symbol(key)),
            HirCallee::Local(id) => {
                let Some(place) = fs.places.get(id).cloned() else {
                    lilac_error::bug!(span, "call through an undeclared local");
                };
                let reg = fs.reg();
                fs.emit(LirInst::CopyReg {
                    src: LirOperand::Place(place),
                    dst: reg.clone(),
                    ty: LirTy::opaque_ref(),
                    span,
                });
                LirCallee::Reg(reg)
            }
            HirCallee::Intrinsic(_) => {
                lilac_error::bug!(span, "intrinsic in the general call path")
            }
        };
        fs.emit(LirInst::Call {
            func: target,
            ins,
            outs: outs.to_vec(),
            span,
        });
    }

    /// A C ABI call: wrapper objects pass (and return) their single field.
    fn emit_extern_call(
        &mut self,
        fs: &mut Fs,
        func: &HirFunc,
        key: &FuncKey,
        args: &[HirExpr],
        outs: &[String],
        span: Span,
    ) {
        let mut ins = Vec::with_capacity(args.len());
        for arg in args {
            let place = self.arg(fs, func, arg);
            let (abi, wrapped) = self.abi_ty(arg.ty);
            if wrapped {
                let (name, _) = self.field_name(arg.ty.id, 0);
                let reg = fs.reg();
                fs.emit(LirInst::Field {
                    obj: place,
                    name,
                    idx: 0,
                    dst: reg.clone(),
                    ty: abi.clone(),
                    span,
                });
                let inner = fs.tmp_lir(abi, span);
                fs.emit(LirInst::CopyPlace {
                    src: LirOperand::Reg(reg),
                    dst: inner.clone(),
                    span,
                });
                ins.push(inner);
            } else {
                ins.push(place);
            }
        }
        let symbol = self.symbol(key);
        let returns: Vec<TypeRef> = self
            .hir
            .funcs
            .iter()
            .find(|candidate| candidate.key == *key)
            .map(|callee| {
                callee
                    .returns
                    .iter()
                    .map(|id| callee.local(*id).ty)
                    .collect()
            })
            .unwrap_or_default();
        let mut call_outs = Vec::with_capacity(outs.len());
        let mut rewrap = Vec::new();
        for (out, ty) in outs.iter().zip(&returns) {
            let (abi, wrapped) = self.abi_ty(*ty);
            if wrapped {
                let inner = fs.tmp_lir(abi, span);
                rewrap.push((out.clone(), inner.clone(), *ty));
                call_outs.push(inner);
            } else {
                call_outs.push(out.clone());
            }
        }
        fs.emit(LirInst::Call {
            func: LirCallee::Func(symbol),
            ins,
            outs: call_outs,
            span,
        });
        for (out, inner, ty) in rewrap {
            let (name, _) = self.field_name(ty.id, 0);
            fs.emit(LirInst::StoreField {
                value: LirOperand::Place(inner),
                obj: out,
                name,
                idx: 0,
                span,
            });
        }
    }

    #[allow(clippy::too_many_lines)]
    fn lower_intrinsic(
        &mut self,
        fs: &mut Fs,
        func: &HirFunc,
        intrinsic: Intrinsic,
        args: &[HirExpr],
        ty: TypeRef,
        span: Span,
    ) -> LirOperand {
        let lir = self.lir_ty(ty);
        match intrinsic {
            Intrinsic::Int(NumOp::Negate, _) | Intrinsic::Float(NumOp::Negate, _) => {
                let float = matches!(intrinsic, Intrinsic::Float(..));
                let operand = self.value_reg(fs, func, &args[0]);
                let zero = fs.reg();
                fs.emit(LirInst::Const {
                    dst: zero.clone(),
                    lit: if float { "0.0" } else { "0x0" }.to_owned(),
                    ty: lir.clone(),
                    span,
                });
                let reg = fs.reg();
                fs.emit(LirInst::Arith {
                    op: if float { ArithOp::FSub } else { ArithOp::ISub },
                    lhs: zero,
                    rhs: operand,
                    dst: reg.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            Intrinsic::Int(op, _) | Intrinsic::Float(op, _) => {
                let float = matches!(intrinsic, Intrinsic::Float(..));
                let Some(op) = constant::arith_op(op, float) else {
                    lilac_error::bug!(span, "intrinsic without an IR operation");
                };
                let regs = self.regs(fs, func, args);
                let reg = fs.reg();
                fs.emit(LirInst::Arith {
                    op,
                    lhs: regs[0].clone(),
                    rhs: regs[1].clone(),
                    dst: reg.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            Intrinsic::FbitToBits(_)
            | Intrinsic::BitsToFbit(_)
            | Intrinsic::Truncate
            | Intrinsic::ExtendZero => {
                let op = match intrinsic {
                    Intrinsic::Truncate => ConvertOp::Trunc,
                    Intrinsic::ExtendZero => ConvertOp::ExtendZero,
                    _ => ConvertOp::Bitcast,
                };
                let src = self.value_reg(fs, func, &args[0]);
                let reg = fs.reg();
                fs.emit(LirInst::Convert {
                    op,
                    src,
                    dst: reg.clone(),
                    ty: lir,
                    span,
                });
                LirOperand::Reg(reg)
            }
            Intrinsic::Memcpy => {
                let ins = args.iter().map(|arg| self.arg(fs, func, arg)).collect();
                fs.emit(LirInst::Call {
                    func: LirCallee::Builtin("@memcpy".to_owned()),
                    ins,
                    outs: Vec::new(),
                    span,
                });
                LirOperand::Reg(String::new())
            }
            Intrinsic::Panic => {
                let msg = self.arg(fs, func, &args[0]);
                let location = self.sources.location(span);
                let site = lilac_ir::trace::site_text(&self.display_of(func), location.as_deref());
                let name = self.intern_data(site.as_bytes());
                let reg = fs.reg();
                fs.emit(LirInst::Const {
                    dst: reg.clone(),
                    lit: format!("!{name}"),
                    ty: LirTy::byte_buffer(),
                    span,
                });
                let site = self.fresh_place(fs, LirOperand::Reg(reg), args[0].ty, span);
                fs.emit(LirInst::Call {
                    func: LirCallee::Func(".lilac.panic".to_owned()),
                    ins: vec![msg, site],
                    outs: Vec::new(),
                    span,
                });
                fs.end(LirTerm::Unreachable { span });
                LirOperand::Reg(String::new())
            }
            _ => {
                let Some(symbol) = intrinsic.runtime_symbol() else {
                    lilac_error::bug!(span, "intrinsic `{intrinsic:?}` has no lowering");
                };
                let ins = args.iter().map(|arg| self.arg(fs, func, arg)).collect();
                if ty.is_error() {
                    fs.emit(LirInst::Call {
                        func: LirCallee::Func(symbol.to_owned()),
                        ins,
                        outs: Vec::new(),
                        span,
                    });
                    return LirOperand::Reg(String::new());
                }
                let out = self.tmp(fs, ty, span);
                fs.emit(LirInst::Call {
                    func: LirCallee::Func(symbol.to_owned()),
                    ins,
                    outs: vec![out.clone()],
                    span,
                });
                LirOperand::Place(out)
            }
        }
    }
}

#[cfg(test)]
mod tests;
