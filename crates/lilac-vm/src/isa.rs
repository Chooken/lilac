//! Bytecode ISA, program model, and verifier.
//!
//! The Lilac bytecode is a register machine over a per-frame place region. All
//! operands are resolved to integers at compile time: registers are `u32`
//! indices into the frame's register file, place operands are `u32` byte
//! offsets into the frame's slot region, and branch targets are `u32` program
//! counters. No string is decoded on the dispatch hot path.

use std::collections::{BTreeSet, HashSet};

/// Version of the `.lbc` text format.
pub const LBC_VERSION: &str = "0.1";

/// Canonical VM target triple.
pub const VM_TARGET: &str = "lilacvm-unknown-none";

/// Pointer width in bytes on the VM (all VM pointers are 64-bit).
pub const VM_POINTER_BYTES: u32 = 8;

/// Maximum size of the globals arena, in bytes.
///
/// The verifier and text parser reject a data layout whose implied arena
/// exceeds this bound, so an untrusted `global_off` cannot request an
/// arbitrarily large zero-fill.
pub(crate) const MAX_GLOBALS_BYTES: u32 = 64 * 1024 * 1024;

/// Index into a [`Program`]'s type table.
pub type TypeId = u32;

/// One entry in the program type table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeInfo {
    /// Printable type name (`#bit8`, `ref #Thing`, ...).
    pub name: String,
    /// Size in bytes (packed layout; alignment is ignored).
    pub size: u32,
    /// Whether values of this type are references into the arena.
    pub is_ref: bool,
}

/// A data constant mapped into [`Program::globals`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Data {
    /// Raw constant bytes (string or number-literal source).
    pub bytes: Vec<u8>,
    /// Byte offset of these bytes within [`Program::globals`].
    pub global_off: u32,
}

/// A source-line mapping for one program counter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Program counter the line applies to.
    pub pc: u32,
    /// Source file name recorded at compile time.
    pub file: String,
    /// 1-based source line.
    pub line: u32,
    /// 1-based source column.
    pub col: u32,
}

/// A value operand: a register index or a slot byte offset.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Operand {
    /// Register-file index.
    Reg(u32),
    /// Byte offset within the frame's slot region.
    Slot(u32),
}

/// A compiler leaf builtin.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Builtin {
    /// `@ptr_add(ref, int) -> ref`.
    PtrAdd,
    /// `@memcpy(dst, src, len)`.
    Memcpy,
    /// `@lilac_abort()`.
    Abort,
}

impl Builtin {
    /// Returns the canonical mnemonic (without the `@` sigil).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PtrAdd => "ptr_add",
            Self::Memcpy => "memcpy",
            Self::Abort => "lilac_abort",
        }
    }

    /// Parses a builtin mnemonic (without the `@` sigil).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ptr_add" => Some(Self::PtrAdd),
            "memcpy" => Some(Self::Memcpy),
            "lilac_abort" => Some(Self::Abort),
            _ => None,
        }
    }

    /// Returns `(input count, output count)` for this builtin.
    #[must_use]
    pub fn arity(self) -> (usize, usize) {
        match self {
            Self::PtrAdd => (2, 1),
            Self::Memcpy => (3, 0),
            Self::Abort => (0, 0),
        }
    }
}

/// Arithmetic/comparison operators (mirrors `lilac_ir::ArithOp`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ArithOp {
    /// `i.add`.
    IAdd,
    /// `i.sub`.
    ISub,
    /// `i.mul`.
    IMul,
    /// `i.div`.
    IDiv,
    /// `i.div.u`.
    IDivU,
    /// `i.mod`.
    IMod,
    /// `i.mod.u`.
    IModU,
    /// `f.add` (bitwise-float add).
    FAdd,
    /// `f.sub`.
    FSub,
    /// `f.mul`.
    FMul,
    /// `f.div`.
    FDiv,
    /// `f.mod`.
    FMod,
    /// `cmp.eql` (bit-equality).
    Eq,
    /// `i.cmp.lt`.
    ILt,
    /// `i.cmp.lt.u`.
    ILtU,
    /// `i.cmp.gt`.
    IGt,
    /// `i.cmp.gt.u`.
    IGtU,
    /// `f.cmp.lt`.
    FLt,
    /// `f.cmp.gt`.
    FGt,
}

impl ArithOp {
    /// Returns the canonical mnemonic.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IAdd => "i.add",
            Self::ISub => "i.sub",
            Self::IMul => "i.mul",
            Self::IDiv => "i.div",
            Self::IDivU => "i.div.u",
            Self::IMod => "i.mod",
            Self::IModU => "i.mod.u",
            Self::FAdd => "f.add",
            Self::FSub => "f.sub",
            Self::FMul => "f.mul",
            Self::FDiv => "f.div",
            Self::FMod => "f.mod",
            Self::Eq => "cmp.eql",
            Self::ILt => "i.cmp.lt",
            Self::ILtU => "i.cmp.lt.u",
            Self::IGt => "i.cmp.gt",
            Self::IGtU => "i.cmp.gt.u",
            Self::FLt => "f.cmp.lt",
            Self::FGt => "f.cmp.gt",
        }
    }

    /// Parses a canonical mnemonic.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "i.add" => Self::IAdd,
            "i.sub" => Self::ISub,
            "i.mul" => Self::IMul,
            "i.div" => Self::IDiv,
            "i.div.u" => Self::IDivU,
            "i.mod" => Self::IMod,
            "i.mod.u" => Self::IModU,
            "f.add" => Self::FAdd,
            "f.sub" => Self::FSub,
            "f.mul" => Self::FMul,
            "f.div" => Self::FDiv,
            "f.mod" => Self::FMod,
            "cmp.eql" => Self::Eq,
            "i.cmp.lt" => Self::ILt,
            "i.cmp.lt.u" => Self::ILtU,
            "i.cmp.gt" => Self::IGt,
            "i.cmp.gt.u" => Self::IGtU,
            "f.cmp.lt" => Self::FLt,
            "f.cmp.gt" => Self::FGt,
            _ => return None,
        })
    }
}

/// Width-conversion operators (mirrors `lilac_ir::ConvertOp`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConvertOp {
    /// `trunc` (narrowing).
    Trunc,
    /// `extend.zero` (zero-extension).
    ExtendZero,
}

impl ConvertOp {
    /// Returns the canonical mnemonic.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trunc => "trunc",
            Self::ExtendZero => "extend.zero",
        }
    }

    /// Parses a canonical mnemonic.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "trunc" => Some(Self::Trunc),
            "extend.zero" => Some(Self::ExtendZero),
            _ => None,
        }
    }
}

/// One decoded bytecode instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instr {
    /// `dst: ty = const imm` (integer/float literal bits).
    Const {
        /// Destination register.
        dst: u32,
        /// Literal bits.
        imm: u64,
        /// Value type.
        ty: TypeId,
    },
    /// `dst: ty = const.data !cN` (reference to a data constant).
    ConstData {
        /// Destination register.
        dst: u32,
        /// Data index.
        data: u32,
        /// Value type.
        ty: TypeId,
    },
    /// Scalar copy (width from `ty`).
    Copy {
        /// Destination operand.
        dst: Operand,
        /// Source operand.
        src: Operand,
        /// Value type.
        ty: TypeId,
    },
    /// Aggregate place-to-place byte copy.
    CopyBytes {
        /// Destination slot offset.
        dst: u32,
        /// Source slot offset.
        src: u32,
        /// Byte count.
        size: u32,
    },
    /// Read `size(ty)` bytes at the reference held in `src`.
    Load {
        /// Destination register.
        dst: u32,
        /// Slot holding the reference.
        src: u32,
        /// Pointee type.
        ty: TypeId,
    },
    /// Write `size(ty)` bytes through the reference held in `dst`.
    Store {
        /// Value operand.
        src: Operand,
        /// Slot holding the reference.
        dst: u32,
        /// Value type.
        ty: TypeId,
    },
    /// `dst = addr slot`.
    Addr {
        /// Destination register.
        dst: u32,
        /// Slot whose address is taken.
        place: u32,
    },
    /// `dst = addr.field slot + off`.
    AddrField {
        /// Destination register.
        dst: u32,
        /// Base slot.
        place: u32,
        /// Field byte offset.
        off: u32,
    },
    /// `dst = addr.func func`.
    AddrFunc {
        /// Destination register.
        dst: u32,
        /// Function index.
        func: u32,
    },
    /// Scalar field read.
    Field {
        /// Destination register.
        dst: u32,
        /// Object slot.
        place: u32,
        /// Field byte offset.
        off: u32,
        /// Field type.
        ty: TypeId,
    },
    /// Scalar field write.
    StoreField {
        /// Value operand.
        src: Operand,
        /// Object slot.
        place: u32,
        /// Field byte offset.
        off: u32,
        /// Field type.
        ty: TypeId,
    },
    /// Typed arithmetic/comparison.
    Arith {
        /// Operator.
        op: ArithOp,
        /// Destination register.
        dst: u32,
        /// Left register.
        lhs: u32,
        /// Right register.
        rhs: u32,
        /// Result type.
        ty: TypeId,
    },
    /// Width conversion.
    Convert {
        /// Operator.
        op: ConvertOp,
        /// Destination register.
        dst: u32,
        /// Source register.
        src: u32,
        /// Result type.
        ty: TypeId,
    },
    /// Direct call by function index.
    Call {
        /// Callee function index.
        func: u32,
        /// Input operands.
        ins: Vec<Operand>,
        /// Output slot offsets.
        outs: Vec<u32>,
    },
    /// Indirect call through a register holding a function id.
    CallIndirect {
        /// Register holding the callee.
        callee: u32,
        /// Input operands.
        ins: Vec<Operand>,
        /// Output slot offsets.
        outs: Vec<u32>,
    },
    /// Call to a compiler leaf builtin.
    CallBuiltin {
        /// Builtin.
        builtin: Builtin,
        /// Input operands.
        ins: Vec<Operand>,
        /// Output slot offsets.
        outs: Vec<u32>,
    },
    /// Unconditional branch.
    Branch {
        /// Target program counter.
        target: u32,
    },
    /// Conditional branch.
    BranchIf {
        /// Condition register.
        cond: u32,
        /// Taken target.
        then: u32,
        /// Untaken target.
        els: u32,
    },
    /// Return from the current function.
    Return,
    /// Print `data` bytes to stderr and abort non-zero.
    Panic {
        /// Data index.
        data: u32,
    },
    /// VM internal trap.
    Unreachable,
}

/// One compiled function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Func {
    /// Function name (without sigil).
    pub name: String,
    /// Whether this is an `extern` signature-only declaration.
    pub is_extern: bool,
    /// Input types.
    pub ins: Vec<TypeId>,
    /// Output types.
    pub outs: Vec<TypeId>,
    /// Register-file slots for a frame of this function.
    pub n_regs: u32,
    /// Per-frame place region size in bytes (zeroed at entry).
    pub slot_bytes: u32,
    /// Instructions in program-counter order.
    pub code: Vec<Instr>,
    /// Source-line table, sorted by `pc`.
    pub lines: Vec<Line>,
    /// Original `%place` name to slot offset (debugger metadata).
    pub place_names: Vec<(String, u32)>,
}

impl Func {
    /// Returns the set of program counters that begin a labeled block.
    ///
    /// A pc is a label if it is `0` or the target of any branch.
    #[must_use]
    pub fn labels(&self) -> BTreeSet<u32> {
        let mut set = BTreeSet::new();
        if !self.code.is_empty() {
            set.insert(0);
        }
        for instr in &self.code {
            match instr {
                Instr::Branch { target } => {
                    set.insert(*target);
                }
                Instr::BranchIf { then, els, .. } => {
                    set.insert(*then);
                    set.insert(*els);
                }
                _ => {}
            }
        }
        set
    }
}

/// A whole bytecode program: versioned, target-tagged, and self-contained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    /// `.lbc` format version.
    pub version: String,
    /// Target triple (must be [`VM_TARGET`]).
    pub target: String,
    /// Type table.
    pub types: Vec<TypeInfo>,
    /// Data constants.
    pub data: Vec<Data>,
    /// Concatenated data bytes; references index into this arena.
    ///
    /// Canonical form: this must equal the arena implied by the [`Data`]
    /// offsets (each entry's bytes placed at its `global_off`, with any gaps
    /// zero-filled). [`verify`] rejects any program that violates this.
    pub globals: Vec<u8>,
    /// Functions.
    pub funcs: Vec<Func>,
}

/// Verifies structural well-formedness of a program.
///
/// Checks the version and target, then every function's signatures and
/// instructions: register/slot bounds, branch-target bounds, data and function
/// indices, and call arity.
///
/// # Errors
///
/// Returns a human-readable message describing the first violation found.
pub fn verify(program: &Program) -> Result<(), String> {
    if program.version != LBC_VERSION {
        return Err(format!(
            "unsupported .lbc version `{}` (expected `{}`)",
            program.version, LBC_VERSION
        ));
    }
    if program.target != VM_TARGET {
        return Err(format!(
            "target `{}` is not the VM target `{}`",
            program.target, VM_TARGET
        ));
    }
    let mut seen_types = HashSet::new();
    for info in &program.types {
        if !seen_types.insert(info.name.as_str()) {
            return Err(format!("duplicate type name `{}`", info.name));
        }
    }
    let mut seen_funcs = HashSet::new();
    for func in &program.funcs {
        if !seen_funcs.insert(func.name.as_str()) {
            return Err(format!("duplicate function name `{}`", func.name));
        }
    }
    if derive_globals(&program.data)? != program.globals {
        return Err("globals arena does not match the data constants".to_string());
    }
    for func in &program.funcs {
        verify_func(program, func)?;
    }
    Ok(())
}

/// Reconstructs the globals arena implied by the data-constant offsets.
///
/// Each entry's bytes are placed at its `global_off`; gaps are zero-filled.
/// All offset/extent arithmetic is validated with checked adds and against
/// [`MAX_GLOBALS_BYTES`] *before* any allocation, so an untrusted offset can
/// only produce an error, never an oversized zero-fill.
pub(crate) fn derive_globals(data: &[Data]) -> Result<Vec<u8>, String> {
    let mut end = 0_usize;
    for (index, entry) in data.iter().enumerate() {
        let start = entry.global_off as usize;
        let stop = start
            .checked_add(entry.bytes.len())
            .ok_or_else(|| format!("data !c{index} offset overflow"))?;
        end = end.max(stop);
    }
    if end > MAX_GLOBALS_BYTES as usize {
        return Err(format!(
            "globals arena of {end} bytes exceeds the {MAX_GLOBALS_BYTES}-byte limit"
        ));
    }
    let mut globals = vec![0_u8; end];
    for entry in data {
        let start = entry.global_off as usize;
        globals[start..start + entry.bytes.len()].copy_from_slice(&entry.bytes);
    }
    Ok(globals)
}

/// Verifies one function.
fn verify_func(program: &Program, func: &Func) -> Result<(), String> {
    for &ty in func.ins.iter().chain(&func.outs) {
        check_type(program, ty)?;
    }
    for instr in &func.code {
        verify_instr(program, func, instr)?;
    }
    Ok(())
}

/// Validates a type id.
fn check_type(program: &Program, ty: TypeId) -> Result<(), String> {
    if (ty as usize) < program.types.len() {
        Ok(())
    } else {
        Err(format!("type id {ty} out of range"))
    }
}

/// Returns the size of a type, validating its id.
fn type_size(program: &Program, ty: TypeId) -> Result<u32, String> {
    program
        .types
        .get(ty as usize)
        .map(|info| info.size)
        .ok_or_else(|| format!("type id {ty} out of range"))
}

/// Validates a register index.
fn check_reg(reg: u32, n_regs: u32) -> Result<(), String> {
    if reg < n_regs {
        Ok(())
    } else {
        Err(format!("register &{reg} out of range (n_regs {n_regs})"))
    }
}

/// Validates a slot offset and the `size` bytes it covers.
fn check_slot(off: u32, size: u32, slot_bytes: u32) -> Result<(), String> {
    match off.checked_add(size) {
        Some(end) if end <= slot_bytes => Ok(()),
        _ => Err(format!(
            "slot %{off} (+{size}) out of range (slot_bytes {slot_bytes})"
        )),
    }
}

/// Validates an operand, treating a slot as `size` bytes wide.
fn check_operand(operand: Operand, size: u32, n_regs: u32, slot_bytes: u32) -> Result<(), String> {
    match operand {
        Operand::Reg(reg) => check_reg(reg, n_regs),
        Operand::Slot(off) => check_slot(off, size, slot_bytes),
    }
}

/// Validates a call's operand ranges against the callee signature.
fn verify_call(
    program: &Program,
    ins: &[Operand],
    outs: &[u32],
    ins_types: &[TypeId],
    outs_types: &[TypeId],
    n_regs: u32,
    slot_bytes: u32,
) -> Result<(), String> {
    for (operand, ty) in ins.iter().zip(ins_types) {
        check_operand(*operand, type_size(program, *ty)?, n_regs, slot_bytes)?;
    }
    for (slot, ty) in outs.iter().zip(outs_types) {
        check_slot(*slot, type_size(program, *ty)?, slot_bytes)?;
    }
    Ok(())
}

/// Verifies one instruction.
fn verify_instr(program: &Program, func: &Func, instr: &Instr) -> Result<(), String> {
    let n_regs = func.n_regs;
    let slot_bytes = func.slot_bytes;
    let code_len = u32::try_from(func.code.len()).unwrap_or(u32::MAX);
    match instr {
        Instr::Const { dst, ty, .. } => {
            check_type(program, *ty)?;
            check_reg(*dst, n_regs)?;
        }
        Instr::ConstData { dst, data, ty } => {
            check_type(program, *ty)?;
            check_reg(*dst, n_regs)?;
            check_data(program, *data)?;
        }
        Instr::Copy { dst, src, ty } => {
            let size = type_size(program, *ty)?;
            check_operand(*dst, size, n_regs, slot_bytes)?;
            check_operand(*src, size, n_regs, slot_bytes)?;
        }
        Instr::CopyBytes { dst, src, size } => {
            check_slot(*dst, *size, slot_bytes)?;
            check_slot(*src, *size, slot_bytes)?;
        }
        Instr::Load { dst, src, ty } => {
            check_type(program, *ty)?;
            check_reg(*dst, n_regs)?;
            check_slot(*src, VM_POINTER_BYTES, slot_bytes)?;
        }
        Instr::Store { src, dst, ty } => {
            let size = type_size(program, *ty)?;
            check_operand(*src, size, n_regs, slot_bytes)?;
            check_slot(*dst, VM_POINTER_BYTES, slot_bytes)?;
        }
        Instr::Addr { dst, place } => {
            check_reg(*dst, n_regs)?;
            check_slot(*place, 1, slot_bytes)?;
        }
        Instr::AddrField { dst, place, off } => {
            check_reg(*dst, n_regs)?;
            check_slot(*place, *off, slot_bytes)?;
        }
        Instr::AddrFunc { dst, func: target } => {
            check_reg(*dst, n_regs)?;
            check_func(program, *target)?;
        }
        Instr::Field {
            dst,
            place,
            off,
            ty,
        } => verify_field_read(program, *dst, *place, *off, *ty, n_regs, slot_bytes)?,
        Instr::StoreField {
            src,
            place,
            off,
            ty,
        } => verify_field_write(program, *src, *place, *off, *ty, n_regs, slot_bytes)?,
        Instr::Arith {
            dst, lhs, rhs, ty, ..
        } => {
            check_type(program, *ty)?;
            check_reg(*dst, n_regs)?;
            check_reg(*lhs, n_regs)?;
            check_reg(*rhs, n_regs)?;
        }
        Instr::Convert { dst, src, ty, .. } => {
            check_type(program, *ty)?;
            check_reg(*dst, n_regs)?;
            check_reg(*src, n_regs)?;
        }
        Instr::Call {
            func: target,
            ins,
            outs,
        } => {
            verify_direct_call(program, *target, ins, outs, n_regs, slot_bytes)?;
        }
        Instr::CallIndirect { callee, ins, outs } => {
            verify_indirect_call(*callee, ins, outs, n_regs, slot_bytes)?;
        }
        Instr::CallBuiltin { builtin, ins, outs } => {
            verify_builtin_call(*builtin, ins, outs, n_regs, slot_bytes)?;
        }
        Instr::Branch { target } => check_target(*target, code_len)?,
        Instr::BranchIf { cond, then, els } => {
            check_reg(*cond, n_regs)?;
            check_target(*then, code_len)?;
            check_target(*els, code_len)?;
        }
        Instr::Return | Instr::Unreachable => {}
        Instr::Panic { data } => check_data(program, *data)?,
    }
    Ok(())
}

/// Verifies a scalar field read.
fn verify_field_read(
    program: &Program,
    dst: u32,
    place: u32,
    off: u32,
    ty: TypeId,
    n_regs: u32,
    slot_bytes: u32,
) -> Result<(), String> {
    let extent = off
        .checked_add(type_size(program, ty)?)
        .ok_or_else(|| "field extent overflow".to_string())?;
    check_reg(dst, n_regs)?;
    check_slot(place, extent, slot_bytes)
}

/// Verifies a scalar field write.
fn verify_field_write(
    program: &Program,
    src: Operand,
    place: u32,
    off: u32,
    ty: TypeId,
    n_regs: u32,
    slot_bytes: u32,
) -> Result<(), String> {
    let size = type_size(program, ty)?;
    let extent = off
        .checked_add(size)
        .ok_or_else(|| "field extent overflow".to_string())?;
    check_operand(src, size, n_regs, slot_bytes)?;
    check_slot(place, extent, slot_bytes)
}

/// Verifies a direct call.
fn verify_direct_call(
    program: &Program,
    target: u32,
    ins: &[Operand],
    outs: &[u32],
    n_regs: u32,
    slot_bytes: u32,
) -> Result<(), String> {
    let callee = check_func(program, target)?;
    check_arity(ins.len(), callee.ins.len(), "input")?;
    check_arity(outs.len(), callee.outs.len(), "output")?;
    verify_call(
        program,
        ins,
        outs,
        &callee.ins,
        &callee.outs,
        n_regs,
        slot_bytes,
    )
}

/// Verifies an indirect call (operand widths are unknown; check bounds only).
fn verify_indirect_call(
    callee: u32,
    ins: &[Operand],
    outs: &[u32],
    n_regs: u32,
    slot_bytes: u32,
) -> Result<(), String> {
    check_reg(callee, n_regs)?;
    for operand in ins {
        check_operand(*operand, 1, n_regs, slot_bytes)?;
    }
    for slot in outs {
        check_slot(*slot, 1, slot_bytes)?;
    }
    Ok(())
}

/// Verifies a builtin call.
fn verify_builtin_call(
    builtin: Builtin,
    ins: &[Operand],
    outs: &[u32],
    n_regs: u32,
    slot_bytes: u32,
) -> Result<(), String> {
    let (want_in, want_out) = builtin.arity();
    check_arity(ins.len(), want_in, "input")?;
    check_arity(outs.len(), want_out, "output")?;
    for operand in ins {
        check_operand(*operand, VM_POINTER_BYTES, n_regs, slot_bytes)?;
    }
    for slot in outs {
        check_slot(*slot, VM_POINTER_BYTES, slot_bytes)?;
    }
    Ok(())
}

/// Validates a branch target.
fn check_target(target: u32, code_len: u32) -> Result<(), String> {
    if target < code_len {
        Ok(())
    } else {
        Err(format!(
            "branch target {target} out of range (code_len {code_len})"
        ))
    }
}

/// Validates a function index, returning the callee.
fn check_func(program: &Program, func: u32) -> Result<&Func, String> {
    program
        .funcs
        .get(func as usize)
        .ok_or_else(|| format!("function id {func} out of range"))
}

/// Validates a data index.
fn check_data(program: &Program, data: u32) -> Result<(), String> {
    if (data as usize) < program.data.len() {
        Ok(())
    } else {
        Err(format!("data id {data} out of range"))
    }
}

/// Validates a call arity.
fn check_arity(got: usize, want: usize, what: &str) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!(
            "call {what} arity {got} does not match callee's {want}"
        ))
    }
}

#[cfg(test)]
pub(crate) fn sample_program(code: Vec<Instr>) -> Program {
    Program {
        version: LBC_VERSION.to_string(),
        target: VM_TARGET.to_string(),
        types: vec![TypeInfo {
            name: "#bit_native".to_string(),
            size: 8,
            is_ref: false,
        }],
        data: Vec::new(),
        globals: Vec::new(),
        funcs: vec![Func {
            name: "main".to_string(),
            is_extern: false,
            ins: Vec::new(),
            outs: Vec::new(),
            n_regs: 1,
            slot_bytes: 8,
            code,
            lines: Vec::new(),
            place_names: Vec::new(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_rejects_branch_out_of_range() {
        let program = sample_program(vec![Instr::Branch { target: 99 }]);
        assert!(verify(&program).is_err());
    }

    #[test]
    fn verifier_accepts_sample() {
        let program = sample_program(vec![Instr::Return]);
        assert!(verify(&program).is_ok());
    }

    #[test]
    fn verifier_rejects_nonconforming_globals() {
        let mut program = sample_program(vec![Instr::Return]);
        program.data = vec![Data {
            bytes: b"hi".to_vec(),
            global_off: 0,
        }];
        program.globals = b"hi".to_vec();
        assert!(verify(&program).is_ok());

        // A trailing arena byte is not implied by the data.
        program.globals.push(0);
        assert!(verify(&program).is_err());

        // A gap byte holding the wrong value is not implied by the data.
        program.globals = b"xi".to_vec();
        assert!(verify(&program).is_err());

        // A zero gap before a data entry is implied.
        program.globals = vec![0, b'h', b'i'];
        program.data = vec![Data {
            bytes: b"hi".to_vec(),
            global_off: 1,
        }];
        assert!(verify(&program).is_ok());
    }
}
