//! LIR → bytecode compilation.
//!
//! [`compile`] walks a verified [`LirModule`] and produces a [`Program`] whose
//! operands are fully resolved integers: registers are dense `u32` indices,
//! places are frame byte offsets, and branch targets are program counters.
//!
//! # Aggregate values
//!
//! A VM register is a single `u64`. A value whose packed size exceeds
//! [`VM_POINTER_BYTES`] cannot be held inline, so an *indirect*-typed register
//! is represented by a `u64` arena offset ("ref") to the aggregate bytes:
//!
//! * `Const` of a `#byte_buffer` data symbol references a synthesized
//!   `{len, ptr}` struct appended to [`Program::data`]; the raw text bytes stay
//!   a separate entry and the struct's `ptr` is their `global_off`.
//! * `ConstData` always writes the referenced entry's `global_off` into the
//!   register.
//! * `CopyReg` of an indirect value from a place emits `Addr`, making the
//!   register a ref to that place.
//! * `Load` of an indirect value copies the ref held by the source slot into
//!   the register.
//! * `Copy`/`Store`/`StoreField` with an indirect `ty` and a register operand
//!   treat the register as a ref; the interpreter copies `size(ty)` bytes.
//! * `Field` of an indirect field emits `AddrField`.
//!
//! Values at most [`VM_POINTER_BYTES`] wide (including small named aggregates)
//! are held directly. All instruction spans feed [`Func::lines`] through the
//! session [`SourceMap`]; dummy spans are skipped. `alloca`/`drop` emit no code
//! (the frame region is sized and zeroed at entry).
//!
//! # Field access through references
//!
//! `Field`/`AddrField`/`StoreField` carry an `indirect` flag set from the
//! object place's type: `true` when the place is a `ref T` (the interpreter
//! dereferences the slot's stored ref to get the field base before applying
//! `off`), `false` when the slot itself is the aggregate (as the LLVM backend's
//! `field_ptr` does). This makes the dereference explicit for every object
//! place, including ref-typed locals, so the interpreter never needs the
//! owner's type at run time.

use std::collections::{HashMap, HashSet};

use lilac_ir::{builtin_layout, LirDataKind, LirFunc, LirInst, LirModule, LirOperand, LirTerm};
use lilac_span::{SourceMap, Span};

use crate::encode::NARROW_MAX;
use crate::isa::{
    verify, ArithOp, Builtin, ConvertOp, Data, Func, Instr, Line, Operand, Program, TypeId,
    TypeInfo, LBC_VERSION, VM_POINTER_BYTES, VM_TARGET,
};

/// Compiles a verified LIR module to a bytecode [`Program`].
///
/// # Errors
///
/// Returns a human-readable message for an internal inconsistency that cannot
/// occur in verified LIR (for example a reference to an unknown place or
/// function). `CompileError`/`FoldIntrinsic` nodes and a program that fails
/// the bytecode verifier are internal errors reported with
/// `lilac_error::bug!`.
pub fn compile(module: &LirModule, source_map: &SourceMap) -> Result<Program, String> {
    let mut compiler = Compiler::new(module, source_map)?;
    let mut funcs = Vec::with_capacity(module.funcs.len());
    for func in &module.funcs {
        funcs.push(compiler.compile_func(func)?);
    }
    let mut program = Program {
        version: LBC_VERSION.to_owned(),
        target: VM_TARGET.to_owned(),
        types: compiler.types.infos,
        data: compiler.data,
        globals: compiler.globals,
        funcs,
    };
    program.rebuild_layouts();
    if let Err(err) = verify(&program) {
        lilac_error::bug!(Span::dummy(), "compiled program failed verification: {err}");
    }
    Ok(program)
}

/// The program-wide type table, deduplicated by name.
struct TypeTable {
    infos: Vec<TypeInfo>,
    ids: HashMap<String, TypeId>,
}

impl TypeTable {
    fn new() -> Self {
        Self {
            infos: Vec::new(),
            ids: HashMap::new(),
        }
    }

    /// Interns `name`, computing its packed size on first use.
    fn intern(&mut self, module: &LirModule, name: &str) -> Result<TypeId, String> {
        if let Some(&id) = self.ids.get(name) {
            return Ok(id);
        }
        let size = type_size(module, name, 0)?;
        let id = u32::try_from(self.infos.len()).map_err(|_| "too many types".to_string())?;
        self.infos.push(TypeInfo {
            name: name.to_owned(),
            size,
            is_ref: is_ref_ty(name),
        });
        self.ids.insert(name.to_owned(), id);
        Ok(id)
    }
}

/// Strips a `ref ` prefix, if any.
fn strip_ref(ty: &str) -> &str {
    ty.strip_prefix("ref ").unwrap_or(ty)
}

/// Whether an IR type spelling denotes a reference.
fn is_ref_ty(ty: &str) -> bool {
    ty.starts_with("ref ") || ty == "#ptr"
}

/// The packed size of a type in bytes.
///
/// `ref` (and `#ptr`) are a pointer wide; `#bitN` are `N / 8`; `#bit_native`
/// follows the VM's 64-bit pointer; `#byte_buffer`/`#format_args` and named
/// layouts sum their fields. `#str`/`#numlit` are data-only and cannot be
/// sized. `depth` guards against a recursive layout (which the type checker
/// rejects, so a trip is an internal inconsistency).
fn type_size(module: &LirModule, ty: &str, depth: u32) -> Result<u32, String> {
    if depth > 64 {
        return Err(format!("recursive layout for type `{ty}`"));
    }
    if ty.starts_with("ref ") {
        return Ok(VM_POINTER_BYTES);
    }
    match ty {
        "#bit8" => Ok(1),
        "#bit16" => Ok(2),
        "#bit32" => Ok(4),
        "#bit64" | "#bit_native" | "#ptr" => Ok(VM_POINTER_BYTES),
        "#str" | "#numlit" => Err(format!("data-only type `{ty}` has no size")),
        other => {
            let fields =
                layout_fields(module, other).ok_or_else(|| format!("unknown type `{other}`"))?;
            let mut total = 0_u32;
            for (_, field) in fields {
                total = total
                    .checked_add(type_size(module, &field, depth + 1)?)
                    .ok_or_else(|| format!("layout `{other}` too large"))?;
            }
            Ok(total)
        }
    }
}

/// Returns a type's fields (name, type) from the module or builtin layouts.
fn layout_fields(module: &LirModule, owner_ty: &str) -> Option<Vec<(String, String)>> {
    let base = strip_ref(owner_ty);
    if let Some(fields) = module.layouts.get(base) {
        return Some(fields.clone());
    }
    builtin_layout(base).map(|fields| {
        fields
            .iter()
            .map(|(name, ty)| ((*name).to_owned(), (*ty).to_owned()))
            .collect()
    })
}

/// The packed byte offset of field `idx` of `owner_ty`.
fn field_offset(module: &LirModule, owner_ty: &str, idx: u32) -> Result<u32, String> {
    let fields =
        layout_fields(module, owner_ty).ok_or_else(|| format!("unknown layout `{owner_ty}`"))?;
    let mut off = 0_u32;
    for (_, field) in fields.iter().take(idx as usize) {
        off = off
            .checked_add(type_size(module, field, 0)?)
            .ok_or_else(|| format!("field offset overflow in `{owner_ty}`"))?;
    }
    Ok(off)
}

/// The declared field type at `idx` of `owner_ty`.
fn field_type(module: &LirModule, owner_ty: &str, idx: u32) -> Result<String, String> {
    let fields =
        layout_fields(module, owner_ty).ok_or_else(|| format!("unknown layout `{owner_ty}`"))?;
    fields
        .get(idx as usize)
        .map(|(_, ty)| ty.clone())
        .ok_or_else(|| format!("field index {idx} out of range for `{owner_ty}`"))
}

/// Whether a type is represented indirectly (by a ref) in a register.
fn is_indirect(module: &LirModule, ty: &str) -> Result<bool, String> {
    Ok(type_size(module, ty, 0)? > VM_POINTER_BYTES)
}

/// Parses an integer literal (`0x`/`0b`/decimal, `_` separators), mirroring the
/// constant folder and the LLVM backend.
fn parse_int(text: &str) -> Option<u64> {
    let digits = text.trim().replace('_', "");
    if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        u64::from_str_radix(bin, 2).ok()
    } else if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        digits.parse::<u64>().ok()
    } else {
        None
    }
}

/// The width in bytes of a bit-container (or pointer) type.
fn integer_bytes(ty: &str) -> u32 {
    match strip_ref(ty) {
        "#bit8" => 1,
        "#bit16" => 2,
        "#bit32" => 4,
        _ => VM_POINTER_BYTES,
    }
}

/// Truncates `imm` to `bytes` bytes.
fn mask_imm(imm: u64, bytes: u32) -> u64 {
    if bytes >= 8 {
        imm
    } else {
        imm & ((1_u64 << (bytes * 8)) - 1)
    }
}

/// Maps a LIR arithmetic operator onto the ISA operator.
fn map_arith(op: &lilac_ir::ArithOp) -> ArithOp {
    match op {
        lilac_ir::ArithOp::IAdd => ArithOp::IAdd,
        lilac_ir::ArithOp::ISub => ArithOp::ISub,
        lilac_ir::ArithOp::IMul => ArithOp::IMul,
        lilac_ir::ArithOp::IDiv => ArithOp::IDiv,
        lilac_ir::ArithOp::IDivU => ArithOp::IDivU,
        lilac_ir::ArithOp::IMod => ArithOp::IMod,
        lilac_ir::ArithOp::IModU => ArithOp::IModU,
        lilac_ir::ArithOp::FAdd => ArithOp::FAdd,
        lilac_ir::ArithOp::FSub => ArithOp::FSub,
        lilac_ir::ArithOp::FMul => ArithOp::FMul,
        lilac_ir::ArithOp::FDiv => ArithOp::FDiv,
        lilac_ir::ArithOp::FMod => ArithOp::FMod,
        lilac_ir::ArithOp::Eq => ArithOp::Eq,
        lilac_ir::ArithOp::ILt => ArithOp::ILt,
        lilac_ir::ArithOp::ILtU => ArithOp::ILtU,
        lilac_ir::ArithOp::IGt => ArithOp::IGt,
        lilac_ir::ArithOp::IGtU => ArithOp::IGtU,
        lilac_ir::ArithOp::FLt => ArithOp::FLt,
        lilac_ir::ArithOp::FGt => ArithOp::FGt,
    }
}

/// Maps a LIR conversion operator onto the ISA operator.
fn map_convert(op: &lilac_ir::ConvertOp) -> ConvertOp {
    match op {
        lilac_ir::ConvertOp::Trunc => ConvertOp::Trunc,
        lilac_ir::ConvertOp::ExtendZero => ConvertOp::ExtendZero,
    }
}

/// The per-frame place region: name → byte offset and type.
///
/// Output places are *ref-backed* (the pointer ABI, spec §5): instead of
/// holding their value inline they occupy an 8-byte cell in the frame's
/// out-ref region, which `call` fills with the absolute arena offset of the
/// caller's storage. [`Places::is_out`] marks them so every access compiles
/// indirect (through the ref), matching the LLVM backend.
#[derive(Default)]
struct Places {
    offsets: HashMap<String, u32>,
    types: HashMap<String, String>,
    names: Vec<(String, u32)>,
    outputs: HashSet<String>,
    scratch: Option<u32>,
    total: u32,
}

impl Places {
    /// Declares a place, packing it after all previously declared places.
    ///
    /// An output (`is_out`) is a pointer-sized ref cell regardless of its value
    /// type; every other place occupies `type_size(ty)` bytes.
    fn declare(
        &mut self,
        module: &LirModule,
        name: &str,
        ty: &str,
        is_out: bool,
    ) -> Result<(), String> {
        if self.offsets.contains_key(name) {
            return Err(format!("duplicate place `%{name}`"));
        }
        let size = if is_out {
            VM_POINTER_BYTES
        } else {
            type_size(module, ty, 0)?
        };
        let off = self.total;
        self.total = self
            .total
            .checked_add(size)
            .ok_or_else(|| format!("frame too large for `{name}`"))?;
        self.offsets.insert(name.to_owned(), off);
        self.types.insert(name.to_owned(), ty.to_owned());
        if is_out {
            self.outputs.insert(name.to_owned());
        }
        self.names.push((name.to_owned(), off));
        Ok(())
    }

    /// Whether `name` is a ref-backed output place.
    fn is_out(&self, name: &str) -> bool {
        self.outputs.contains(name)
    }

    /// The offset of a shared 8-byte scratch cell, allocated on first use.
    ///
    /// `load`/`store` of a `ref`-typed output need a second indirection: the
    /// ref value lives in the caller's storage, so it is staged through this
    /// cell. One cell suffices because it is consumed immediately.
    fn scratch(&mut self) -> u32 {
        if let Some(off) = self.scratch {
            return off;
        }
        let off = self.total;
        self.total = self.total.saturating_add(VM_POINTER_BYTES);
        self.scratch = Some(off);
        off
    }

    /// The byte offset of a declared place.
    fn slot(&self, name: &str) -> Result<u32, String> {
        self.offsets
            .get(name)
            .copied()
            .ok_or_else(|| format!("unknown place `%{name}`"))
    }

    /// The declared type of a place.
    fn ty(&self, name: &str) -> Result<&str, String> {
        self.types
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("unknown place `%{name}`"))
    }
}

/// Mutable state for compiling one function.
struct FuncBuilder<'a, 'm> {
    compiler: &'m mut Compiler<'a>,
    func: &'m LirFunc,
    places: Places,
    block_pc: HashMap<String, u32>,
    regs: HashMap<String, u32>,
    reg_types: HashMap<String, String>,
    next_reg: u32,
    code: Vec<Instr>,
    lines: Vec<Line>,
}

/// Which field of a branch instruction a deferred label patch fills.
#[derive(Copy, Clone)]
enum BranchSlot {
    /// `Instr::Branch::target`.
    Target,
    /// `Instr::BranchIf::then`.
    Then,
    /// `Instr::BranchIf::els`.
    Els,
}

/// A branch target left unresolved until every block start is known.
///
/// Block starts are recorded from *actual* emission (an out-place access can
/// emit several instructions), so a terminator is emitted with placeholder
/// targets and patched once the whole body has been walked.
struct PendingTarget {
    /// Index into [`FuncBuilder::code`] of the branch instruction.
    index: usize,
    /// Field of that instruction to patch.
    slot: BranchSlot,
    /// Label naming the destination block.
    label: String,
}

impl FuncBuilder<'_, '_> {
    /// Interns a type in the program type table.
    fn intern(&mut self, ty: &str) -> Result<TypeId, String> {
        self.compiler.types.intern(self.compiler.module, ty)
    }

    /// The index of an already-defined register.
    fn reg(&self, name: &str) -> Result<u32, String> {
        self.regs
            .get(name)
            .copied()
            .ok_or_else(|| format!("unknown register `&{name}`"))
    }

    /// The declared type of an already-defined register.
    fn reg_ty(&self, name: &str) -> Result<&str, String> {
        self.reg_types
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("unknown register type for `&{name}`"))
    }

    /// Defines `name` as a fresh register (or returns its existing index).
    fn def_reg(&mut self, name: &str) -> u32 {
        if let Some(&index) = self.regs.get(name) {
            return index;
        }
        let index = self.next_reg;
        self.next_reg = self.next_reg.saturating_add(1);
        self.regs.insert(name.to_owned(), index);
        index
    }

    /// Defines `name` with its declared type `ty`.
    fn def_reg_typed(&mut self, name: &str, ty: &str) -> u32 {
        let index = self.def_reg(name);
        self.reg_types.insert(name.to_owned(), ty.to_owned());
        index
    }

    /// Resolves a place operand to its slot offset.
    fn slot(&self, name: &str) -> Result<u32, String> {
        self.places.slot(name)
    }

    /// Resolves a LIR operand to a VM operand.
    fn operand(&self, operand: &LirOperand) -> Result<Operand, String> {
        match operand {
            LirOperand::Reg(name) => Ok(Operand::Reg(self.reg(name)?)),
            LirOperand::Place(name) => Ok(Operand::Slot(self.slot(name)?)),
        }
    }

    /// A fresh anonymous register (used for synthesized output reads).
    fn fresh_reg(&mut self) -> u32 {
        let index = self.next_reg;
        self.next_reg = self.next_reg.saturating_add(1);
        index
    }

    /// The `#ptr` type id (a pointer-sized scalar).
    fn ptr_ty(&mut self) -> Result<TypeId, String> {
        self.intern("#ptr")
    }

    /// Reads a place as a value operand.
    ///
    /// A ref-backed output is read through its caller-supplied ref into a
    /// fresh register (`Load`); every other place is addressed directly as a
    /// slot operand. This is how call inputs and value copies consume an
    /// output's current value.
    fn read_operand(&mut self, place: &str, span: Span) -> Result<Operand, String> {
        if !self.places.is_out(place) {
            return Ok(Operand::Slot(self.slot(place)?));
        }
        let ty_name = self.places.ty(place)?.to_owned();
        let ty = self.intern(&ty_name)?;
        let dst = self.fresh_reg();
        let src = self.slot(place)?;
        self.emit(Instr::Load { dst, src, ty }, span)?;
        Ok(Operand::Reg(dst))
    }

    /// Reads a LIR operand as a value (register as-is, place through
    /// [`Self::read_operand`]).
    fn value_operand(&mut self, operand: &LirOperand, span: Span) -> Result<Operand, String> {
        match operand {
            LirOperand::Reg(name) => Ok(Operand::Reg(self.reg(name)?)),
            LirOperand::Place(name) => self.read_operand(name, span),
        }
    }

    /// Loads the inner ref stored at a `ref`-typed output's caller storage.
    ///
    /// The output's ref cell points at the caller's storage, which itself holds
    /// a `ref` value; a `load`/`store` through that output must use the inner
    /// ref, so this reads it into a fresh register.
    fn out_ref_value(&mut self, place: &str, span: Span) -> Result<u32, String> {
        let ty = self.ptr_ty()?;
        let dst = self.fresh_reg();
        let src = self.slot(place)?;
        self.emit(Instr::Load { dst, src, ty }, span)?;
        Ok(dst)
    }

    /// Stages `reg`'s ref value in the shared scratch cell, returning the cell.
    fn stage_ref(&mut self, reg: u32, span: Span) -> Result<u32, String> {
        let ty = self.ptr_ty()?;
        let scratch = self.places.scratch();
        self.emit(
            Instr::Copy {
                dst: Operand::Slot(scratch),
                src: Operand::Reg(reg),
                ty,
            },
            span,
        )?;
        Ok(scratch)
    }

    /// Pushes an instruction and records its source line (dummy spans skipped).
    fn emit(&mut self, instr: Instr, span: Span) -> Result<(), String> {
        let pc = u32::try_from(self.code.len()).map_err(|_| "program too large".to_string())?;
        self.code.push(instr);
        if span.is_dummy() {
            return Ok(());
        }
        let Some((line, col)) = self.compiler.source_map.line_col(span) else {
            return Ok(());
        };
        let Some(file) = self.compiler.source_map.file(span.file) else {
            return Ok(());
        };
        let line = u32::try_from(line).map_err(|_| "line number too large".to_string())?;
        let col = u32::try_from(col).map_err(|_| "column too large".to_string())?;
        self.lines.push(Line {
            pc,
            file: file.name.clone(),
            line,
            col,
        });
        Ok(())
    }

    /// The packed field offset of `obj.idx`.
    fn field_off(&self, obj: &str, idx: u32) -> Result<u32, String> {
        let owner = self.places.ty(obj)?;
        field_offset(self.compiler.module, owner, idx)
    }

    /// Emits the code for one instruction.
    #[allow(clippy::too_many_lines)]
    fn emit_inst(&mut self, inst: &LirInst) -> Result<(), String> {
        match inst {
            LirInst::CopyReg { src, dst, ty, span } => {
                let id = self.intern(ty)?;
                if let LirOperand::Place(place) = src {
                    if self.places.is_out(place) {
                        let dst = self.def_reg_typed(dst, ty);
                        let src = self.slot(place)?;
                        self.emit(Instr::Load { dst, src, ty: id }, *span)?;
                        return Ok(());
                    }
                }
                if is_indirect(self.compiler.module, ty)? {
                    let dst = self.def_reg_typed(dst, ty);
                    self.emit(
                        match src {
                            LirOperand::Place(place) => Instr::Addr {
                                dst,
                                place: self.slot(place)?,
                            },
                            LirOperand::Reg(reg) => Instr::Copy {
                                dst: Operand::Reg(dst),
                                src: Operand::Reg(self.reg(reg)?),
                                ty: id,
                            },
                        },
                        *span,
                    )
                } else {
                    let dst = self.def_reg_typed(dst, ty);
                    let src = self.operand(src)?;
                    self.emit(
                        Instr::Copy {
                            dst: Operand::Reg(dst),
                            src,
                            ty: id,
                        },
                        *span,
                    )
                }
            }
            LirInst::CopyPlace { src, dst, span } => {
                let dst_ty = self.places.ty(dst)?.to_owned();
                if self.places.is_out(dst) {
                    // Writing an output goes through its caller-supplied ref.
                    let src = self.value_operand(src, *span)?;
                    let ty = self.intern(&dst_ty)?;
                    let dst = self.slot(dst)?;
                    self.emit(Instr::Store { src, dst, ty }, *span)
                } else {
                    match src {
                        LirOperand::Place(place) if self.places.is_out(place) => {
                            // Reading an output stages its value in a register
                            // first, then copies into the destination place.
                            let ty = self.intern(&dst_ty)?;
                            let reg = self.fresh_reg();
                            let src = self.slot(place)?;
                            self.emit(Instr::Load { dst: reg, src, ty }, *span)?;
                            let dst = self.slot(dst)?;
                            self.emit(
                                Instr::Copy {
                                    dst: Operand::Slot(dst),
                                    src: Operand::Reg(reg),
                                    ty,
                                },
                                *span,
                            )
                        }
                        LirOperand::Place(place) => {
                            let size = type_size(self.compiler.module, &dst_ty, 0)?;
                            let src = self.slot(place)?;
                            let dst = self.slot(dst)?;
                            self.emit(Instr::CopyBytes { dst, src, size }, *span)
                        }
                        LirOperand::Reg(reg) => {
                            let ty = self.intern(&dst_ty)?;
                            let src = Operand::Reg(self.reg(reg)?);
                            let dst = self.slot(dst)?;
                            self.emit(
                                Instr::Copy {
                                    dst: Operand::Slot(dst),
                                    src,
                                    ty,
                                },
                                *span,
                            )
                        }
                    }
                }
            }
            LirInst::Const { dst, lit, ty, span } => {
                let dst = self.def_reg_typed(dst, ty);
                if let Some(name) = lit.strip_prefix('!') {
                    let raw = self.compiler.data_id(name)?;
                    let ty_id = self.intern(ty)?;
                    if *ty == "#byte_buffer" {
                        let data = self.compiler.byte_buffer_struct(raw)?;
                        self.emit(
                            Instr::ConstData {
                                dst,
                                data,
                                ty: ty_id,
                            },
                            *span,
                        )
                    } else if is_ref_ty(ty) {
                        self.emit(
                            Instr::ConstData {
                                dst,
                                data: raw,
                                ty: ty_id,
                            },
                            *span,
                        )
                    } else {
                        lilac_error::bug!(
                            *span,
                            "data-symbol const type `{ty}` has no bytecode lowering"
                        );
                    }
                } else {
                    let imm =
                        parse_int(lit).ok_or_else(|| format!("invalid const literal `{lit}`"))?;
                    let imm = mask_imm(imm, integer_bytes(ty));
                    let ty = self.intern(ty)?;
                    self.emit(Instr::Const { dst, imm, ty }, *span)
                }
            }
            LirInst::Load { src, dst, ty, span } => {
                let dst = self.def_reg_typed(dst, ty);
                let ty = self.intern(ty)?;
                if self.places.is_out(src) {
                    // The caller's storage holds the ref; load it, stage it, then
                    // load the pointee through it.
                    let inner = self.out_ref_value(src, *span)?;
                    let src = self.stage_ref(inner, *span)?;
                    self.emit(Instr::Load { dst, src, ty }, *span)
                } else {
                    let src = self.slot(src)?;
                    self.emit(Instr::Load { dst, src, ty }, *span)
                }
            }
            LirInst::Addr {
                place, dst, span, ..
            } => {
                let dst = self.def_reg_typed(dst, "#ptr");
                if self.places.is_out(place) {
                    // `addr %out` is the ref itself: copy the caller-supplied
                    // offset out of the out-ref cell into the register.
                    let ty = self.ptr_ty()?;
                    let src = self.slot(place)?;
                    self.emit(
                        Instr::Copy {
                            dst: Operand::Reg(dst),
                            src: Operand::Slot(src),
                            ty,
                        },
                        *span,
                    )
                } else {
                    let place = self.slot(place)?;
                    self.emit(Instr::Addr { dst, place }, *span)
                }
            }
            LirInst::AddrFunc {
                func, dst, span, ..
            } => {
                let dst = self.def_reg_typed(dst, "#ptr");
                let func = self.compiler.func_id(func)?;
                self.emit(Instr::AddrFunc { dst, func }, *span)
            }
            LirInst::Call {
                func,
                ins,
                outs,
                span,
            } => {
                let ins = ins
                    .iter()
                    .map(|place| self.read_operand(place, *span))
                    .collect::<Result<Vec<_>, _>>()?;
                let outs = outs
                    .iter()
                    .map(|place| self.slot(place))
                    .collect::<Result<Vec<_>, _>>()?;
                let instr = match func {
                    lilac_ir::LirCallee::Func(name) => Instr::Call {
                        func: self.compiler.func_id(name)?,
                        ins,
                        outs,
                    },
                    lilac_ir::LirCallee::Reg(name) => Instr::CallIndirect {
                        callee: self.reg(name)?,
                        ins,
                        outs,
                    },
                    lilac_ir::LirCallee::Builtin(name) => {
                        let builtin = Builtin::parse(name.strip_prefix('@').unwrap_or(name))
                            .ok_or_else(|| format!("unknown builtin `{name}`"))?;
                        Instr::CallBuiltin { builtin, ins, outs }
                    }
                };
                self.emit(instr, *span)
            }
            LirInst::Field {
                obj,
                idx,
                dst,
                ty,
                span,
                ..
            } => {
                let dst = self.def_reg_typed(dst, ty);
                let place = self.slot(obj)?;
                let off = self.field_off(obj, *idx)?;
                let indirect = self.places.is_out(obj) || is_ref_ty(self.places.ty(obj)?);
                if is_indirect(self.compiler.module, ty)? {
                    self.emit(
                        Instr::AddrField {
                            dst,
                            place,
                            off,
                            indirect,
                        },
                        *span,
                    )
                } else {
                    let ty = self.intern(ty)?;
                    self.emit(
                        Instr::Field {
                            dst,
                            place,
                            off,
                            ty,
                            indirect,
                        },
                        *span,
                    )
                }
            }
            LirInst::StoreField {
                value,
                obj,
                idx,
                span,
                ..
            } => {
                let place = self.slot(obj)?;
                let off = self.field_off(obj, *idx)?;
                let indirect = self.places.is_out(obj) || is_ref_ty(self.places.ty(obj)?);
                let field_ty = field_type(self.compiler.module, self.places.ty(obj)?, *idx)?;
                let src = self.value_operand(value, *span)?;
                let ty = self.intern(&field_ty)?;
                self.emit(
                    Instr::StoreField {
                        src,
                        place,
                        off,
                        ty,
                        indirect,
                    },
                    *span,
                )
            }
            LirInst::Store { src, dst, span } => {
                let pointee = strip_ref(self.places.ty(dst)?).to_owned();
                let ty = self.intern(&pointee)?;
                if self.places.is_out(dst) {
                    // Store *through* the ref held at the caller's storage: load
                    // the inner ref, stage it, then store the value through it.
                    let inner = self.out_ref_value(dst, *span)?;
                    let dst = self.stage_ref(inner, *span)?;
                    let src = self.value_operand(src, *span)?;
                    self.emit(Instr::Store { src, dst, ty }, *span)
                } else {
                    let dst = self.slot(dst)?;
                    let src = self.value_operand(src, *span)?;
                    self.emit(Instr::Store { src, dst, ty }, *span)
                }
            }
            LirInst::Convert {
                op,
                src,
                dst,
                ty,
                span,
            } => {
                let dst = self.def_reg_typed(dst, ty);
                let src = self.reg(src)?;
                let ty = self.intern(ty)?;
                self.emit(
                    Instr::Convert {
                        op: map_convert(op),
                        dst,
                        src,
                        ty,
                    },
                    *span,
                )
            }
            LirInst::FieldAddr {
                obj,
                idx,
                dst,
                span,
                ..
            } => {
                let dst = self.def_reg_typed(dst, "#ptr");
                let place = self.slot(obj)?;
                let off = self.field_off(obj, *idx)?;
                let indirect = self.places.is_out(obj) || is_ref_ty(self.places.ty(obj)?);
                self.emit(
                    Instr::AddrField {
                        dst,
                        place,
                        off,
                        indirect,
                    },
                    *span,
                )
            }
            LirInst::Arith {
                op,
                lhs,
                rhs,
                dst,
                ty,
                span,
            } => {
                let operand_ty_name = self.reg_ty(lhs)?.to_owned();
                let operand_ty = self.intern(&operand_ty_name)?;
                let dst = self.def_reg_typed(dst, ty);
                let lhs = self.reg(lhs)?;
                let rhs = self.reg(rhs)?;
                let ty = self.intern(ty)?;
                self.emit(
                    Instr::Arith {
                        op: map_arith(op),
                        dst,
                        lhs,
                        rhs,
                        operand_ty,
                        ty,
                    },
                    *span,
                )
            }
            LirInst::Alloca { .. } | LirInst::Drop { .. } => Ok(()),
            LirInst::CompileError { call_span, .. } => {
                lilac_error::bug!(
                    *call_span,
                    "unfolded `@compile_error` reached the VM compiler"
                );
            }
            LirInst::FoldIntrinsic { name, span, .. } => {
                lilac_error::bug!(*span, "unfolded `{name}` reached the VM compiler");
            }
        }
    }

    /// Emits the code for a block terminator.
    ///
    /// Branch targets are emitted as placeholders and pushed onto `pending`;
    /// [`Self::resolve_targets`] fills them once every block start is known.
    fn emit_term(
        &mut self,
        term: &LirTerm,
        next: Option<&str>,
        pending: &mut Vec<PendingTarget>,
    ) -> Result<(), String> {
        match term {
            LirTerm::Return { span } => self.emit(Instr::Return, *span),
            LirTerm::Branch { target, span } => {
                let index = self.code.len();
                self.emit(Instr::Branch { target: 0 }, *span)?;
                pending.push(PendingTarget {
                    index,
                    slot: BranchSlot::Target,
                    label: target.clone(),
                });
                Ok(())
            }
            LirTerm::BranchIf {
                cond,
                then,
                els,
                span,
            } => {
                let cond = self.reg(cond)?;
                let els_label = match els {
                    Some(label) => label.clone(),
                    None => next
                        .ok_or_else(|| "`branch.if` without an else is the last block".to_string())?
                        .to_owned(),
                };
                let index = self.code.len();
                self.emit(
                    Instr::BranchIf {
                        cond,
                        then: 0,
                        els: 0,
                    },
                    *span,
                )?;
                pending.push(PendingTarget {
                    index,
                    slot: BranchSlot::Then,
                    label: then.clone(),
                });
                pending.push(PendingTarget {
                    index,
                    slot: BranchSlot::Els,
                    label: els_label,
                });
                Ok(())
            }
            LirTerm::Panic { msg, span } => {
                let data = self.compiler.data_id(msg)?;
                self.emit(Instr::Panic { data }, *span)
            }
            LirTerm::Unreachable { span } => self.emit(Instr::Unreachable, *span),
        }
    }

    /// Fills every deferred branch target with its block's first instruction.
    fn resolve_targets(&mut self, pending: Vec<PendingTarget>) -> Result<(), String> {
        for patch in pending {
            let target = self.label_pc(&patch.label)?;
            match (&mut self.code[patch.index], patch.slot) {
                (Instr::Branch { target: slot }, BranchSlot::Target) => *slot = target,
                (Instr::BranchIf { then, .. }, BranchSlot::Then) => *then = target,
                (Instr::BranchIf { els, .. }, BranchSlot::Els) => *els = target,
                _ => return Err("internal branch patch mismatch".to_string()),
            }
        }
        Ok(())
    }

    /// The program counter of a labeled block.
    fn label_pc(&self, label: &str) -> Result<u32, String> {
        self.block_pc
            .get(label)
            .copied()
            .ok_or_else(|| format!("unknown block `${label}`"))
    }

    /// Emits the whole function body.
    fn run(mut self) -> Result<Func, String> {
        let func = self.func;
        let ins = func
            .ins
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>()
            .iter()
            .map(|ty| self.intern(ty))
            .collect::<Result<Vec<_>, _>>()?;
        let outs = func
            .outs
            .iter()
            .map(|(_, ty)| ty.clone())
            .collect::<Vec<_>>()
            .iter()
            .map(|ty| self.intern(ty))
            .collect::<Result<Vec<_>, _>>()?;
        if func.is_extern {
            return Ok(Func {
                name: func.name.clone(),
                is_extern: true,
                ins,
                outs,
                in_offsets: Vec::new(),
                in_sizes: Vec::new(),
                out_offset: 0,
                out_sizes: Vec::new(),
                n_regs: 0,
                slot_bytes: self.places.total,
                code: Vec::new(),
                call_sites: Vec::new(),
                operands: Vec::new(),
                lines: Vec::new(),
                place_names: std::mem::take(&mut self.places.names),
            });
        }
        // Walk the blocks, recording each start from the *actual* instruction
        // count emitted (an out-place access may expand to several ISA
        // instructions). Terminators carry placeholder targets, resolved after
        // every block start is known.
        let mut pending: Vec<PendingTarget> = Vec::new();
        let block_count = func.blocks.len();
        for index in 0..block_count {
            let block = &func.blocks[index];
            let start =
                u32::try_from(self.code.len()).map_err(|_| "program too large".to_string())?;
            self.block_pc.insert(block.label.clone(), start);
            self.regs.clear();
            self.reg_types.clear();
            for inst in &block.insts {
                self.emit_inst(inst)?;
            }
            let next = func.blocks.get(index + 1).map(|b| b.label.as_str());
            self.emit_term(&block.term, next, &mut pending)?;
        }
        self.resolve_targets(pending)?;
        // Fold a redundant `copy %place -> &r` and a `const k -> &j` into the
        // arithmetic that consumes both, then remap branch targets and line
        // pcs to the folded instruction indices.
        let (folded, arith_map) = fold_arith_imm(&self.compiler.types.infos, &self.code);
        let (folded, call_map) = fold_call_inputs(&folded);
        let (folded, fall_map) = drop_fallthrough_branches(&folded);
        let composed: Vec<u32> = arith_map
            .iter()
            .map(|&m| remap(&call_map, m))
            .map(|m| remap(&fall_map, m))
            .collect();
        for line in &mut self.lines {
            line.pc = *composed
                .get(line.pc as usize)
                .ok_or_else(|| "source-line pc out of range".to_string())?;
        }
        self.code = folded;
        // The typed list still uses instruction-index branch targets and line
        // pcs; encode it into the executed word stream and remap both to word
        // offsets (M5c.1 makes `pc` a word index).
        let (words, call_sites, operands, offsets) = {
            let (builder, offsets) =
                crate::encode::encode_instrs(&self.compiler.types.infos, &self.code)?;
            (builder.words, builder.call_sites, builder.operands, offsets)
        };
        for line in &mut self.lines {
            line.pc = *offsets
                .get(line.pc as usize)
                .ok_or_else(|| "source-line pc out of range".to_string())?;
        }
        Ok(Func {
            name: func.name.clone(),
            is_extern: false,
            ins,
            outs,
            in_offsets: Vec::new(),
            in_sizes: Vec::new(),
            out_offset: 0,
            out_sizes: Vec::new(),
            n_regs: self.next_reg,
            slot_bytes: self.places.total,
            code: words,
            call_sites,
            operands,
            lines: self.lines,
            place_names: std::mem::take(&mut self.places.names),
        })
    }
}

/// Folds `copy`/`const`/`arith` triples into [`Instr::ArithImm`].
///
/// Returns the folded instruction list and an `old index -> new index` map
/// (length `code.len() + 1`) so branch targets and line pcs can be remapped.
/// A fold is applied only when each folded temporary register is read exactly
/// once (by the arithmetic), the consumed instructions are not branch targets,
/// and the operands fit the narrow encoding.
fn fold_arith_imm(types: &[TypeInfo], code: &[Instr]) -> (Vec<Instr>, Vec<u32>) {
    let reads = reg_read_count(code);
    let mut is_target = vec![false; code.len() + 1];
    for instr in code {
        match instr {
            Instr::Branch { target } => mark_target(&mut is_target, *target),
            Instr::BranchIf { then, els, .. } => {
                mark_target(&mut is_target, *then);
                mark_target(&mut is_target, *els);
            }
            _ => {}
        }
    }
    let mut new_code = Vec::with_capacity(code.len());
    let mut map = vec![0_u32; code.len() + 1];
    let mut i = 0;
    while i < code.len() {
        let start = u32::try_from(new_code.len()).unwrap_or(u32::MAX);
        if let Some((fused, consumed)) = try_fuse(types, code, i, &reads, &is_target) {
            for offset in 0..consumed {
                map[i + offset] = start;
            }
            new_code.push(fused);
            i += consumed;
        } else {
            map[i] = start;
            new_code.push(code[i].clone());
            i += 1;
        }
    }
    map[code.len()] = u32::try_from(new_code.len()).unwrap_or(u32::MAX);
    for instr in &mut new_code {
        match instr {
            Instr::Branch { target } => *target = remap(&map, *target),
            Instr::BranchIf { then, els, .. } => {
                *then = remap(&map, *then);
                *els = remap(&map, *els);
            }
            _ => {}
        }
    }
    (new_code, map)
}

/// Marks an instruction index as a branch target (ignoring out-of-range ones).
fn mark_target(is_target: &mut [bool], target: u32) {
    if let Some(slot) = is_target.get_mut(target as usize) {
        *slot = true;
    }
}

/// Remaps an old instruction index through `map`, falling back to `0`.
fn remap(map: &[u32], index: u32) -> u32 {
    map.get(index as usize).copied().unwrap_or(0)
}

/// Attempts to fuse the instructions starting at `i`, returning the fused
/// instruction and how many source instructions it consumes.
fn try_fuse(
    types: &[TypeInfo],
    code: &[Instr],
    i: usize,
    reads: &HashMap<u32, u32>,
    is_target: &[bool],
) -> Option<(Instr, usize)> {
    // Shape A: `copy &r <- %s`, `const k -> &j`, `arith dst, &r, &j`.
    if let (
        Some(Instr::Copy {
            dst: Operand::Reg(copy_dst),
            src: Operand::Slot(slot),
            ty: copy_ty,
        }),
        Some(Instr::Const {
            dst: const_dst,
            imm,
            ..
        }),
        Some(Instr::Arith {
            op,
            dst,
            lhs,
            rhs,
            operand_ty,
            ..
        }),
    ) = (code.get(i), code.get(i + 1), code.get(i + 2))
    {
        if lhs == copy_dst
            && rhs == const_dst
            && read_once(reads, *copy_dst)
            && read_once(reads, *const_dst)
            && !is_target.get(i + 1).copied().unwrap_or(true)
            && !is_target.get(i + 2).copied().unwrap_or(true)
            && *copy_dst <= NARROW_MAX
            && *slot <= NARROW_MAX
            && *dst <= NARROW_MAX
            && same_size(types, *copy_ty, *operand_ty)
        {
            return Some((
                Instr::ArithImm {
                    op: *op,
                    dst: *dst,
                    lhs: *slot,
                    lhs_slot: true,
                    imm: *imm,
                    operand_ty: *operand_ty,
                },
                3,
            ));
        }
    }
    // Shape B: `const k -> &j`, `arith dst, &r, &j` (register left operand).
    if let (
        Some(Instr::Const {
            dst: const_dst,
            imm,
            ..
        }),
        Some(Instr::Arith {
            op,
            dst,
            lhs,
            rhs,
            operand_ty,
            ..
        }),
    ) = (code.get(i), code.get(i + 1))
    {
        if rhs == const_dst
            && read_once(reads, *const_dst)
            && !is_target.get(i + 1).copied().unwrap_or(true)
            && *lhs <= NARROW_MAX
            && *dst <= NARROW_MAX
        {
            return Some((
                Instr::ArithImm {
                    op: *op,
                    dst: *dst,
                    lhs: *lhs,
                    lhs_slot: false,
                    imm: *imm,
                    operand_ty: *operand_ty,
                },
                2,
            ));
        }
    }
    None
}

/// Whether a register is read exactly once across `code`.
fn read_once(reads: &HashMap<u32, u32>, reg: u32) -> bool {
    reads.get(&reg).copied() == Some(1)
}

/// Whether two types have the same packed byte size.
fn same_size(types: &[TypeInfo], a: TypeId, b: TypeId) -> bool {
    match (types.get(a as usize), types.get(b as usize)) {
        (Some(a), Some(b)) => a.size == b.size,
        _ => false,
    }
}

/// Counts how many times each register is read across `code`.
fn reg_read_count(code: &[Instr]) -> HashMap<u32, u32> {
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut bump = |reg: u32| {
        *counts.entry(reg).or_insert(0) += 1;
    };
    for instr in code {
        match instr {
            Instr::Copy {
                src: Operand::Reg(reg),
                ..
            }
            | Instr::Store {
                src: Operand::Reg(reg),
                ..
            }
            | Instr::StoreField {
                src: Operand::Reg(reg),
                ..
            }
            | Instr::Convert { src: reg, .. }
            | Instr::BranchIf { cond: reg, .. } => bump(*reg),
            Instr::Arith { lhs, rhs, .. } => {
                bump(*lhs);
                bump(*rhs);
            }
            Instr::ArithImm { lhs, lhs_slot, .. } => {
                if !lhs_slot {
                    bump(*lhs);
                }
            }
            Instr::CallIndirect { callee, ins, .. } => {
                bump(*callee);
                bump_operands(ins, &mut bump);
            }
            Instr::Call { ins, .. } | Instr::CallBuiltin { ins, .. } => {
                bump_operands(ins, &mut bump);
            }
            _ => {}
        }
    }
    counts
}

/// Counts register reads in an operand list.
fn bump_operands(ins: &[Operand], bump: &mut impl FnMut(u32)) {
    for operand in ins {
        if let Operand::Reg(reg) = operand {
            bump(*reg);
        }
    }
}

/// Forwards a single-use staging copy into the call that consumes it.
///
/// `%s = copy &r` immediately followed by a call whose inputs read `%s`
/// becomes a call reading `&r` directly, removing the slot round-trip. This is
/// safe only when the slot is written once, read once, and is never a call
/// output (checked by the caller).
fn fold_call_inputs(code: &[Instr]) -> (Vec<Instr>, Vec<u32>) {
    let (reads, writes) = slot_access_counts(code);
    let mut is_out: HashSet<u32> = HashSet::new();
    for instr in code {
        match instr {
            Instr::Call { outs, .. }
            | Instr::CallIndirect { outs, .. }
            | Instr::CallBuiltin { outs, .. } => {
                is_out.extend(outs.iter().copied());
            }
            _ => {}
        }
    }
    let mut is_target = vec![false; code.len() + 1];
    for instr in code {
        match instr {
            Instr::Branch { target } => mark_target(&mut is_target, *target),
            Instr::BranchIf { then, els, .. } => {
                mark_target(&mut is_target, *then);
                mark_target(&mut is_target, *els);
            }
            _ => {}
        }
    }
    let mut new_code = Vec::with_capacity(code.len());
    let mut map = vec![0_u32; code.len() + 1];
    let mut i = 0;
    while i < code.len() {
        let start = u32::try_from(new_code.len()).unwrap_or(u32::MAX);
        let folded = match (code.get(i), code.get(i + 1)) {
            (
                Some(Instr::Copy {
                    dst: Operand::Slot(slot),
                    src: Operand::Reg(reg),
                    ..
                }),
                Some(call),
            ) if reads.get(slot).copied() == Some(1)
                && writes.get(slot).copied() == Some(1)
                && !is_out.contains(slot)
                && !is_target.get(i + 1).copied().unwrap_or(true) =>
            {
                forward_call(call, *slot, *reg)
            }
            _ => None,
        };
        if let Some(call) = folded {
            map[i] = start;
            map[i + 1] = start;
            new_code.push(call);
            i += 2;
        } else {
            map[i] = start;
            new_code.push(code[i].clone());
            i += 1;
        }
    }
    map[code.len()] = u32::try_from(new_code.len()).unwrap_or(u32::MAX);
    for instr in &mut new_code {
        match instr {
            Instr::Branch { target } => *target = remap(&map, *target),
            Instr::BranchIf { then, els, .. } => {
                *then = remap(&map, *then);
                *els = remap(&map, *els);
            }
            _ => {}
        }
    }
    (new_code, map)
}

/// Replaces a call's `Slot(slot)` input with `Reg(reg)`, if present.
fn forward_call(call: &Instr, slot: u32, reg: u32) -> Option<Instr> {
    let mut out = call.clone();
    let (Instr::Call { ins, .. }
    | Instr::CallIndirect { ins, .. }
    | Instr::CallBuiltin { ins, .. }) = &mut out
    else {
        return None;
    };
    let mut found = false;
    for operand in ins.iter_mut() {
        if *operand == Operand::Slot(slot) {
            *operand = Operand::Reg(reg);
            found = true;
        }
    }
    found.then_some(out)
}

/// Counts reads and writes of each frame slot across `code`.
///
/// Register reads of a slot's value count as reads; a call output counts as a
/// runtime write to its slot.
fn slot_access_counts(code: &[Instr]) -> (HashMap<u32, u32>, HashMap<u32, u32>) {
    let mut reads: HashMap<u32, u32> = HashMap::new();
    let mut writes: HashMap<u32, u32> = HashMap::new();
    for instr in code {
        match instr {
            Instr::Copy { dst, src, .. } => {
                bump_slot(&mut reads, *src);
                bump_slot(&mut writes, *dst);
            }
            Instr::CopyBytes { dst, src, .. } => {
                bump_slot(&mut reads, Operand::Slot(*src));
                bump_slot(&mut writes, Operand::Slot(*dst));
            }
            Instr::Load { src, .. } => bump_slot(&mut reads, Operand::Slot(*src)),
            Instr::Store { src, dst, .. } => {
                bump_slot(&mut reads, *src);
                bump_slot(&mut reads, Operand::Slot(*dst));
            }
            Instr::Addr { place, .. } | Instr::AddrField { place, .. } => {
                bump_slot(&mut reads, Operand::Slot(*place));
            }
            Instr::Field { place, .. } => bump_slot(&mut reads, Operand::Slot(*place)),
            Instr::StoreField { src, place, .. } => {
                bump_slot(&mut reads, *src);
                bump_slot(&mut reads, Operand::Slot(*place));
            }
            Instr::Call { ins, outs, .. }
            | Instr::CallIndirect { ins, outs, .. }
            | Instr::CallBuiltin { ins, outs, .. } => {
                for operand in ins {
                    bump_slot(&mut reads, *operand);
                }
                for out in outs {
                    bump_slot(&mut writes, Operand::Slot(*out));
                }
            }
            _ => {}
        }
    }
    (reads, writes)
}

/// Bumps a slot's counter if `operand` is a slot.
fn bump_slot(counts: &mut HashMap<u32, u32>, operand: Operand) {
    if let Operand::Slot(off) = operand {
        *counts.entry(off).or_insert(0) += 1;
    }
}

/// Removes `branch` instructions whose target is the immediately following
/// instruction (an explicit fall-through), then remaps targets and line pcs.
fn drop_fallthrough_branches(code: &[Instr]) -> (Vec<Instr>, Vec<u32>) {
    let mut new_code = Vec::with_capacity(code.len());
    let mut map = vec![0_u32; code.len() + 1];
    for (i, instr) in code.iter().enumerate() {
        let falls_through = matches!(instr, Instr::Branch { target } if *target as usize == i + 1);
        map[i] = u32::try_from(new_code.len()).unwrap_or(u32::MAX);
        if !falls_through {
            new_code.push(instr.clone());
        }
    }
    map[code.len()] = u32::try_from(new_code.len()).unwrap_or(u32::MAX);
    for instr in &mut new_code {
        match instr {
            Instr::Branch { target } => *target = remap(&map, *target),
            Instr::BranchIf { then, els, .. } => {
                *then = remap(&map, *then);
                *els = remap(&map, *els);
            }
            _ => {}
        }
    }
    (new_code, map)
}

/// Program-wide compilation state.
struct Compiler<'a> {
    module: &'a LirModule,
    source_map: &'a SourceMap,
    types: TypeTable,
    data: Vec<Data>,
    globals: Vec<u8>,
    func_ids: HashMap<&'a str, u32>,
    data_ids: HashMap<&'a str, u32>,
    byte_buffers: HashMap<u32, u32>,
}

impl<'a> Compiler<'a> {
    /// Builds the compiler, emitting the module's raw data constants.
    fn new(module: &'a LirModule, source_map: &'a SourceMap) -> Result<Self, String> {
        let mut compiler = Self {
            module,
            source_map,
            types: TypeTable::new(),
            data: Vec::new(),
            globals: Vec::new(),
            func_ids: module
                .funcs
                .iter()
                .enumerate()
                .map(|(index, func)| {
                    u32::try_from(index)
                        .map(|index| (func.name.as_str(), index))
                        .map_err(|_| "too many functions".to_string())
                })
                .collect::<Result<_, _>>()?,
            data_ids: HashMap::new(),
            byte_buffers: HashMap::new(),
        };
        for datum in &module.data {
            let bytes = match &datum.kind {
                LirDataKind::Str(text) | LirDataKind::NumLit(text) => text.as_bytes().to_vec(),
            };
            let index = compiler.push_data(bytes)?;
            compiler.data_ids.insert(datum.name.as_str(), index);
        }
        Ok(compiler)
    }

    /// Appends a data entry, returning its index; globals stay gap-free.
    fn push_data(&mut self, bytes: Vec<u8>) -> Result<u32, String> {
        let global_off =
            u32::try_from(self.globals.len()).map_err(|_| "globals arena too large".to_string())?;
        let index =
            u32::try_from(self.data.len()).map_err(|_| "too many data constants".to_string())?;
        self.globals.extend_from_slice(&bytes);
        self.data.push(Data { bytes, global_off });
        Ok(index)
    }

    /// A function index by name.
    fn func_id(&self, name: &str) -> Result<u32, String> {
        self.func_ids
            .get(name)
            .copied()
            .ok_or_else(|| format!("unknown function `@{name}`"))
    }

    /// A data index by name (without the `!` sigil).
    fn data_id(&self, name: &str) -> Result<u32, String> {
        self.data_ids
            .get(name)
            .copied()
            .ok_or_else(|| format!("unknown data constant `!{name}`"))
    }

    /// A `#byte_buffer` constant's `{len, ptr}` struct data index.
    ///
    /// The raw text entry keeps its own index; `ptr` is that entry's
    /// `global_off` (globals occupy the arena's low range, so a global offset
    /// is a valid arena ref).
    fn byte_buffer_struct(&mut self, raw: u32) -> Result<u32, String> {
        if let Some(&index) = self.byte_buffers.get(&raw) {
            return Ok(index);
        }
        let entry = self
            .data
            .get(raw as usize)
            .ok_or_else(|| format!("data index {raw} out of range"))?;
        let len = entry.bytes.len() as u64;
        let ptr = u64::from(entry.global_off);
        let mut bytes = Vec::with_capacity(2 * usize::try_from(VM_POINTER_BYTES).unwrap_or(8));
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(&ptr.to_le_bytes());
        let index = self.push_data(bytes)?;
        self.byte_buffers.insert(raw, index);
        Ok(index)
    }

    /// Compiles one LIR function.
    fn compile_func(&mut self, func: &LirFunc) -> Result<Func, String> {
        let mut places = Places::default();
        // Outputs are ref-backed and occupy the frame's low out-ref region
        // (8 bytes each); inputs and locals follow. `rebuild_layout` computes
        // matching input offsets.
        for (name, ty) in &func.outs {
            places.declare(self.module, name, ty, true)?;
        }
        for (name, ty) in &func.ins {
            places.declare(self.module, name, ty, false)?;
        }
        if !func.is_extern {
            for block in &func.blocks {
                for inst in &block.insts {
                    if let LirInst::Alloca { ty, place, .. } = inst {
                        places.declare(self.module, place, ty, false)?;
                    }
                }
            }
        }
        let builder = FuncBuilder {
            compiler: self,
            func,
            places,
            block_pc: HashMap::new(),
            regs: HashMap::new(),
            reg_types: HashMap::new(),
            next_reg: 0,
            code: Vec::new(),
            lines: Vec::new(),
        };
        builder.run()
    }
}

/// Compiles `.lilac` source to a bytecode program for the VM target.
///
/// Shared by the crate's own tests and the interpreter/debugger tests; it is
/// compiled only for test builds.
#[cfg(test)]
pub(crate) fn compile_src(src: &str) -> Program {
    use lilac_driver::{Session, Target};
    let mut session = Session::new();
    session.target = Target::parse("lilacvm-unknown-none").expect("target");
    let module = session
        .compile_lir_with_stages("t.lilac", src, &mut |_: &'static str, _: &LirModule| {})
        .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
    compile(&module, &session.source_map).expect("compile")
}

#[cfg(test)]
mod tests {
    use super::compile_src;
    use crate::decode::{decode, Decoded};
    use crate::isa::{Func, Program};

    /// Decodes a function's whole word stream for structural assertions.
    fn decoded(func: &Func) -> Vec<Decoded<'_>> {
        let mut out = Vec::new();
        let mut pc = 0_usize;
        while pc < func.code.len() {
            let (instr, next) = decode(&func.code, pc, &func.call_sites, &func.operands)
                .unwrap_or_else(|err| panic!("decode at {pc}: {err}"));
            out.push(instr);
            pc = next as usize;
        }
        out
    }

    fn func<'a>(program: &'a Program, name: &str) -> &'a Func {
        program
            .funcs
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("function {name}"))
    }

    #[test]
    fn compiles_arith_into_registers() {
        let program = compile_src(
            "main: function (a: @bit_native, b: @bit_native) nothing {\n    x: @bit_native = @i_add(a, b)\n}\n",
        );
        let main = func(&program, "main");
        assert!(decoded(main)
            .iter()
            .any(|i| matches!(i, Decoded::IAdd { .. })));
        assert!(main.n_regs >= 3);
    }

    #[test]
    fn compiles_line_table_from_spans() {
        let program = compile_src("main: function () nothing {\n    x: @bit_native = 0x1\n}\n");
        let main = program
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("main");
        assert!(!main.lines.is_empty());
        assert!(main.lines.iter().all(|l| l.line >= 1));
    }

    #[test]
    fn data_consts_get_global_offsets() {
        let program = compile_src("main: function () nothing {\n    s: @byte_buffer = \"hi\"\n}\n");
        assert!(!program.data.is_empty());
        assert!(program.globals.len() >= 2);
    }

    /// `format_args_push` fields a `ref #format_args` (indirect); the
    /// `format_args_*` by-value bodies field a direct `#format_args`.
    const FORMAT_ARGS_SRC: &str = "main: function () nothing {\n    part: @byte_buffer = \"x\"\n    msg: @format_args = $\"a{part}b\"\n    n: @bit_native = @format_args_count(msg)\n    first: @byte_buffer = @format_args_piece(msg, 0x0)\n}\n";

    fn field_flags(program: &crate::isa::Program) -> (bool, bool) {
        let mut saw_indirect = false;
        let mut saw_direct = false;
        for func in &program.funcs {
            for instr in decoded(func) {
                match instr {
                    Decoded::Field { indirect, .. }
                    | Decoded::AddrField { indirect, .. }
                    | Decoded::StoreField { indirect, .. } => {
                        if indirect {
                            saw_indirect = true;
                        } else {
                            saw_direct = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        (saw_indirect, saw_direct)
    }

    #[test]
    fn field_indirection_tracks_object_refness() {
        let program = compile_src(FORMAT_ARGS_SRC);
        let (saw_indirect, saw_direct) = field_flags(&program);
        assert!(saw_indirect, "expected a ref-object field access");
        assert!(saw_direct, "expected a direct-object field access");

        // `format_args_push`'s object (`%table`) is a `ref #format_args`.
        let push = func(&program, ".lilac.format_args_push");
        assert!(decoded(push).iter().any(|i| matches!(
            i,
            Decoded::Field { indirect: true, .. } | Decoded::StoreField { indirect: true, .. }
        )));

        // `format_args_count`'s object (`%table`) is a direct `#format_args`.
        let count = func(&program, ".lilac.format_args_count");
        assert!(decoded(count).iter().any(|i| matches!(
            i,
            Decoded::Field {
                indirect: false,
                ..
            }
        )));
    }

    /// A ref-object field read at an `off` past the *frame's* direct extent must
    /// compile: the slot holds a pointer, so `off` is relative to the pointee.
    /// The `get` frame is `instance(8) + result(1)`, yet `instance.b` is at
    /// offset 8 — the old frame-extent check rejected it and tripped `bug!`.
    #[test]
    fn ref_object_field_beyond_frame_extent_compiles() {
        let program = compile_src(
            "Pair: object {\n    a: @bit64\n    b: @bit64\n    get: function (instance: ref Pair) result: @bit8 {\n        result = @truncate(instance.b)\n    }\n}\nmain: function () nothing {\n    p: Pair = Pair {\n        a = 0x1\n        b = 0x2\n    }\n    r: @bit8 = p.get()\n}\n",
        );
        let get = func(&program, "Pair.get");
        assert!(decoded(get).iter().any(|i| matches!(
            i,
            Decoded::Field {
                indirect: true,
                off: 8,
                ..
            }
        )));
    }
}
