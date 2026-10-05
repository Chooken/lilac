//! Bytecode ISA, program model, and verifier.
//!
//! The Lilac bytecode is a register machine over a per-frame place region. All
//! operands are resolved to integers at compile time: registers are `u32`
//! indices into the frame's register file, place operands are `u32` byte
//! offsets into the frame's slot region, and branch targets are `u32` program
//! counters. No string is decoded on the dispatch hot path.

use std::collections::{BTreeSet, HashSet};

use crate::decode::{decode, Decoded};

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

/// Maximum number of register-file slots a single function frame may declare.
///
/// The verifier rejects `n_regs` above this bound *before* the interpreter
/// allocates a frame, so an untrusted `.lbc` cannot request an arbitrarily
/// large register file.
pub(crate) const MAX_REGS: u32 = 1 << 20;

/// Maximum per-frame place-region size, in bytes.
///
/// Like [`MAX_GLOBALS_BYTES`], this bounds an allocation driven directly by an
/// untrusted artifact: the verifier rejects `slot_bytes` above it before any
/// frame is entered.
pub(crate) const MAX_FRAME_BYTES: u32 = 64 * 1024 * 1024;

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

/// A resolved call site: callee and operand-pool ranges.
///
/// `ins` and `outs` index a function's shared `operands` pool; entries in
/// `outs` are stored as [`Operand::Slot`] byte offsets. A compact call
/// instruction carries a 24-bit index into `Func.call_sites` rather than
/// inlining the operand lists, so the executed stream holds no `Vec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallSite {
    /// Callee function index (unused, `0`, for indirect and builtin calls).
    pub func: u32,
    /// Range of input operands in the pool.
    pub ins: core::ops::Range<u32>,
    /// Range of output slot operands in the pool.
    pub outs: core::ops::Range<u32>,
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
    ///
    /// `indirect` selects the base: when `true`, the slot holds a ref that is
    /// dereferenced first; when `false`, the slot itself is the aggregate base.
    AddrField {
        /// Destination register.
        dst: u32,
        /// Base slot.
        place: u32,
        /// Field byte offset.
        off: u32,
        /// Whether `place` holds a ref to dereference before adding `off`.
        indirect: bool,
    },
    /// `dst = addr.func func`.
    AddrFunc {
        /// Destination register.
        dst: u32,
        /// Function index.
        func: u32,
    },
    /// Scalar field read.
    ///
    /// `indirect` selects the base: when `true`, the slot holds a ref that is
    /// dereferenced first; when `false`, the slot itself is the aggregate base.
    Field {
        /// Destination register.
        dst: u32,
        /// Object slot.
        place: u32,
        /// Field byte offset.
        off: u32,
        /// Field type.
        ty: TypeId,
        /// Whether `place` holds a ref to dereference before adding `off`.
        indirect: bool,
    },
    /// Scalar field write.
    ///
    /// `indirect` selects the base: when `true`, the slot holds a ref that is
    /// dereferenced first; when `false`, the slot itself is the aggregate base.
    StoreField {
        /// Value operand.
        src: Operand,
        /// Object slot.
        place: u32,
        /// Field byte offset.
        off: u32,
        /// Field type.
        ty: TypeId,
        /// Whether `place` holds a ref to dereference before adding `off`.
        indirect: bool,
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
        /// Operand width type (from the left register's declared type).
        ///
        /// Comparisons have a `#bit8` LIR result regardless of operand width,
        /// so the interpreter uses `operand_ty` for width, signedness, and
        /// float format, exactly as the LLVM backend reads the operand register
        /// types.
        operand_ty: TypeId,
        /// Result type.
        ty: TypeId,
    },
    /// Fused `dst = op lhs, imm` where `lhs` is a register or a frame slot.
    ///
    /// This is the sanctioned superinstruction (spec §12): it lets the
    /// LIR→bytecode compiler fold a redundant `copy %place -> &r` and a
    /// `const` into the arithmetic that consumes both, cutting the instruction
    /// count directly.
    ArithImm {
        /// Operator.
        op: ArithOp,
        /// Destination register.
        dst: u32,
        /// Left operand raw index (register index or slot offset).
        lhs: u32,
        /// Whether `lhs` is a slot offset (else a register index).
        lhs_slot: bool,
        /// Immediate right operand (already masked to the operand width).
        imm: u64,
        /// Operand width type.
        operand_ty: TypeId,
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
    /// Fused `ArithImm` feeding a direct call's single input register.
    ///
    /// Folds `&d = op lhs, imm` immediately followed by `call @f (in &d)`: the
    /// computed value is written to the call input register, then the call runs.
    CallArithImm {
        /// Arithmetic operator.
        op: ArithOp,
        /// Left operand raw index (register index or slot offset).
        lhs: u32,
        /// Whether `lhs` is a slot offset (else a register index).
        lhs_slot: bool,
        /// Immediate right operand.
        imm: u64,
        /// Operand width type.
        operand_ty: TypeId,
        /// Callee function index.
        func: u32,
        /// Input operands (exactly one register, the arithmetic destination).
        ins: Vec<Operand>,
        /// Output slot offsets.
        outs: Vec<u32>,
    },
    /// Fused `ArithImm` + direct call + output copy.
    ///
    /// Folds `&r = op lhs, imm`, `call @f (in &r) (out %s)`, and
    /// `&d = copy %s` into one instruction: the argument is computed, the call
    /// runs, and on return the out place is copied into `&d`.
    CallArithOut {
        /// Arithmetic operator.
        op: ArithOp,
        /// Left operand raw index (register index or slot offset).
        lhs: u32,
        /// Whether `lhs` is a slot offset (else a register index).
        lhs_slot: bool,
        /// Immediate right operand.
        imm: u64,
        /// Operand width type.
        operand_ty: TypeId,
        /// Callee function index.
        func: u32,
        /// Input operands (exactly one register, the arithmetic destination).
        ins: Vec<Operand>,
        /// Output slot offsets (exactly one).
        outs: Vec<u32>,
        /// Destination register for the returned output.
        dst: u32,
        /// Output byte size.
        size: u32,
    },
    /// Direct call whose single output is copied into a register on return.
    ///
    /// Folds `call @f (out %s)` immediately followed by `&d = copy %s`: the
    /// callee still writes the caller's out place by reference, and `return`
    /// copies it into `&d`, eliminating the separate read.
    CallOut {
        /// Callee function index.
        func: u32,
        /// Input operands.
        ins: Vec<Operand>,
        /// Output slot offsets (exactly one).
        outs: Vec<u32>,
        /// Destination register for the returned output.
        dst: u32,
        /// Output byte size (bounds the copied value).
        size: u32,
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
    /// Fused `if (op lhs, imm) { then } else { els }`.
    ///
    /// This is the sanctioned compare+branch superinstruction (spec §12): it
    /// folds an [`Instr::ArithImm`] comparison whose result is read only by the
    /// immediately following [`Instr::BranchIf`].
    BranchIfArithImm {
        /// Comparison operator.
        op: ArithOp,
        /// Left operand raw index (register index or slot offset).
        lhs: u32,
        /// Whether `lhs` is a slot offset (else a register index).
        lhs_slot: bool,
        /// Immediate right operand (already masked to the operand width).
        imm: u64,
        /// Operand width type.
        operand_ty: TypeId,
        /// Taken target for a non-zero comparison result.
        then: u32,
        /// Untaken target for a zero result.
        els: u32,
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
    /// Copy a scalar into the return register and return (return fusion).
    CopyReturn {
        /// Source operand.
        src: Operand,
        /// Value type (byte size).
        ty: TypeId,
    },
    /// Compute arithmetic into the return register and return (return fusion).
    ArithReturn {
        /// Operator.
        op: ArithOp,
        /// Left register.
        lhs: u32,
        /// Right register.
        rhs: u32,
        /// Operand width type.
        operand_ty: TypeId,
    },
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
    /// Byte offset of each input within the callee's input region.
    ///
    /// Precomputed by [`Program::rebuild_layouts`] so `call` binds arguments
    /// without a `type_size` lookup; parallel to `ins`.
    pub in_offsets: Vec<u32>,
    /// Byte size of each input, parallel to [`Func::in_offsets`].
    ///
    /// A `u32` (not `u8`) because runtime values such as `#format_args` are
    /// wider than 255 bytes.
    pub in_sizes: Vec<u32>,
    /// Byte offset just past the input region within the frame's slot region.
    ///
    /// The frame begins with the out-ref region (`outs.len()` pointer-sized
    /// refs), so the inputs follow it and this equals
    /// `outs.len() * VM_POINTER_BYTES + summed input sizes`. Retained as
    /// layout metadata; `return` copies nothing (outputs are by reference).
    pub out_offset: u32,
    /// Byte size of each output, parallel to `outs`.
    pub out_sizes: Vec<u32>,
    /// Register-file slots for a frame of this function.
    pub n_regs: u32,
    /// Per-frame place region size in bytes (zeroed at entry).
    pub slot_bytes: u32,
    /// Reserved register index holding this function's single scalar input when
    /// it uses the register-argument ABI (`None` = inputs live in the slot
    /// region). When `Some`, `call` writes the input here instead of into the
    /// frame's input slots.
    pub arg_reg: Option<u32>,
    /// Reserved register index holding this function's single scalar output when
    /// it uses the register-return ABI (`None` = outputs are by reference).
    /// `return` copies this register into the caller's out place.
    pub ret_reg: Option<u32>,
    /// The executed compact `u32` word stream (M5c.1).
    ///
    /// `pc` is a word index; branch targets are word offsets. The typed
    /// [`Instr`] is compile-time IR only.
    pub code: Vec<u32>,
    /// Resolved call sites referenced by the stream's call instructions.
    pub call_sites: Vec<CallSite>,
    /// Shared operand pool sliced by each [`CallSite`]'s ranges.
    pub operands: Vec<Operand>,
    /// Source-line table, sorted by `pc` (word offsets).
    pub lines: Vec<Line>,
    /// Original `%place` name to slot offset (debugger metadata).
    pub place_names: Vec<(String, u32)>,
}

impl Func {
    /// Returns the set of word program counters that begin a labeled block.
    ///
    /// A pc is a label if it is `0` or the target of any branch. The stream is
    /// decoded linearly; a malformed stream simply stops the scan (the verifier
    /// is the authority on well-formedness).
    #[must_use]
    pub fn labels(&self) -> BTreeSet<u32> {
        let mut set = BTreeSet::new();
        if !self.code.is_empty() {
            set.insert(0);
        }
        let mut pc = 0_usize;
        while pc < self.code.len() {
            let Ok((decoded, next)) = decode(&self.code, pc, &self.call_sites, &self.operands)
            else {
                break;
            };
            match decoded {
                Decoded::Branch { target } => {
                    set.insert(target);
                }
                Decoded::BranchIf { then, els, .. }
                | Decoded::BranchIfArithImm { then, els, .. } => {
                    set.insert(then);
                    set.insert(els);
                }
                _ => {}
            }
            let next = next as usize;
            if next <= pc {
                break;
            }
            pc = next;
        }
        set
    }

    /// Recomputes the precomputed call/return layout fields from the type table.
    ///
    /// The frame begins with the **out-ref region**: one `u64` ref per output
    /// (the pointer ABI, spec §5). Inputs occupy the region immediately after it
    /// in signature order; `out_offset` records where inputs end. The
    /// interpreter's `call` then binds inputs and fills the out-ref region
    /// without any `type_size` lookup, and `return` copies nothing.
    pub(crate) fn rebuild_layout(&mut self, types: &[TypeInfo]) {
        self.in_offsets.clear();
        self.in_sizes.clear();
        // A register-return function has no out-ref cells; a register-argument
        // function has no input cells in the slot region.
        let out_count = if self.ret_reg.is_some() {
            0
        } else {
            u32::try_from(self.outs.len()).unwrap_or(u32::MAX)
        };
        let mut offset = out_count.saturating_mul(VM_POINTER_BYTES);
        if self.arg_reg.is_some() {
            // Record the input's width so `call` reads exactly it, not a full
            // pointer, when the source is a sub-pointer-sized slot.
            if let Some(&ty) = self.ins.first() {
                self.in_sizes.push(type_size_of(types, ty));
            }
            self.out_offset = offset;
            self.out_sizes.clear();
            self.out_sizes
                .extend(self.outs.iter().map(|&ty| type_size_of(types, ty)));
            return;
        }
        for &ty in &self.ins {
            let size = type_size_of(types, ty);
            self.in_offsets.push(offset);
            self.in_sizes.push(size);
            offset = offset.saturating_add(size);
        }
        self.out_offset = offset;
        self.out_sizes.clear();
        self.out_sizes
            .extend(self.outs.iter().map(|&ty| type_size_of(types, ty)));
    }
}

/// The size of a type id in a type table (zero when out of range).
fn type_size_of(types: &[TypeInfo], ty: TypeId) -> u32 {
    types.get(ty as usize).map_or(0, |info| info.size)
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

impl Program {
    /// Recomputes every function's precomputed call/return layout fields.
    ///
    /// Called by the compiler and the `.lbc` parser so that a runnable program
    /// always carries the metadata `call`/`return` need. See
    /// [`Func::rebuild_layout`].
    pub(crate) fn rebuild_layouts(&mut self) {
        let types = &self.types;
        for func in &mut self.funcs {
            func.rebuild_layout(types);
        }
    }
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

/// Whether an instruction ends a function's control flow.
///
/// A verified non-extern body must end in one of these so `pc` can never run
/// off the end of `code`.
fn is_terminator(instr: &Decoded) -> bool {
    matches!(
        instr,
        Decoded::Return
            | Decoded::CopyReturn { .. }
            | Decoded::ArithReturn { .. }
            | Decoded::Panic { .. }
            | Decoded::Unreachable
            | Decoded::Branch { .. }
            | Decoded::BranchIf { .. }
            | Decoded::BranchIfArithImm { .. }
    )
}

/// Verifies one function by decoding its word stream.
///
/// The stream is decoded linearly to establish the set of valid instruction
/// boundaries; every instruction is checked, then each branch target must land
/// on one of those boundaries. A malformed stream is reported as a clean error,
/// never a panic.
fn verify_func(program: &Program, func: &Func) -> Result<(), String> {
    if func.n_regs > MAX_REGS {
        return Err(format!(
            "function `{}` declares {} registers (limit {MAX_REGS})",
            func.name, func.n_regs
        ));
    }
    if func.slot_bytes > MAX_FRAME_BYTES {
        return Err(format!(
            "function `{}` declares {} slot bytes (limit {MAX_FRAME_BYTES})",
            func.name, func.slot_bytes
        ));
    }
    for &ty in func.ins.iter().chain(&func.outs) {
        check_type(program, ty)?;
    }
    if func.is_extern {
        return Ok(());
    }
    let code_len = func.code.len();
    if code_len == 0 {
        return Err(format!("function `{}` has an empty body", func.name));
    }
    let mut boundaries = vec![false; code_len];
    let mut targets: Vec<u32> = Vec::new();
    let mut pc = 0_usize;
    let mut last_terminator = false;
    while pc < code_len {
        boundaries[pc] = true;
        let (decoded, next) = decode(&func.code, pc, &func.call_sites, &func.operands)
            .map_err(|err| format!("function `{}`: {err}", func.name))?;
        if next as usize <= pc {
            return Err(format!(
                "function `{}`: instruction at word {pc} does not advance",
                func.name
            ));
        }
        last_terminator = is_terminator(&decoded);
        verify_decoded(program, func, &decoded, &mut targets)?;
        pc = next as usize;
    }
    if !last_terminator {
        return Err(format!(
            "function `{}` does not end in a control-flow terminator",
            func.name
        ));
    }
    let code_len = u32::try_from(code_len).unwrap_or(u32::MAX);
    for target in targets {
        check_target(target, code_len, &boundaries)?;
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

/// Validates a call's input operands and output slots against the callee
/// signature. Decoded outputs are always [`Operand::Slot`]s.
fn verify_call(
    program: &Program,
    ins: &[Operand],
    outs: &[Operand],
    ins_types: &[TypeId],
    outs_types: &[TypeId],
    n_regs: u32,
    slot_bytes: u32,
) -> Result<(), String> {
    for (operand, ty) in ins.iter().zip(ins_types) {
        check_operand(*operand, type_size(program, *ty)?, n_regs, slot_bytes)?;
    }
    for (operand, ty) in outs.iter().zip(outs_types) {
        match operand {
            Operand::Slot(slot) => check_slot(*slot, type_size(program, *ty)?, slot_bytes)?,
            Operand::Reg(_) => return Err("call output operand must be a slot".to_string()),
        }
    }
    Ok(())
}

/// Verifies one decoded instruction.
///
/// Branch targets are collected into `targets` and validated against the
/// instruction-boundary set after the whole stream has been decoded.
#[allow(clippy::too_many_lines)]
fn verify_decoded(
    program: &Program,
    func: &Func,
    instr: &Decoded,
    targets: &mut Vec<u32>,
) -> Result<(), String> {
    let n_regs = func.n_regs;
    let slot_bytes = func.slot_bytes;
    match instr {
        Decoded::Const { dst, .. } => check_reg(*dst, n_regs)?,
        Decoded::ConstData { dst, data } => {
            check_reg(*dst, n_regs)?;
            check_data(program, *data)?;
        }
        Decoded::Copy { dst, src, size } => {
            check_operand(*dst, *size, n_regs, slot_bytes)?;
            check_operand(*src, *size, n_regs, slot_bytes)?;
        }
        Decoded::CopyBytes { dst, src, size } => {
            check_slot(*dst, *size, slot_bytes)?;
            check_slot(*src, *size, slot_bytes)?;
        }
        Decoded::Load { dst, src, .. } => {
            check_reg(*dst, n_regs)?;
            check_slot(*src, VM_POINTER_BYTES, slot_bytes)?;
        }
        Decoded::Store { src, dst, size } => {
            check_operand(*src, *size, n_regs, slot_bytes)?;
            check_slot(*dst, VM_POINTER_BYTES, slot_bytes)?;
        }
        Decoded::Addr { dst, place } => {
            check_reg(*dst, n_regs)?;
            check_slot(*place, 1, slot_bytes)?;
        }
        Decoded::AddrField {
            dst,
            place,
            off,
            indirect,
        } => {
            check_reg(*dst, n_regs)?;
            check_field_base(*place, *off, *indirect, slot_bytes)?;
        }
        Decoded::AddrFunc { dst, func: target } => {
            check_reg(*dst, n_regs)?;
            check_func(program, *target)?;
        }
        Decoded::Field {
            dst,
            place,
            off,
            size,
            indirect,
        } => {
            let extent = off
                .checked_add(*size)
                .ok_or_else(|| "field extent overflow".to_string())?;
            check_reg(*dst, n_regs)?;
            check_field_base(*place, extent, *indirect, slot_bytes)?;
        }
        Decoded::StoreField {
            src,
            place,
            off,
            size,
            indirect,
        } => {
            let extent = off
                .checked_add(*size)
                .ok_or_else(|| "field extent overflow".to_string())?;
            check_operand(*src, *size, n_regs, slot_bytes)?;
            check_field_base(*place, extent, *indirect, slot_bytes)?;
        }
        Decoded::IAdd { dst, lhs, rhs, .. }
        | Decoded::ISub { dst, lhs, rhs, .. }
        | Decoded::IMul { dst, lhs, rhs, .. }
        | Decoded::IDiv { dst, lhs, rhs, .. }
        | Decoded::IMod { dst, lhs, rhs, .. }
        | Decoded::ILt { dst, lhs, rhs, .. }
        | Decoded::IGt { dst, lhs, rhs, .. }
        | Decoded::FAdd { dst, lhs, rhs, .. }
        | Decoded::FSub { dst, lhs, rhs, .. }
        | Decoded::FMul { dst, lhs, rhs, .. }
        | Decoded::FDiv { dst, lhs, rhs, .. }
        | Decoded::FMod { dst, lhs, rhs, .. }
        | Decoded::FLt { dst, lhs, rhs, .. }
        | Decoded::FGt { dst, lhs, rhs, .. }
        | Decoded::IDivU { dst, lhs, rhs, .. }
        | Decoded::IModU { dst, lhs, rhs, .. }
        | Decoded::Eq { dst, lhs, rhs, .. }
        | Decoded::ILtU { dst, lhs, rhs, .. }
        | Decoded::IGtU { dst, lhs, rhs, .. } => {
            check_reg(*dst, n_regs)?;
            check_reg(*lhs, n_regs)?;
            check_reg(*rhs, n_regs)?;
        }
        Decoded::Trunc { dst, src, .. } | Decoded::ExtendZero { dst, src } => {
            check_reg(*dst, n_regs)?;
            check_reg(*src, n_regs)?;
        }
        Decoded::ArithImm {
            dst,
            lhs,
            lhs_slot,
            width,
            ..
        } => {
            check_reg(*dst, n_regs)?;
            let size = u32::from(*width) / 8;
            if *lhs_slot {
                check_slot(*lhs, size, slot_bytes)?;
            } else {
                check_reg(*lhs, n_regs)?;
            }
        }
        Decoded::Call { site, ins, outs } => {
            verify_direct_call(program, site.func, ins, outs, n_regs, slot_bytes)?;
        }
        Decoded::CallOut {
            dst,
            site,
            ins,
            outs,
            ..
        } => {
            check_reg(*dst, n_regs)?;
            verify_direct_call(program, site.func, ins, outs, n_regs, slot_bytes)?;
        }
        Decoded::CallArithImm {
            lhs,
            lhs_slot,
            width,
            site,
            ins,
            outs,
            ..
        } => {
            let size = u32::from(*width) / 8;
            if *lhs_slot {
                check_slot(*lhs, size, slot_bytes)?;
            } else {
                check_reg(*lhs, n_regs)?;
            }
            verify_direct_call(program, site.func, ins, outs, n_regs, slot_bytes)?;
        }
        Decoded::CallArithOut {
            lhs,
            lhs_slot,
            width,
            site,
            ins,
            outs,
            dst,
            ..
        } => {
            let size = u32::from(*width) / 8;
            if *lhs_slot {
                check_slot(*lhs, size, slot_bytes)?;
            } else {
                check_reg(*lhs, n_regs)?;
            }
            check_reg(*dst, n_regs)?;
            verify_direct_call(program, site.func, ins, outs, n_regs, slot_bytes)?;
        }
        Decoded::CallIndirect {
            callee, ins, outs, ..
        } => {
            check_reg(*callee, n_regs)?;
            verify_indirect_call(ins, outs, n_regs, slot_bytes)?;
        }
        Decoded::CallBuiltin {
            builtin, ins, outs, ..
        } => {
            verify_builtin_call(*builtin, ins, outs, n_regs, slot_bytes)?;
        }
        Decoded::Branch { target } => targets.push(*target),
        Decoded::BranchIf { cond, then, els } => {
            check_reg(*cond, n_regs)?;
            targets.push(*then);
            targets.push(*els);
        }
        Decoded::BranchIfArithImm {
            lhs,
            lhs_slot,
            width,
            then,
            els,
            ..
        } => {
            let size = u32::from(*width) / 8;
            if *lhs_slot {
                check_slot(*lhs, size, slot_bytes)?;
            } else {
                check_reg(*lhs, n_regs)?;
            }
            targets.push(*then);
            targets.push(*els);
        }
        Decoded::Return | Decoded::Unreachable => {}
        Decoded::CopyReturn { src, size } => {
            check_operand(*src, *size, n_regs, slot_bytes)?;
        }
        Decoded::ArithReturn { lhs, rhs, .. } => {
            check_reg(*lhs, n_regs)?;
            check_reg(*rhs, n_regs)?;
        }
        Decoded::Panic { data } => check_data(program, *data)?,
    }
    Ok(())
}

/// Validates a field instruction's object slot.
///
/// When `indirect`, the slot must hold a pointer-sized ref: `off` is relative
/// to the dereferenced pointee, not the frame, so it is not added to the
/// frame-slot bound. Otherwise the slot itself is the aggregate and must cover
/// `direct_extent` bytes (`off`, or `off + size` for scalar reads/writes).
fn check_field_base(
    place: u32,
    direct_extent: u32,
    indirect: bool,
    slot_bytes: u32,
) -> Result<(), String> {
    if indirect {
        check_slot(place, VM_POINTER_BYTES, slot_bytes)
    } else {
        check_slot(place, direct_extent, slot_bytes)
    }
}

/// Verifies a direct call.
fn verify_direct_call(
    program: &Program,
    target: u32,
    ins: &[Operand],
    outs: &[Operand],
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
    ins: &[Operand],
    outs: &[Operand],
    n_regs: u32,
    slot_bytes: u32,
) -> Result<(), String> {
    for operand in ins {
        check_operand(*operand, 1, n_regs, slot_bytes)?;
    }
    for operand in outs {
        match operand {
            Operand::Slot(slot) => check_slot(*slot, 1, slot_bytes)?,
            Operand::Reg(_) => return Err("call output operand must be a slot".to_string()),
        }
    }
    Ok(())
}

/// Verifies a builtin call.
fn verify_builtin_call(
    builtin: Builtin,
    ins: &[Operand],
    outs: &[Operand],
    n_regs: u32,
    slot_bytes: u32,
) -> Result<(), String> {
    let (want_in, want_out) = builtin.arity();
    check_arity(ins.len(), want_in, "input")?;
    check_arity(outs.len(), want_out, "output")?;
    for operand in ins {
        check_operand(*operand, VM_POINTER_BYTES, n_regs, slot_bytes)?;
    }
    for operand in outs {
        match operand {
            Operand::Slot(slot) => check_slot(*slot, VM_POINTER_BYTES, slot_bytes)?,
            Operand::Reg(_) => return Err("call output operand must be a slot".to_string()),
        }
    }
    Ok(())
}

/// Validates a branch target against the code extent and instruction
/// boundaries, so a branch cannot land inside a multi-word instruction.
fn check_target(target: u32, code_len: u32, boundaries: &[bool]) -> Result<(), String> {
    if target >= code_len {
        return Err(format!(
            "branch target {target} out of range (code_len {code_len})"
        ));
    }
    if boundaries.get(target as usize).copied().unwrap_or(false) {
        Ok(())
    } else {
        Err(format!(
            "branch target {target} is not an instruction boundary"
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

/// The single-type table used by the crate's test programs.
#[cfg(test)]
pub(crate) fn sample_types() -> Vec<TypeInfo> {
    vec![TypeInfo {
        name: "#bit_native".to_string(),
        size: 8,
        is_ref: false,
    }]
}

/// Encodes a typed instruction list (instruction-index branch targets) into the
/// `(words, call_sites, operands)` triple, panicking only on a test bug.
#[cfg(test)]
pub(crate) fn words_of(
    types: &[TypeInfo],
    instrs: &[Instr],
) -> (Vec<u32>, Vec<CallSite>, Vec<Operand>) {
    let (builder, _) = crate::encode::encode_instrs(types, instrs).expect("encode test program");
    (builder.words, builder.call_sites, builder.operands)
}

/// Builds a one-function `main` program from a raw word stream.
#[cfg(test)]
pub(crate) fn sample_program_words(code: Vec<u32>) -> Program {
    Program {
        version: LBC_VERSION.to_string(),
        target: VM_TARGET.to_string(),
        types: sample_types(),
        data: Vec::new(),
        globals: Vec::new(),
        funcs: vec![Func {
            name: "main".to_string(),
            is_extern: false,
            ins: Vec::new(),
            outs: Vec::new(),
            in_offsets: Vec::new(),
            in_sizes: Vec::new(),
            out_offset: 0,
            out_sizes: Vec::new(),
            n_regs: 1,
            slot_bytes: 8,
            arg_reg: None,
            ret_reg: None,
            code,
            call_sites: Vec::new(),
            operands: Vec::new(),
            lines: Vec::new(),
            place_names: Vec::new(),
        }],
    }
}

/// Builds a one-function `main` program from typed instructions, encoding them.
#[cfg(test)]
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sample_program(code: Vec<Instr>) -> Program {
    let types = sample_types();
    let (words, call_sites, operands) = words_of(&types, &code);
    Program {
        version: LBC_VERSION.to_string(),
        target: VM_TARGET.to_string(),
        types,
        data: Vec::new(),
        globals: Vec::new(),
        funcs: vec![Func {
            name: "main".to_string(),
            is_extern: false,
            ins: Vec::new(),
            outs: Vec::new(),
            in_offsets: Vec::new(),
            in_sizes: Vec::new(),
            out_offset: 0,
            out_sizes: Vec::new(),
            n_regs: 1,
            slot_bytes: 8,
            arg_reg: None,
            ret_reg: None,
            code: words,
            call_sites,
            operands,
            lines: Vec::new(),
            place_names: Vec::new(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{OP_BRANCH, OP_CONST};

    /// A `Const` word 0 for `dst` (followed by two immediate words).
    fn const_word(dst: u32) -> u32 {
        u32::from(OP_CONST) | (dst << 8)
    }

    /// A `Branch` word targeting word `target`.
    fn branch_word(target: u32) -> u32 {
        u32::from(OP_BRANCH) | (target << 8)
    }

    #[test]
    fn verifier_rejects_branch_out_of_range() {
        let program = sample_program_words(vec![branch_word(99)]);
        let err = verify(&program).expect_err("branch out of range");
        assert!(err.contains("out of range"), "{err}");
    }

    /// An unknown opcode in the stream is a clean verifier error.
    #[test]
    fn verifier_rejects_unknown_opcode() {
        let program = sample_program_words(vec![0x00FF_FFFF]);
        let err = verify(&program).expect_err("unknown opcode");
        assert!(err.contains("unknown opcode"), "{err}");
    }

    /// A branch that lands inside a multi-word instruction (the `Const`
    /// immediate) is rejected even though decoding there might succeed.
    #[test]
    fn verifier_rejects_misaligned_branch_target() {
        // `Const &0 = 0` is three words; the branch's target (word 1) lands
        // inside its immediate words.
        let program = sample_program_words(vec![const_word(0), 0, 0, branch_word(1)]);
        let err = verify(&program).expect_err("misaligned branch");
        assert!(err.contains("boundary"), "{err}");
    }

    #[test]
    fn verifier_accepts_sample() {
        let program = sample_program(vec![Instr::Return]);
        assert!(verify(&program).is_ok());
    }

    #[test]
    fn verifier_rejects_missing_terminator() {
        let program = sample_program(vec![Instr::Const {
            dst: 0,
            imm: 1,
            ty: 0,
        }]);
        let err = verify(&program).expect_err("missing terminator");
        assert!(err.contains("does not end"), "{err}");
    }

    #[test]
    fn verifier_rejects_empty_body() {
        let program = sample_program(Vec::new());
        let err = verify(&program).expect_err("empty body");
        assert!(err.contains("empty body"), "{err}");
    }

    #[test]
    fn verifier_allows_intermediate_fallthrough() {
        let program = sample_program(vec![
            Instr::Const {
                dst: 0,
                imm: 1,
                ty: 0,
            },
            Instr::Unreachable,
        ]);
        assert!(verify(&program).is_ok());
    }

    #[test]
    fn verifier_rejects_oversized_regs() {
        let mut program = sample_program(vec![Instr::Return]);
        program.funcs[0].n_regs = MAX_REGS + 1;
        let err = verify(&program).expect_err("oversized regs");
        assert!(err.contains("registers"), "{err}");
    }

    #[test]
    fn verifier_rejects_oversized_frame() {
        let mut program = sample_program(vec![Instr::Return]);
        program.funcs[0].slot_bytes = MAX_FRAME_BYTES + 1;
        let err = verify(&program).expect_err("oversized frame");
        assert!(err.contains("slot bytes"), "{err}");
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

    /// An `indirect` field access treats the object slot as a pointer: `off`
    /// is relative to the pointee and must not be added to the frame-slot
    /// bound, so an offset past the frame extent still verifies.
    #[test]
    fn verifier_accepts_indirect_field_beyond_frame_extent() {
        let program = sample_program(vec![
            Instr::Field {
                dst: 0,
                place: 0,
                off: 1024,
                ty: 0,
                indirect: true,
            },
            Instr::AddrField {
                dst: 0,
                place: 0,
                off: 1024,
                indirect: true,
            },
            Instr::StoreField {
                src: Operand::Reg(0),
                place: 0,
                off: 1024,
                ty: 0,
                indirect: true,
            },
            Instr::Return,
        ]);
        assert!(verify(&program).is_ok(), "{:?}", verify(&program));
    }

    /// An `indirect` field access still requires the object slot to hold a
    /// full pointer.
    #[test]
    fn verifier_rejects_indirect_field_pointer_out_of_range() {
        let program = sample_program(vec![
            Instr::Field {
                dst: 0,
                place: 4,
                off: 0,
                ty: 0,
                indirect: true,
            },
            Instr::Return,
        ]);
        assert!(verify(&program).is_err());
    }

    /// A direct field access stays bounded by `place + off + size`.
    #[test]
    fn verifier_rejects_direct_field_beyond_frame_extent() {
        let program = sample_program(vec![
            Instr::Field {
                dst: 0,
                place: 0,
                off: 4,
                ty: 0,
                indirect: false,
            },
            Instr::Return,
        ]);
        assert!(verify(&program).is_err());
    }
}
