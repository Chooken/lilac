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

use std::collections::HashMap;

use lilac_ir::{builtin_layout, LirDataKind, LirFunc, LirInst, LirModule, LirOperand, LirTerm};
use lilac_span::{SourceMap, Span};

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
    let program = Program {
        version: LBC_VERSION.to_owned(),
        target: VM_TARGET.to_owned(),
        types: compiler.types.infos,
        data: compiler.data,
        globals: compiler.globals,
        funcs,
    };
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
#[derive(Default)]
struct Places {
    offsets: HashMap<String, u32>,
    types: HashMap<String, String>,
    names: Vec<(String, u32)>,
    total: u32,
}

impl Places {
    /// Declares a place, packing it after all previously declared places.
    fn declare(&mut self, module: &LirModule, name: &str, ty: &str) -> Result<(), String> {
        if self.offsets.contains_key(name) {
            return Err(format!("duplicate place `%{name}`"));
        }
        let size = type_size(module, ty, 0)?;
        let off = self.total;
        self.total = self
            .total
            .checked_add(size)
            .ok_or_else(|| format!("frame too large for `{name}`"))?;
        self.offsets.insert(name.to_owned(), off);
        self.types.insert(name.to_owned(), ty.to_owned());
        self.names.push((name.to_owned(), off));
        Ok(())
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
    next_reg: u32,
    code: Vec<Instr>,
    lines: Vec<Line>,
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
                if is_indirect(self.compiler.module, ty)? {
                    let dst = self.def_reg(dst);
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
                    let dst = self.def_reg(dst);
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
                let dst_slot = self.slot(dst)?;
                let dst_ty = self.places.ty(dst)?.to_owned();
                match src {
                    LirOperand::Place(place) => {
                        let size = type_size(self.compiler.module, &dst_ty, 0)?;
                        let src = self.slot(place)?;
                        self.emit(
                            Instr::CopyBytes {
                                dst: dst_slot,
                                src,
                                size,
                            },
                            *span,
                        )
                    }
                    LirOperand::Reg(reg) => {
                        let ty = self.intern(&dst_ty)?;
                        let src = Operand::Reg(self.reg(reg)?);
                        self.emit(
                            Instr::Copy {
                                dst: Operand::Slot(dst_slot),
                                src,
                                ty,
                            },
                            *span,
                        )
                    }
                }
            }
            LirInst::Const { dst, lit, ty, span } => {
                let dst = self.def_reg(dst);
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
                let dst = self.def_reg(dst);
                let src = self.slot(src)?;
                let ty = self.intern(ty)?;
                self.emit(Instr::Load { dst, src, ty }, *span)
            }
            LirInst::Addr {
                place, dst, span, ..
            } => {
                let dst = self.def_reg(dst);
                let place = self.slot(place)?;
                self.emit(Instr::Addr { dst, place }, *span)
            }
            LirInst::AddrFunc {
                func, dst, span, ..
            } => {
                let dst = self.def_reg(dst);
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
                    .map(|place| self.slot(place).map(Operand::Slot))
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
                let dst = self.def_reg(dst);
                let place = self.slot(obj)?;
                let off = self.field_off(obj, *idx)?;
                let indirect = is_ref_ty(self.places.ty(obj)?);
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
                let indirect = is_ref_ty(self.places.ty(obj)?);
                let field_ty = field_type(self.compiler.module, self.places.ty(obj)?, *idx)?;
                let src = self.operand(value)?;
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
                let dst_slot = self.slot(dst)?;
                let pointee = strip_ref(self.places.ty(dst)?).to_owned();
                let src = self.operand(src)?;
                let ty = self.intern(&pointee)?;
                self.emit(
                    Instr::Store {
                        src,
                        dst: dst_slot,
                        ty,
                    },
                    *span,
                )
            }
            LirInst::Convert {
                op,
                src,
                dst,
                ty,
                span,
            } => {
                let dst = self.def_reg(dst);
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
                let dst = self.def_reg(dst);
                let place = self.slot(obj)?;
                let off = self.field_off(obj, *idx)?;
                let indirect = is_ref_ty(self.places.ty(obj)?);
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
                let dst = self.def_reg(dst);
                let lhs = self.reg(lhs)?;
                let rhs = self.reg(rhs)?;
                let ty = self.intern(ty)?;
                self.emit(
                    Instr::Arith {
                        op: map_arith(op),
                        dst,
                        lhs,
                        rhs,
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
    fn emit_term(&mut self, term: &LirTerm, next: Option<&str>) -> Result<(), String> {
        match term {
            LirTerm::Return { span } => self.emit(Instr::Return, *span),
            LirTerm::Branch { target, span } => {
                let target = self.label_pc(target)?;
                self.emit(Instr::Branch { target }, *span)
            }
            LirTerm::BranchIf {
                cond,
                then,
                els,
                span,
            } => {
                let cond = self.reg(cond)?;
                let then = self.label_pc(then)?;
                let els = if let Some(label) = els {
                    self.label_pc(label)?
                } else {
                    let label = next.ok_or_else(|| {
                        "`branch.if` without an else is the last block".to_string()
                    })?;
                    self.label_pc(label)?
                };
                self.emit(Instr::BranchIf { cond, then, els }, *span)
            }
            LirTerm::Panic { msg, span } => {
                let data = self.compiler.data_id(msg)?;
                self.emit(Instr::Panic { data }, *span)
            }
            LirTerm::Unreachable { span } => self.emit(Instr::Unreachable, *span),
        }
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
                n_regs: 0,
                slot_bytes: self.places.total,
                code: Vec::new(),
                lines: Vec::new(),
                place_names: std::mem::take(&mut self.places.names),
            });
        }
        let mut pc = 0_u32;
        for block in &func.blocks {
            self.block_pc.insert(block.label.clone(), pc);
            for inst in &block.insts {
                pc = pc
                    .checked_add(emitted_count(inst))
                    .ok_or_else(|| "program too large".to_string())?;
            }
            pc = pc
                .checked_add(1)
                .ok_or_else(|| "program too large".to_string())?;
        }
        let block_count = func.blocks.len();
        for index in 0..block_count {
            let block = &func.blocks[index];
            self.regs.clear();
            for inst in &block.insts {
                self.emit_inst(inst)?;
            }
            let next = func.blocks.get(index + 1).map(|b| b.label.as_str());
            self.emit_term(&block.term, next)?;
        }
        Ok(Func {
            name: func.name.clone(),
            is_extern: false,
            ins,
            outs,
            n_regs: self.next_reg,
            slot_bytes: self.places.total,
            code: self.code,
            lines: self.lines,
            place_names: std::mem::take(&mut self.places.names),
        })
    }
}

/// How many bytecode instructions one LIR instruction emits.
fn emitted_count(inst: &LirInst) -> u32 {
    match inst {
        LirInst::Alloca { .. }
        | LirInst::Drop { .. }
        | LirInst::CompileError { .. }
        | LirInst::FoldIntrinsic { .. } => 0,
        _ => 1,
    }
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
        for (name, ty) in &func.ins {
            places.declare(self.module, name, ty)?;
        }
        for (name, ty) in &func.outs {
            places.declare(self.module, name, ty)?;
        }
        if !func.is_extern {
            for block in &func.blocks {
                for inst in &block.insts {
                    if let LirInst::Alloca { ty, place, .. } = inst {
                        places.declare(self.module, place, ty)?;
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
    use crate::isa::Instr;

    #[test]
    fn compiles_arith_into_registers() {
        let program = compile_src(
            "main: function (a: @bit_native, b: @bit_native) nothing {\n    x: @bit_native = @i_add(a, b)\n}\n",
        );
        let main = program
            .funcs
            .iter()
            .find(|f| f.name == "main")
            .expect("main");
        assert!(main.code.iter().any(|i| matches!(i, Instr::Arith { .. })));
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
            for instr in &func.code {
                match instr {
                    Instr::Field { indirect, .. }
                    | Instr::AddrField { indirect, .. }
                    | Instr::StoreField { indirect, .. } => {
                        if *indirect {
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
        let push = program
            .funcs
            .iter()
            .find(|f| f.name == ".lilac.format_args_push")
            .expect("push");
        assert!(push.code.iter().any(|i| matches!(
            i,
            Instr::Field { indirect: true, .. } | Instr::StoreField { indirect: true, .. }
        )));

        // `format_args_count`'s object (`%table`) is a direct `#format_args`.
        let count = program
            .funcs
            .iter()
            .find(|f| f.name == ".lilac.format_args_count")
            .expect("count");
        assert!(count.code.iter().any(|i| matches!(
            i,
            Instr::Field {
                indirect: false,
                ..
            }
        )));
    }
}
