//! HIR -> LIR lowering: places, ownership, and control flow.
//!
//! Ownership is decided by typeck's ownership pass, and lowering follows it:
//!
//! - A [`lilac_hir::HirExprKind::Move`] reads its place bit for bit. Reading
//!   any other *place* (local, field, dereference) into a new owner copies
//!   it: the type's `@on_copy` hook, or copy glue when only its fields have
//!   hooks, or a plain bit copy. Every other expression is a fresh value
//!   and moves.
//! - Locals drop where a [`HirStmt::Drop`] says; a fresh value nobody takes
//!   is dropped at the end of its statement (or by the early exit leaving
//!   it). A drop flag ([`HirFunc::flags`]) is set when its place is
//!   initialized and cleared when it is moved out.
//! - Dropping runs the type's `@on_drop`, then drops every field (drop glue).
//! - Assignment evaluates the new value, handles the old one as its
//!   [`lilac_hir::HirOld`] says (`@on_override(ref old, ref new)`, then a
//!   drop, for a full place), then moves the new one in.
//!
//! Arguments always reach the callee in a fresh place (the callee owns and may
//! modify its parameters). Every function zeroes its return slots on entry:
//! storage is zero-filled, but an out is empty until it is set.

mod calls;
mod constant;
mod control;
mod expr;
mod globals;
mod intrinsics;
mod ownership;
mod symbols;

pub use symbols::check_extern_signatures;
use symbols::extern_symbol;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use lilac_error::{codes, Diagnostic, Diags};
use lilac_hir::{FuncKey, HirFunc, HirPath, HirProgram, HirStmt, Layout, LocalId};
use lilac_ir::{
    ConvertOp, Label, LirBlock, LirConst, LirExport, LirFunc, LirInst, LirLayout, LirLayoutKind,
    LirModule, LirOperand, LirTerm, LirTy, PlaceName, Reg, Symbol,
};
use lilac_resolve::Resolved;
use lilac_span::{BytePos, SourceMap, Span};
use lilac_target::Target;
use lilac_ty::{TypeId, TypeKind, TypeRef, Types};

/// Lowered LIR plus each function's source name.
pub struct Lowered {
    /// The module.
    pub module: LirModule,
    /// LIR symbol -> name as written in source (`Holder[@bit32].make`);
    /// stack traces print these.
    pub names: HashMap<String, String>,
    /// Constant `@array_at` indexes past the array's end, in lowering order
    /// ([`check_constant_indices`] reports them).
    pub bad_indices: Vec<BadIndex>,
    /// Integer divisions and remainders that panic whatever runs before
    /// them, in lowering order ([`check_constant_divisions`] reports them).
    pub bad_divisions: Vec<BadDivision>,
    /// Global initializers that are not constant expressions
    /// ([`check_global_initializers`] reports them).
    pub bad_globals: Vec<Span>,
}

/// A constant `@array_at` index that is out of bounds: `E037`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadIndex {
    /// LIR symbol of the function holding the access.
    pub func: String,
    /// The index argument.
    pub span: Span,
    /// The index's value.
    pub index: u64,
    /// The array's length.
    pub len: u64,
}

/// Why an integer division or remainder panics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DivisionPanic {
    /// The divisor is zero.
    ByZero,
    /// A signed `MIN` divided by `-1`: the quotient does not fit.
    Overflow,
}

impl DivisionPanic {
    /// The panic message, also the `E060` message.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::ByZero => "division by zero",
            Self::Overflow => "division overflow",
        }
    }

    /// The `E060` label at the operation.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ByZero => "the divisor is zero",
            Self::Overflow => "the minimum signed value divided by -1 does not fit",
        }
    }
}

/// A division or remainder in a live arm whose constant operands make it
/// panic: `E060`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadDivision {
    /// LIR symbol of the function holding it (`None` in a global's
    /// initializer).
    pub func: Option<String>,
    /// The intrinsic call, or the `=>` call whose body divides.
    pub span: Span,
    /// Why it panics.
    pub panic: DivisionPanic,
}

/// Reports every [`Lowered::bad_divisions`] entry as `E060`, traced like
/// [`check_constant_indices`]: through every caller, live or not. Run it
/// before DCE.
pub fn check_constant_divisions(lowered: &Lowered, diags: &mut Diags) {
    if lowered.bad_divisions.is_empty() {
        return;
    }
    let graph = lilac_ir::CallGraph::new(&lowered.module);
    for bad in &lowered.bad_divisions {
        let mut diagnostic = Diagnostic::error(codes::E060, bad.panic.message()).label(
            bad.span,
            true,
            bad.panic.label(),
        );
        if let Some(func) = &bad.func {
            diagnostic = diagnostic.trace(graph.trace(func, bad.span, &lowered.names));
        }
        diags.emit(diagnostic);
    }
}

/// Reports every [`Lowered::bad_indices`] entry as `E037`, traced through
/// every caller in `lowered`'s module, live or not: a constant index is
/// wrong whether or not anything calls the function yet. Run it before DCE.
/// It only emits, so the live checks after DCE still report too.
pub fn check_constant_indices(lowered: &Lowered, diags: &mut Diags) {
    if lowered.bad_indices.is_empty() {
        return;
    }
    let graph = lilac_ir::CallGraph::new(&lowered.module);
    for bad in &lowered.bad_indices {
        diags.emit(
            Diagnostic::error(
                codes::E037,
                format!(
                    "index {} is out of bounds for an array of length {}",
                    bad.index, bad.len
                ),
            )
            .label(bad.span, true, format!("the last index is {}", bad.len - 1))
            .trace(graph.trace(&bad.func, bad.span, &lowered.names)),
        );
    }
}

/// Reports every [`Lowered::bad_globals`] entry as `E038`.
pub fn check_global_initializers(lowered: &Lowered, diags: &mut Diags) {
    for span in &lowered.bad_globals {
        diags.emit(
            Diagnostic::error(codes::E038, "a global's initializer must be constant")
                .label(*span, true, "not a constant expression")
                .help("assign it at run time instead, or leave the global zeroed: `name: Type`"),
        );
    }
}

/// Lowers checked, error-free HIR to LIR for `target` (which fixes every
/// layout and `@size_of`); `sources` locates `@panic` sites. When `cancel` is
/// set (a newer compile is due), lowering stops before the next function and
/// the module is incomplete.
#[must_use]
pub fn lower(
    hir: &HirProgram,
    res: &Resolved,
    types: &Types,
    sources: &SourceMap,
    target: Target,
    cancel: Option<&AtomicBool>,
) -> Lowered {
    let mut lower = Lower::new(hir, res, types, sources, target);
    lower.lower_globals();
    let mut externs = HashSet::new();
    for func in &hir.funcs {
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            break;
        }
        let lowered = lower.lower_func(func);
        // Externs sharing a link name are one declaration (their signatures
        // match: `check_extern_signatures` rejected any that differ).
        if lowered.is_extern && !externs.insert(lowered.name.clone()) {
            continue;
        }
        lower.module.funcs.push(lowered);
    }
    Lowered {
        module: lower.module,
        names: lower.names,
        bad_indices: lower.bad_indices,
        bad_divisions: lower.bad_divisions,
        bad_globals: lower.bad_globals,
    }
}

/// The value of each selector `lilac_typeck::check_selectors` checked
/// (the index of a top-level conditional's selected arm), or `None` when
/// it failed to check or is not constant (`E039`).
#[must_use]
pub fn evaluate_selectors(
    hir: &HirProgram,
    res: &Resolved,
    types: &Types,
    sources: &SourceMap,
    target: Target,
    selectors: &[Option<lilac_hir::HirExpr>],
) -> Vec<Option<u64>> {
    let mut lower = Lower::new(hir, res, types, sources, target);
    selectors
        .iter()
        .map(|selector| match lower.constant(selector.as_ref()?)? {
            constant::Value::Int(bits, _) => Some(bits),
            _ => None,
        })
        .collect()
}

impl<'a> Lower<'a> {
    /// A lowering of `hir` with every function's symbol and every layout
    /// registered.
    fn new(
        hir: &'a HirProgram,
        res: &'a Resolved,
        types: &'a Types,
        sources: &'a SourceMap,
        target: Target,
    ) -> Self {
        let mut lower = Lower {
            hir,
            res,
            types,
            sources,
            names: HashMap::new(),
            bad_indices: Vec::new(),
            bad_divisions: Vec::new(),
            bad_globals: Vec::new(),
            target,
            module: LirModule::new(target),
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
            lower.symbols.insert(func.key.clone(), symbol.into());
        }
        let mut ids: Vec<TypeId> = hir.layouts.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            lower.lay_out(id);
        }
        lower
    }

    /// Adds the layout of type `id` to the module, computed by
    /// `lilac-layout` for the target, after the layouts of every type it
    /// holds by value (directly or as array elements), whose sizes it
    /// needs. Typeck rejected by-value cycles (`E053`) and types larger
    /// than the target allows (`E067`), so this always succeeds.
    fn lay_out(&mut self, id: TypeId) {
        let name = self.type_name(id);
        if self.module.layouts.contains_key(&name) {
            return;
        }
        let Some(layout) = self.hir.layouts.get(&id) else {
            return;
        };
        let slots = layout.slots();
        for (_, ty) in &slots {
            if !ty.is_ref {
                self.lay_out_held(ty.id);
            }
        }
        let fields = slots
            .iter()
            .map(|(name, ty)| (name.clone(), self.lir_ty(*ty)))
            .collect();
        let kind = match layout {
            Layout::Object { .. } => LirLayoutKind::Struct,
            Layout::Enum { .. } => LirLayoutKind::Tagged,
        };
        match LirLayout::new(kind, fields, &self.module.layouts, &self.target) {
            Ok(layout) => {
                self.module.layouts.insert(name, layout);
            }
            Err(error) => lilac_error::bug!(
                Span::dummy(),
                "type `{name}` has no layout on `{}`: {error}",
                self.target
            ),
        }
    }

    /// Lays out the named type `id` is or (as an array) holds.
    fn lay_out_held(&mut self, id: TypeId) {
        match self.types.kind(id) {
            TypeKind::Array { elem, .. } => self.lay_out_held(*elem),
            TypeKind::Object(_) | TypeKind::Instance { .. } => self.lay_out(id),
            _ => {}
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
    /// Constant out-of-bounds `@array_at` indexes (`E037`).
    bad_indices: Vec<BadIndex>,
    /// Divisions and remainders that panic on constant operands (`E060`).
    bad_divisions: Vec<BadDivision>,
    /// Global initializers that are not constant (`E038`).
    bad_globals: Vec<Span>,
    /// The target: every layout, and `@bit_native`'s width.
    target: Target,
    module: LirModule,
    /// LIR symbol of every function instance.
    symbols: HashMap<FuncKey, Symbol>,
    /// `extern` instances (C ABI calls).
    externs: HashSet<FuncKey>,
    /// Drop routine per type (a hook or generated glue).
    drop_glue: HashMap<TypeId, Symbol>,
    /// Copy routine per type (a hook or generated glue).
    copy_glue: HashMap<TypeId, Symbol>,
    needs_drop: HashMap<TypeId, bool>,
    needs_copy: HashMap<TypeId, bool>,
    /// Every function instance by key (constant `=>` calls).
    funcs: HashMap<FuncKey, &'a HirFunc>,
}

/// A place: a named LIR place, or a `ref` place holding the address of one.
#[derive(Clone, Debug)]
enum Place {
    Direct(PlaceName),
    Indirect(PlaceName),
}

impl Place {
    fn name(&self) -> &PlaceName {
        match self {
            Self::Direct(name) | Self::Indirect(name) => name,
        }
    }
}

/// Temporaries of one block (or expression region).
#[derive(Default)]
struct Scope {
    /// Fresh values nobody took yet (dropped at the end of their statement).
    temps: Vec<(PlaceName, TypeRef)>,
}

/// Per-function emission state.
struct FnState {
    ins: Vec<(PlaceName, LirTy)>,
    outs: Vec<(PlaceName, LirTy)>,
    blocks: Vec<LirBlock>,
    label: Label,
    insts: Vec<LirInst>,
    open: bool,
    labels: u32,
    regs: u32,
    tmps: u32,
    /// LIR place of every declared local.
    places: HashMap<LocalId, PlaceName>,
    /// Place names in use (shadowing gets `.N` suffixes).
    used: HashSet<PlaceName>,
    /// Hoisted entry-block allocations.
    allocas: Vec<LirInst>,
    /// Place types.
    place_tys: HashMap<PlaceName, LirTy>,
    scopes: Vec<Scope>,
    /// `(head label, exit label, scope depth)` per enclosing loop.
    loops: Vec<(Label, Label, usize)>,
}

impl FnState {
    fn tmp_lir(&mut self, lir: LirTy, span: Span) -> PlaceName {
        self.tmps += 1;
        let place = PlaceName::new(format!("tmp.{}", self.tmps));
        self.used.insert(place.clone());
        self.alloca(&place, lir, span);
        place
    }

    /// The value at `place`, read now: a direct place as itself; through a
    /// reference, a scalar is loaded into a register and an aggregate is
    /// copied into a fresh place (registers hold only scalars).
    fn read_place(&mut self, place: &Place, lir: &LirTy, span: Span) -> LirOperand {
        match place {
            Place::Direct(name) => LirOperand::Place(name.clone()),
            Place::Indirect(name) if lir.is_scalar() => {
                let reg = self.reg();
                self.emit(LirInst::Load {
                    src: name.clone(),
                    dst: reg.clone(),
                    ty: lir.clone(),
                    span,
                });
                LirOperand::Reg(reg)
            }
            Place::Indirect(name) => {
                let copy = self.tmp_lir(lir.clone(), span);
                self.emit(LirInst::CopyDeref {
                    src: name.clone(),
                    dst: copy.clone(),
                    span,
                });
                LirOperand::Place(copy)
            }
        }
    }

    /// Copies the value at `place` into `dst` ([`Self::read_place`] with
    /// no temporary in between for an aggregate behind a reference).
    fn read_into(&mut self, place: &Place, lir: &LirTy, dst: &PlaceName, span: Span) {
        match place {
            Place::Indirect(name) if !lir.is_scalar() => self.emit(LirInst::CopyDeref {
                src: name.clone(),
                dst: dst.clone(),
                span,
            }),
            _ => {
                let value = self.read_place(place, lir, span);
                self.emit(LirInst::CopyPlace {
                    src: value,
                    dst: dst.clone(),
                    span,
                });
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

    /// A scalar operand in a register (an aggregate never is one).
    fn reg_of(&mut self, operand: LirOperand, lir: &LirTy, span: Span) -> Reg {
        if !lir.is_scalar() {
            lilac_error::bug!(span, "the aggregate `{lir}` read into a register");
        }
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
            label: "entry".into(),
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
            self.label = Label::new(format!("l.{}", self.labels));
            self.open = true;
        }
        self.insts.push(inst);
    }

    /// A register holding the constant `value`.
    fn constant(&mut self, value: LirConst, ty: LirTy, span: Span) -> LirOperand {
        let reg = self.reg();
        self.emit(LirInst::Const {
            dst: reg.clone(),
            value,
            ty,
            span,
        });
        LirOperand::Reg(reg)
    }

    /// `convert bitcast` of `src` to `ty` into a fresh register.
    fn bitcast(&mut self, src: Reg, ty: LirTy, span: Span) -> Reg {
        let dst = self.reg();
        self.emit(LirInst::Convert {
            op: ConvertOp::Bitcast,
            src,
            dst: dst.clone(),
            ty,
            span,
        });
        dst
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

    fn at(&mut self, label: Label) {
        self.label = label;
        self.open = true;
    }

    fn label(&mut self) -> Label {
        self.labels += 1;
        Label::new(format!("l.{}", self.labels))
    }

    fn reg(&mut self) -> Reg {
        self.regs += 1;
        Reg::new(self.regs.to_string())
    }

    fn branch(&mut self, target: &Label, span: Span) {
        self.end(LirTerm::Branch {
            target: target.clone(),
            span,
        });
    }

    /// A fresh name for a place, avoiding collisions.
    fn place_name(&mut self, base: &str) -> PlaceName {
        let mut name = PlaceName::new(base);
        let mut counter = 0;
        while !self.used.insert(name.clone()) {
            counter += 1;
            name = PlaceName::new(format!("{base}.{counter}"));
        }
        name
    }

    fn alloca(&mut self, place: &PlaceName, ty: LirTy, span: Span) {
        self.place_tys.insert(place.clone(), ty.clone());
        self.allocas.push(LirInst::Alloca {
            ty,
            place: place.clone(),
            span,
        });
    }

    /// Closes the function; an open block falls off the end at `span`.
    fn finish(mut self, name: Symbol, inline: bool, span: Span) -> LirFunc {
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
            export: None,
            variadic: false,
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
        HirStmt::Assign { place, value, .. } if place.span.file == value.span.file => Span {
            lo: place.span.lo.min(value.span.lo),
            hi: place.span.hi.max(value.span.hi),
            file: place.span.file,
        },
        HirStmt::Let { value, .. } | HirStmt::Assign { value, .. } => value.span,
        HirStmt::Expr(expr) => expr.span,
        HirStmt::Return(span)
        | HirStmt::Fail(span)
        | HirStmt::Break(span)
        | HirStmt::Continue(span)
        | HirStmt::Drop { span, .. } => *span,
        HirStmt::If { cond, .. } => cond.span,
        HirStmt::Loop(block) | HirStmt::Block(block) => block.span,
        HirStmt::Match { scrutinee, .. } => scrutinee.span,
    }
}

fn is_fresh(place: &PlaceName) -> bool {
    place.as_str().starts_with("tmp.")
}

impl Lower<'_> {
    // ---- names and types ---------------------------------------------------

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
            TypeKind::Array { elem, len } => {
                LirTy::Array(Box::new(self.lir_ty(TypeRef::value(*elem))), *len)
            }
            TypeKind::Param { .. } | TypeKind::Error => LirTy::Named("error".to_owned()),
        };
        if ty.is_ref {
            base.to_ref()
        } else {
            base
        }
    }

    /// The size and alignment of a LIR type, read from the module's layouts
    /// (never computed here). Every type lowering sizes was checked to fit.
    fn lir_size_align(&self, ty: &LirTy) -> lilac_layout::SizeAlign {
        lilac_ir::size_align(&self.module.layouts, ty, &self.target).unwrap_or_else(|| {
            lilac_error::bug!(Span::dummy(), "`{ty}` has no size on `{}`", self.target)
        })
    }

    /// `@bit_native`'s width in bits.
    fn native_bits(&self) -> u32 {
        self.target.bit_native_width()
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
        let mut state = FnState::new();
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
                let place = state.place_name(&base);
                let ty = if extern_ {
                    self.abi_ty(local.ty).0
                } else {
                    self.lir_ty(local.ty)
                };
                state.place_tys.insert(place.clone(), ty.clone());
                state.places.insert(*id, place.clone());
                if prefix == "arg" {
                    state.ins.push((place, ty));
                } else {
                    state.outs.push((place, ty));
                }
            }
        }
        let Some(body) = &func.body else {
            return LirFunc {
                name: symbol,
                ins: state.ins,
                outs: state.outs,
                is_extern: true,
                export: None,
                variadic: self
                    .res
                    .def(func.key.def)
                    .func()
                    .is_some_and(|decl| decl.variadic),
                inline: false,
                blocks: Vec::new(),
            };
        };
        for (place, _) in state.outs.clone() {
            state.emit(LirInst::Zero {
                place,
                span: func.span,
            });
        }
        for (_, flag) in &func.flags {
            let place = self.declare(&mut state, func, *flag);
            state.emit(LirInst::Zero {
                place,
                span: func.span,
            });
        }
        for id in &func.params {
            Self::mark(&mut state, func, &HirPath::local(*id), true, func.span);
        }
        for stmt in &body.stmts {
            self.lower_stmt(&mut state, func, stmt);
        }
        let end = end_of(body.span);
        if state.open {
            self.drop_scope(&mut state, 0, end);
        }
        let mut lowered = state.finish(symbol, func.inline, end);
        lowered.export = self.export_of(func);
        lowered
    }

    /// The C entry of an `export`: its symbol and each slot's C type.
    fn export_of(&self, func: &HirFunc) -> Option<LirExport> {
        let decl = self.res.def(func.key.def).func()?;
        if !decl.is_export {
            return None;
        }
        let abi = |ids: &[LocalId]| -> Vec<LirTy> {
            ids.iter()
                .map(|id| self.abi_ty(func.local(*id).ty).0)
                .collect()
        };
        Some(LirExport {
            symbol: Symbol::new(extern_symbol(self.res, func.key.def)),
            ins: abi(&func.params),
            outs: abi(&func.returns),
        })
    }

    /// Declares a local's place (zeroed alloca) and returns it.
    fn declare(&mut self, state: &mut FnState, func: &HirFunc, id: LocalId) -> PlaceName {
        let local = func.local(id);
        let base = if local.name.is_empty() {
            format!("l.{}", id.0)
        } else {
            local.name.clone()
        };
        let place = state.place_name(&base);
        let ty = self.lir_ty(local.ty);
        state.alloca(&place, ty, local.span);
        state.places.insert(id, place.clone());
        place
    }
}

#[cfg(test)]
mod tests;
