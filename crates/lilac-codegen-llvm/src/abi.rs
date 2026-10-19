//! How C passes and returns aggregates by value on each target, after
//! clang's `TargetInfo`s (`clang/lib/CodeGen/Targets`), so a Lilac
//! `extern`, `export` or C function value agrees with the C compiler on
//! the other side. The input is a [`Shape`], the C type of a slot (a
//! scalar, or a struct, union or array tree with C offsets); the output
//! says how each slot travels: as the scalar itself ([`Pass::Scalar`]), as
//! LLVM values loaded from the object's bytes ([`Pass::Direct`]), or as the
//! address of a copy ([`Pass::Indirect`]). Nothing here touches LLVM: the
//! backend turns a [`Piece`] into its type.
//!
//! An empty aggregate never gets here (the frontend's `E028`), nor does a
//! target that passes no aggregates ([`Target::passes_c_aggregates`]).

use lilac_target::{Arch, Os, Scalar, Target};

/// The C type of one slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    /// An integer of this many bits.
    Int(u32),
    /// A binary float of this many bits.
    Float(u32),
    /// A data or function pointer.
    Ptr,
    /// A struct: each field at its offset, in declaration order.
    Struct {
        size: u64,
        align: u64,
        fields: Vec<(u64, Shape)>,
    },
    /// A union: every member at offset 0.
    Union {
        size: u64,
        align: u64,
        members: Vec<Shape>,
    },
    /// An array of `len` elements.
    Array { elem: Box<Shape>, len: u64 },
}

/// One LLVM value a slot travels as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    /// `iN`.
    Int(u32),
    /// `half`, `float`, `double` or `fp128`.
    Float(u32),
    /// `ptr`.
    Ptr,
    /// `<count x float>` of floats of `bits`.
    Vector { bits: u32, count: u32 },
    /// `[count x piece]`.
    Array(Box<Piece>, u32),
}

/// A [`Piece`] read from (or written to) an object's bytes at `offset`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Part {
    pub(crate) offset: u64,
    pub(crate) piece: Piece,
}

/// How one parameter travels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    /// A scalar slot, as its own type.
    Scalar,
    /// One LLVM parameter per part, each loaded from the object; an HFA
    /// on AAPCS64 also aligns its stack slot (`alignstack`).
    Direct {
        parts: Vec<Part>,
        align_stack: Option<u64>,
    },
    /// The address of a copy: `byval` (the callee's own copy, made by the
    /// call) aligned to `align`, or a plain pointer to a copy the caller
    /// made and the callee may change.
    Indirect { byval: bool, align: u64 },
}

/// How a function's result travels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Return {
    /// No result.
    Void,
    /// A scalar result, as its own type.
    Scalar,
    /// One part returned as itself, or two (an x86-64 pair) as the literal
    /// struct `{ lo, hi }`.
    Direct(Vec<Part>),
    /// Through a hidden first parameter (`sret`) pointing at memory of
    /// this alignment.
    Indirect { align: u64 },
}

/// A whole C signature's slots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Signature {
    pub(crate) params: Vec<Pass>,
    pub(crate) ret: Return,
}

impl Shape {
    fn is_scalar(&self) -> bool {
        matches!(self, Self::Int(_) | Self::Float(_) | Self::Ptr)
    }

    /// `sizeof`, in bytes.
    pub(crate) fn size(&self, target: Target) -> u64 {
        match self {
            Self::Int(bits) | Self::Float(bits) => u64::from(*bits) / 8,
            Self::Ptr => target.scalar_layout(Scalar::Pointer).0,
            Self::Struct { size, .. } | Self::Union { size, .. } => *size,
            Self::Array { elem, len } => elem.size(target) * len,
        }
    }

    /// `_Alignof`, in bytes.
    pub(crate) fn align(&self, target: Target) -> u64 {
        let scalar = match self {
            Self::Int(bits) => Scalar::bit(*bits),
            Self::Float(bits) => Scalar::float(*bits),
            Self::Ptr => Some(Scalar::Pointer),
            Self::Struct { align, .. } | Self::Union { align, .. } => return *align,
            Self::Array { elem, .. } => return elem.align(target),
        };
        scalar.map_or(1, |scalar| target.scalar_layout(scalar).1)
    }

    /// The fields of a struct or union with their offsets.
    fn fields(&self) -> Vec<(u64, &Shape)> {
        match self {
            Self::Struct { fields, .. } => fields.iter().map(|(at, f)| (*at, f)).collect(),
            Self::Union { members, .. } => members.iter().map(|m| (0, m)).collect(),
            _ => Vec::new(),
        }
    }

    fn is_record(&self) -> bool {
        matches!(self, Self::Struct { .. } | Self::Union { .. })
    }
}

impl Piece {
    /// The bytes a load or store of it touches (LLVM's store size).
    pub(crate) fn store_size(&self, target: Target) -> u64 {
        match self {
            Self::Int(bits) => u64::from(bits.div_ceil(8)),
            Self::Float(bits) => u64::from(*bits) / 8,
            Self::Ptr => target.scalar_layout(Scalar::Pointer).0,
            Self::Vector { bits, count } => u64::from(bits * count) / 8,
            Self::Array(elem, count) => elem.alloc_size(target) * u64::from(*count),
        }
    }

    /// LLVM's ABI alignment of it on x86-64, the one target that places
    /// a part by it (the `{ lo, hi }` pair).
    fn x86_64_align(&self) -> u64 {
        match self {
            Self::Int(bits) => u64::from(bits.div_ceil(8).next_power_of_two().min(8)),
            Self::Float(bits) => u64::from(*bits / 8),
            Self::Ptr => 8,
            Self::Vector { bits, count } => u64::from(bits * count / 8).next_power_of_two(),
            Self::Array(elem, _) => elem.x86_64_align(),
        }
    }

    /// LLVM's allocation size: the store size rounded to the alignment.
    fn alloc_size(&self, target: Target) -> u64 {
        match self {
            Self::Int(bits) => u64::from(bits.div_ceil(8).next_power_of_two()),
            _ => self.store_size(target),
        }
    }
}

/// The furthest byte a list of parts reaches: past the object's size,
/// the backend moves the object through a scratch slot this large.
pub(crate) fn extent(parts: &[Part], target: Target) -> u64 {
    parts
        .iter()
        .map(|part| part.offset + part.piece.store_size(target))
        .max()
        .unwrap_or(0)
}

/// How a C function with these parameters and result passes each slot on
/// `target`.
pub(crate) fn classify(target: Target, params: &[Shape], ret: Option<&Shape>) -> Signature {
    match target.arch {
        Arch::X86_64 if target.os == Os::Windows => Signature {
            params: params.iter().map(|p| win64_param(target, p)).collect(),
            ret: ret.map_or(Return::Void, |r| win64_return(target, r)),
        },
        Arch::X86_64 => sysv_signature(target, params, ret),
        Arch::X86 => Signature {
            params: params.iter().map(|p| i386_param(target, p)).collect(),
            ret: ret.map_or(Return::Void, |r| i386_return(target, r)),
        },
        Arch::Aarch64 => Signature {
            params: params.iter().map(|p| aarch64_param(target, p)).collect(),
            ret: ret.map_or(Return::Void, |r| aarch64_return(target, r)),
        },
        Arch::Arm => Signature {
            params: params.iter().map(|p| arm_param(target, p)).collect(),
            ret: ret.map_or(Return::Void, |r| arm_return(target, r)),
        },
        Arch::Riscv32 | Arch::Riscv64 => Signature {
            params: params.iter().map(|p| riscv_param(target, p)).collect(),
            ret: ret.map_or(Return::Void, |r| riscv_return(target, r)),
        },
        Arch::Wasm32 | Arch::Wasm64 => Signature {
            params: params.iter().map(|p| wasm_param(target, p)).collect(),
            ret: ret.map_or(Return::Void, |r| wasm_return(target, r)),
        },
    }
}

fn direct(piece: Piece) -> Pass {
    Pass::Direct {
        parts: vec![Part { offset: 0, piece }],
        align_stack: None,
    }
}

fn direct_return(piece: Piece) -> Return {
    Return::Direct(vec![Part { offset: 0, piece }])
}

fn scalar_piece(shape: &Shape) -> Piece {
    match shape {
        Shape::Int(bits) => Piece::Int(*bits),
        Shape::Float(bits) => Piece::Float(*bits),
        _ => Piece::Ptr,
    }
}

/// A struct's one non-empty element, through single-element arrays and
/// nested single-element structs, when it fills the whole struct
/// (clang's `isSingleElementStruct`).
fn single_element(shape: &Shape, target: Target) -> Option<&Shape> {
    if !shape.is_record() {
        return None;
    }
    let mut found = None;
    for (_, field) in shape.fields() {
        if field.size(target) == 0 {
            continue;
        }
        if found.is_some() {
            return None;
        }
        let mut field = field;
        while let Shape::Array { elem, len: 1 } = field {
            field = elem;
        }
        found = Some(if field.is_scalar() {
            field
        } else {
            single_element(field, target)?
        });
    }
    let found = found?;
    (found.size(target) == shape.size(target)).then_some(found)
}

/// The base type and member count of a homogeneous floating-point
/// aggregate: one to four floats of one size and no padding (clang's
/// `isHomogeneousAggregate`). `base_ok` says which floats count.
fn homogeneous(shape: &Shape, target: Target, base_ok: fn(u32) -> bool) -> Option<(u32, u64)> {
    let mut base = None;
    let members = hfa_members(shape, target, base_ok, &mut base)?;
    let base = base?;
    (members > 0 && members <= 4).then_some((base, members))
}

fn hfa_members(
    shape: &Shape,
    target: Target,
    base_ok: fn(u32) -> bool,
    base: &mut Option<u32>,
) -> Option<u64> {
    match shape {
        Shape::Array { elem, len } => {
            if *len == 0 {
                return None;
            }
            Some(hfa_members(elem, target, base_ok, base)? * len)
        }
        Shape::Struct { .. } | Shape::Union { .. } => {
            let union = matches!(shape, Shape::Union { .. });
            let mut members = 0;
            for (_, field) in shape.fields() {
                let mut inner = field;
                while let Shape::Array { elem, len } = inner {
                    if *len == 0 {
                        return None;
                    }
                    inner = elem;
                }
                if inner.is_record() && inner.size(target) == 0 {
                    continue;
                }
                let count = hfa_members(field, target, base_ok, base)?;
                members = if union {
                    members.max(count)
                } else {
                    members + count
                };
            }
            let bits = (*base)?;
            (u64::from(bits) / 8 * members == shape.size(target)).then_some(members)
        }
        Shape::Float(bits) if base_ok(*bits) => match base {
            Some(known) if *known != *bits => None,
            _ => {
                *base = Some(*bits);
                Some(1)
            }
        },
        _ => None,
    }
}

// x86-64 System V (clang's `X86_64ABIInfo`).

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Class {
    None,
    Integer,
    Sse,
    SseUp,
    Memory,
}

fn merge(accum: Class, field: Class) -> Class {
    match (accum, field) {
        _ if accum == field || field == Class::None => accum,
        (_, Class::Memory) => Class::Memory,
        (Class::None, _) => field,
        (Class::Integer, _) | (_, Class::Integer) => Class::Integer,
        _ => Class::Sse,
    }
}

fn post_merge(size_bits: u64, lo: &mut Class, hi: &mut Class) {
    if *hi == Class::Memory {
        *lo = Class::Memory;
    }
    if size_bits > 128 && (*lo != Class::Sse || *hi != Class::SseUp) {
        *lo = Class::Memory;
    }
    if *hi == Class::SseUp && *lo != Class::Sse {
        *hi = Class::Sse;
    }
}

/// The classes of the two eightbytes of `shape` placed `base` bits into
/// the slot.
fn sysv_classify(shape: &Shape, base: u64, target: Target) -> (Class, Class) {
    let mut lo = Class::None;
    let mut hi = Class::None;
    let current = |lo: &mut Class, hi: &mut Class, class: Class| {
        if base < 64 {
            *lo = class;
        } else {
            *hi = class;
        }
    };
    let size = shape.size(target) * 8;
    match shape {
        Shape::Int(_) | Shape::Ptr => current(&mut lo, &mut hi, Class::Integer),
        Shape::Float(128) => {
            lo = Class::Sse;
            hi = Class::SseUp;
        }
        Shape::Float(_) => current(&mut lo, &mut hi, Class::Sse),
        Shape::Array { elem, len } => {
            current(&mut lo, &mut hi, Class::Memory);
            let elem_size = elem.size(target) * 8;
            if size > 512 || !base.is_multiple_of(elem.align(target) * 8) {
                return (lo, hi);
            }
            current(&mut lo, &mut hi, Class::None);
            // Clang keeps a lone 256- or 512-bit vector in registers under
            // AVX; Lilac has no vectors and assumes no AVX, so past 128 bits
            // is memory.
            if size > 128 {
                return (lo, hi);
            }
            for index in 0..*len {
                let (field_lo, field_hi) = sysv_classify(elem, base + index * elem_size, target);
                lo = merge(lo, field_lo);
                hi = merge(hi, field_hi);
                if lo == Class::Memory || hi == Class::Memory {
                    break;
                }
            }
            post_merge(size, &mut lo, &mut hi);
        }
        Shape::Struct { .. } | Shape::Union { .. } => {
            if size > 512 {
                current(&mut lo, &mut hi, Class::Memory);
                return (lo, hi);
            }
            for (offset, field) in shape.fields() {
                // As for arrays: no vector, so past 128 bits is memory.
                if size > 128 || !(offset * 8).is_multiple_of(field.align(target) * 8) {
                    lo = Class::Memory;
                    post_merge(size, &mut lo, &mut hi);
                    return (lo, hi);
                }
                let (field_lo, field_hi) = sysv_classify(field, base + offset * 8, target);
                lo = merge(lo, field_lo);
                hi = merge(hi, field_hi);
                if lo == Class::Memory || hi == Class::Memory {
                    break;
                }
            }
            post_merge(size, &mut lo, &mut hi);
        }
    }
    (lo, hi)
}

/// Clang's LLVM type for a C type (`CodeGenTypes::ConvertType`), as far as
/// the x86-64 eightbyte types look into it: a union is its storage member
/// (the most aligned, then the largest, first such) and byte padding.
enum Ir {
    Int(u32),
    Float(u32),
    Ptr,
    Struct { size: u64, fields: Vec<(u64, Ir)> },
    Array { elem: Box<Ir>, elem_size: u64 },
}

fn ir_of(shape: &Shape, target: Target) -> Ir {
    match shape {
        Shape::Int(bits) => Ir::Int(*bits),
        Shape::Float(bits) => Ir::Float(*bits),
        Shape::Ptr => Ir::Ptr,
        Shape::Struct { size, fields, .. } => Ir::Struct {
            size: *size,
            fields: fields
                .iter()
                .map(|(at, field)| (*at, ir_of(field, target)))
                .collect(),
        },
        Shape::Union { size, members, .. } => {
            let mut storage: Option<&Shape> = None;
            for member in members {
                let better = storage.is_none_or(|known| {
                    let (a, b) = (member.align(target), known.align(target));
                    a > b || (a == b && member.size(target) > known.size(target))
                });
                if better {
                    storage = Some(member);
                }
            }
            let used = storage.map_or(0, |s| s.size(target));
            let mut fields = Vec::new();
            if let Some(storage) = storage {
                fields.push((0, ir_of(storage, target)));
            }
            if *size > used {
                fields.push((
                    used,
                    Ir::Array {
                        elem: Box::new(Ir::Int(8)),
                        elem_size: 1,
                    },
                ));
            }
            Ir::Struct {
                size: *size,
                fields,
            }
        }
        Shape::Array { elem, .. } => Ir::Array {
            elem: Box::new(ir_of(elem, target)),
            elem_size: elem.size(target),
        },
    }
}

/// The struct element holding `offset`: the last one starting at or
/// before it (LLVM's `getElementContainingOffset`).
fn element_at(fields: &[(u64, Ir)], offset: u64) -> Option<&(u64, Ir)> {
    fields.iter().rev().find(|(at, _)| *at <= offset)
}

/// Whether bits `start..end` of `shape` hold only padding (clang's
/// `BitsContainNoUserData`).
fn no_user_data(shape: &Shape, start: u64, end: u64, target: Target) -> bool {
    if shape.size(target) * 8 <= start {
        return true;
    }
    let check = |offset: u64, field: &Shape| {
        if offset >= end {
            return None;
        }
        let field_start = start.saturating_sub(offset);
        Some(no_user_data(field, field_start, end - offset, target))
    };
    match shape {
        Shape::Array { elem, len } => {
            let elem_bits = elem.size(target) * 8;
            for index in 0..*len {
                match check(index * elem_bits, elem) {
                    None => break,
                    Some(false) => return false,
                    Some(true) => {}
                }
            }
            true
        }
        Shape::Struct { .. } | Shape::Union { .. } => {
            for (offset, field) in shape.fields() {
                match check(offset * 8, field) {
                    None => break,
                    Some(false) => return false,
                    Some(true) => {}
                }
            }
            true
        }
        _ => false,
    }
}

/// The integer-class type of the eightbyte at `offset` (clang's
/// `GetINTEGERTypeAtOffset`).
fn integer_at(ir: &Ir, ir_offset: u64, source: &Shape, offset: u64, target: Target) -> Piece {
    if ir_offset == 0 {
        match ir {
            Ir::Ptr => return Piece::Ptr,
            Ir::Int(64) => return Piece::Int(64),
            Ir::Int(bits @ (8 | 16 | 32)) => {
                let bits = u64::from(*bits);
                if no_user_data(source, offset * 8 + bits, offset * 8 + 64, target) {
                    return Piece::Int(u32::try_from(bits).unwrap_or(64));
                }
            }
            _ => {}
        }
    }
    match ir {
        Ir::Struct { size, fields } if ir_offset < *size => {
            if let Some((at, field)) = element_at(fields, ir_offset) {
                return integer_at(field, ir_offset - at, source, offset, target);
            }
        }
        Ir::Array { elem, elem_size } if *elem_size > 0 => {
            return integer_at(elem, ir_offset % elem_size, source, offset, target);
        }
        _ => {}
    }
    let left = source.size(target) - offset;
    Piece::Int(u32::try_from(left.min(8) * 8).unwrap_or(64))
}

/// The float type starting exactly at `offset`, if any (clang's
/// `getFPTypeAtOffset`).
fn float_at(ir: &Ir, offset: u64) -> Option<u32> {
    match ir {
        Ir::Float(bits) if offset == 0 => Some(*bits),
        Ir::Struct { fields, .. } => {
            let (at, field) = element_at(fields, offset)?;
            float_at(field, offset - at)
        }
        Ir::Array { elem, elem_size } if *elem_size > 0 => float_at(elem, offset % elem_size),
        _ => None,
    }
}

/// The SSE-class type of the eightbyte at `offset` (clang's
/// `GetSSETypeAtOffset`).
fn sse_at(ir: &Ir, ir_offset: u64, source: &Shape, offset: u64, target: Target) -> Piece {
    let left = source.size(target) - offset;
    let first = match float_at(ir, ir_offset) {
        None | Some(64) => return Piece::Float(64),
        Some(bits) => bits,
    };
    let first_size = u64::from(first / 8);
    let mut second = if left > first_size {
        float_at(ir, ir_offset + first_size)
    } else {
        None
    };
    if second.is_none() && first == 16 && left > 4 {
        second = float_at(ir, ir_offset + 4);
    }
    let Some(second) = second else {
        return Piece::Float(first);
    };
    match (first, second) {
        (32, 32) => Piece::Vector { bits: 32, count: 2 },
        (16, 16) => {
            let third = if left > 4 {
                float_at(ir, ir_offset + 4)
            } else {
                None
            };
            Piece::Vector {
                bits: 16,
                count: if third.is_some() { 4 } else { 2 },
            }
        }
        (16, _) | (_, 16) => Piece::Vector { bits: 16, count: 4 },
        _ => Piece::Float(64),
    }
}

/// The two eightbytes as parts, the low one widened so the high one sits
/// at offset 8 (clang's `GetX86_64ByValArgumentPair`).
fn sysv_pair(lo: Piece, hi: Piece) -> Vec<Part> {
    let lo_size = match &lo {
        Piece::Int(bits) => u64::from(bits.div_ceil(8).next_power_of_two()),
        Piece::Float(bits) => u64::from(*bits / 8),
        Piece::Vector { bits, count } => u64::from(bits * count / 8),
        Piece::Ptr | Piece::Array(..) => 8,
    };
    let hi_align = hi.x86_64_align();
    let lo = if lo_size.div_ceil(hi_align) * hi_align == 8 {
        lo
    } else if matches!(lo, Piece::Float(16 | 32)) {
        Piece::Float(64)
    } else {
        Piece::Int(64)
    };
    vec![
        Part {
            offset: 0,
            piece: lo,
        },
        Part {
            offset: 8,
            piece: hi,
        },
    ]
}

/// The register-class parts of an aggregate whose eightbytes are `lo` and
/// `hi` (neither `Memory`), with the integer and SSE registers they take.
fn sysv_parts(shape: &Shape, lo: Class, hi: Class, target: Target) -> (Vec<Part>, u32, u32) {
    let ir = ir_of(shape, target);
    let (mut ints, mut sses) = (0, 0);
    let low = match lo {
        Class::Integer => {
            ints += 1;
            Some(integer_at(&ir, 0, shape, 0, target))
        }
        Class::Sse => {
            sses += 1;
            Some(sse_at(&ir, 0, shape, 0, target))
        }
        _ => None,
    };
    let high = match hi {
        Class::Integer => {
            ints += 1;
            Some(integer_at(&ir, 8, shape, 8, target))
        }
        Class::Sse => {
            sses += 1;
            Some(sse_at(&ir, 8, shape, 8, target))
        }
        Class::SseUp => {
            // Only a 128-bit float fills both halves of one register.
            let one = single_element(shape, target).unwrap_or(shape);
            let piece = if matches!(one, Shape::Float(128)) {
                Piece::Float(128)
            } else {
                Piece::Vector { bits: 64, count: 2 }
            };
            return (vec![Part { offset: 0, piece }], ints, sses);
        }
        _ => None,
    };
    let parts = match (low, high) {
        (Some(lo), Some(hi)) => sysv_pair(lo, hi),
        (Some(piece), None) => vec![Part { offset: 0, piece }],
        (None, Some(piece)) => vec![Part { offset: 8, piece }],
        (None, None) => Vec::new(),
    };
    (parts, ints, sses)
}

fn sysv_signature(target: Target, params: &[Shape], ret: Option<&Shape>) -> Signature {
    let mut free_int = 6u32;
    let mut free_sse = 8u32;
    let ret = match ret {
        None => Return::Void,
        Some(shape) if shape.is_scalar() => Return::Scalar,
        Some(shape) => {
            let (lo, hi) = sysv_classify(shape, 0, target);
            if lo == Class::Memory {
                free_int -= 1;
                Return::Indirect {
                    align: shape.align(target),
                }
            } else {
                Return::Direct(sysv_parts(shape, lo, hi, target).0)
            }
        }
    };
    let mut passes = Vec::with_capacity(params.len());
    for shape in params {
        let (pass, ints, sses) = if shape.is_scalar() {
            match shape {
                Shape::Float(_) => (Pass::Scalar, 0, 1),
                _ => (Pass::Scalar, 1, 0),
            }
        } else {
            let (lo, hi) = sysv_classify(shape, 0, target);
            if lo == Class::Memory {
                (sysv_memory(shape, free_int, target), 0, 0)
            } else {
                let (parts, ints, sses) = sysv_parts(shape, lo, hi, target);
                (
                    Pass::Direct {
                        parts,
                        align_stack: None,
                    },
                    ints,
                    sses,
                )
            }
        };
        if free_int >= ints && free_sse >= sses {
            free_int -= ints;
            free_sse -= sses;
            passes.push(pass);
        } else {
            passes.push(sysv_memory(shape, free_int, target));
        }
    }
    Signature {
        params: passes,
        ret,
    }
}

/// An x86-64 argument passed in memory: `byval` at least 8-aligned, or,
/// once the integer registers are gone, an eightbyte as an integer
/// (clang's `getIndirectResult`).
fn sysv_memory(shape: &Shape, free_int: u32, target: Target) -> Pass {
    if shape.is_scalar() {
        return Pass::Scalar;
    }
    let align = shape.align(target).max(8);
    let size = shape.size(target);
    if free_int == 0 && align == 8 && size <= 8 {
        return direct(Piece::Int(u32::try_from(size * 8).unwrap_or(64)));
    }
    Pass::Indirect { byval: true, align }
}

// Windows x64 (clang's `WinX86_64ABIInfo`): 1, 2, 4 or 8 bytes as an
// integer, anything else by reference.

fn win64_small(shape: &Shape, target: Target) -> Option<Piece> {
    let size = shape.size(target);
    matches!(size, 1 | 2 | 4 | 8).then(|| Piece::Int(u32::try_from(size * 8).unwrap_or(64)))
}

fn win64_param(target: Target, shape: &Shape) -> Pass {
    if shape.is_scalar() {
        return Pass::Scalar;
    }
    win64_small(shape, target).map_or(
        Pass::Indirect {
            byval: false,
            align: shape.align(target),
        },
        direct,
    )
}

fn win64_return(target: Target, shape: &Shape) -> Return {
    if shape.is_scalar() {
        return Return::Scalar;
    }
    win64_small(shape, target).map_or(
        Return::Indirect {
            align: shape.align(target),
        },
        direct_return,
    )
}

// 32-bit x86 (clang's `X86_32ABIInfo`).

/// A record of 32- and 64-bit scalars with no padding, up to 16 bytes,
/// travels as its fields, which lay out on the stack as the record does
/// (clang's `canExpandIndirectArgument`).
fn i386_expansion(shape: &Shape, target: Target) -> Option<Vec<Part>> {
    let size = shape.size(target);
    if !shape.is_record() || size > 16 {
        return None;
    }
    let mut parts = Vec::new();
    let mut total = 0;
    for (offset, field) in shape.fields() {
        let bits = field.size(target) * 8;
        if !field.is_scalar() || !matches!(bits, 32 | 64) {
            return None;
        }
        total += bits / 8;
        parts.push(Part {
            offset,
            piece: scalar_piece(field),
        });
    }
    (total == size).then_some(parts)
}

fn i386_param(target: Target, shape: &Shape) -> Pass {
    if shape.is_scalar() {
        return Pass::Scalar;
    }
    match i386_expansion(shape, target) {
        Some(parts) => Pass::Direct {
            parts,
            align_stack: None,
        },
        None => Pass::Indirect {
            byval: true,
            align: 4,
        },
    }
}

/// Whether a register-sized record of register-sized fields comes back in
/// registers (clang's `shouldReturnTypeInRegister`).
fn i386_in_register(shape: &Shape, target: Target) -> bool {
    if !matches!(shape.size(target), 1 | 2 | 4 | 8) {
        return false;
    }
    match shape {
        Shape::Array { elem, .. } => i386_in_register(elem, target),
        Shape::Struct { .. } | Shape::Union { .. } => shape
            .fields()
            .into_iter()
            .filter(|(_, field)| field.size(target) > 0)
            .all(|(_, field)| i386_in_register(field, target)),
        _ => true,
    }
}

fn i386_return(target: Target, shape: &Shape) -> Return {
    if shape.is_scalar() {
        return Return::Scalar;
    }
    let indirect = Return::Indirect {
        align: shape.align(target),
    };
    // Windows and Darwin return small records in registers; the System V
    // psABI (Linux, bare metal) returns every record in memory.
    let windows = target.os == Os::Windows;
    if !(windows || target.os == Os::Macos) || !i386_in_register(shape, target) {
        return indirect;
    }
    if let Some(one) = single_element(shape, target) {
        if matches!(one, Shape::Ptr) || (!windows && matches!(one, Shape::Float(_))) {
            return direct_return(scalar_piece(one));
        }
    }
    let bits = u32::try_from(shape.size(target) * 8).unwrap_or(64);
    direct_return(Piece::Int(bits))
}

// AArch64 (clang's `AArch64ABIInfo`: AAPCS64, Darwin, Windows).

fn aarch64_hfa(shape: &Shape, target: Target) -> Option<(u32, u64)> {
    homogeneous(shape, target, |_| true)
}

fn hfa_piece(base: u32, members: u64) -> Piece {
    Piece::Array(
        Box::new(Piece::Float(base)),
        u32::try_from(members).unwrap_or(4),
    )
}

fn aarch64_param(target: Target, shape: &Shape) -> Pass {
    if shape.is_scalar() {
        return Pass::Scalar;
    }
    let align = shape.align(target);
    if let Some((base, members)) = aarch64_hfa(shape, target) {
        // AAPCS64 aligns the stack slot of an HFA; Darwin and Windows do not.
        let aapcs = !matches!(target.os, Os::Macos | Os::Windows);
        return Pass::Direct {
            parts: vec![Part {
                offset: 0,
                piece: hfa_piece(base, members),
            }],
            align_stack: aapcs.then_some(if align >= 16 { 16 } else { 8 }),
        };
    }
    let size = shape.size(target) * 8;
    if size <= 128 {
        let unit: u32 = if align >= 16 { 128 } else { 64 };
        let rounded = size.div_ceil(u64::from(unit)) * u64::from(unit);
        return direct(if rounded == u64::from(unit) {
            Piece::Int(unit)
        } else {
            Piece::Array(
                Box::new(Piece::Int(unit)),
                u32::try_from(rounded / u64::from(unit)).unwrap_or(2),
            )
        });
    }
    Pass::Indirect {
        byval: false,
        align,
    }
}

fn aarch64_return(target: Target, shape: &Shape) -> Return {
    if shape.is_scalar() {
        return Return::Scalar;
    }
    if let Some((base, members)) = aarch64_hfa(shape, target) {
        return direct_return(hfa_piece(base, members));
    }
    let size = shape.size(target) * 8;
    let align = shape.align(target);
    if size <= 64 {
        return direct_return(Piece::Int(u32::try_from(size).unwrap_or(64)));
    }
    if size <= 128 {
        let rounded = size.div_ceil(64) * 64;
        return direct_return(if align < 16 && rounded == 128 {
            Piece::Array(Box::new(Piece::Int(64)), 2)
        } else {
            Piece::Int(u32::try_from(rounded).unwrap_or(128))
        });
    }
    Return::Indirect { align }
}

// 32-bit Arm (clang's `ARMABIInfo`, AAPCS; Windows is hard-float VFP).

fn arm_hfa(shape: &Shape, target: Target) -> Option<(u32, u64)> {
    if target.os != Os::Windows {
        return None;
    }
    homogeneous(shape, target, |bits| matches!(bits, 32 | 64))
}

fn arm_param(target: Target, shape: &Shape) -> Pass {
    if shape.is_scalar() {
        return Pass::Scalar;
    }
    if let Some((base, members)) = arm_hfa(shape, target) {
        return direct(hfa_piece(base, members));
    }
    let align = shape.align(target);
    let size = shape.size(target);
    if size > 64 {
        return Pass::Indirect {
            byval: true,
            align: align.clamp(4, 8),
        };
    }
    let (unit, bytes) = if align <= 4 { (32, 4) } else { (64, 8) };
    direct(Piece::Array(
        Box::new(Piece::Int(unit)),
        u32::try_from(size.div_ceil(bytes)).unwrap_or(16),
    ))
}

fn arm_return(target: Target, shape: &Shape) -> Return {
    if shape.is_scalar() {
        return Return::Scalar;
    }
    if let Some((base, members)) = arm_hfa(shape, target) {
        return direct_return(hfa_piece(base, members));
    }
    match shape.size(target) {
        0..=1 => direct_return(Piece::Int(8)),
        2 => direct_return(Piece::Int(16)),
        3..=4 => direct_return(Piece::Int(32)),
        _ => Return::Indirect {
            align: shape.align(target),
        },
    }
}

// RISC-V, soft-float (clang's `RISCVABIInfo` with no FPRs).

fn riscv_direct(target: Target, shape: &Shape) -> Option<Piece> {
    let xlen: u32 = if target.arch == Arch::Riscv64 { 64 } else { 32 };
    let size = shape.size(target) * 8;
    if size <= u64::from(xlen) {
        Some(Piece::Int(xlen))
    } else if size <= u64::from(2 * xlen) {
        Some(if shape.align(target) * 8 == u64::from(2 * xlen) {
            Piece::Int(2 * xlen)
        } else {
            Piece::Array(Box::new(Piece::Int(xlen)), 2)
        })
    } else {
        None
    }
}

fn riscv_param(target: Target, shape: &Shape) -> Pass {
    if shape.is_scalar() {
        return Pass::Scalar;
    }
    riscv_direct(target, shape).map_or(
        Pass::Indirect {
            byval: false,
            align: shape.align(target),
        },
        direct,
    )
}

fn riscv_return(target: Target, shape: &Shape) -> Return {
    if shape.is_scalar() {
        return Return::Scalar;
    }
    riscv_direct(target, shape).map_or(
        Return::Indirect {
            align: shape.align(target),
        },
        direct_return,
    )
}

// WebAssembly (clang's `WebAssemblyABIInfo`, the default ABI).

fn wasm_param(target: Target, shape: &Shape) -> Pass {
    if shape.is_scalar() {
        return Pass::Scalar;
    }
    match single_element(shape, target) {
        Some(one) => direct(scalar_piece(one)),
        None => Pass::Indirect {
            byval: true,
            align: shape.align(target),
        },
    }
}

fn wasm_return(target: Target, shape: &Shape) -> Return {
    if shape.is_scalar() {
        return Return::Scalar;
    }
    match single_element(shape, target) {
        Some(one) => direct_return(scalar_piece(one)),
        None => Return::Indirect {
            align: shape.align(target),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(triple: &str) -> Target {
        Target::parse(triple).expect("a supported triple")
    }

    fn record(target: Target, fields: Vec<Shape>) -> Shape {
        let mut offset: u64 = 0;
        let mut align = 1;
        let mut placed = Vec::new();
        for field in fields {
            let a = field.align(target);
            offset = offset.div_ceil(a) * a;
            align = align.max(a);
            let size = field.size(target);
            placed.push((offset, field));
            offset += size;
        }
        Shape::Struct {
            size: offset.div_ceil(align) * align,
            align,
            fields: placed,
        }
    }

    fn parts(pass: &Pass) -> Vec<Piece> {
        match pass {
            Pass::Direct { parts, .. } => parts.iter().map(|p| p.piece.clone()).collect(),
            other => panic!("not direct: {other:?}"),
        }
    }

    #[test]
    fn sysv_pairs_follow_the_eightbytes() {
        let t = target("x86_64-unknown-linux-gnu");
        let double_int = record(t, vec![Shape::Float(64), Shape::Int(32)]);
        let sig = classify(t, &[double_int], None);
        assert_eq!(
            parts(&sig.params[0]),
            vec![Piece::Float(64), Piece::Int(32)]
        );
        let floats = record(t, vec![Shape::Float(32); 3]);
        let sig = classify(t, &[floats], None);
        assert_eq!(
            parts(&sig.params[0]),
            vec![Piece::Vector { bits: 32, count: 2 }, Piece::Float(32)]
        );
        let ints = record(t, vec![Shape::Int(32); 3]);
        let sig = classify(t, &[ints], None);
        assert_eq!(parts(&sig.params[0]), vec![Piece::Int(64), Piece::Int(32)]);
        let bytes = record(t, vec![Shape::Int(8); 3]);
        let sig = classify(t, &[bytes], None);
        assert_eq!(parts(&sig.params[0]), vec![Piece::Int(24)]);
    }

    #[test]
    fn sysv_large_records_go_byval_and_return_in_memory() {
        let t = target("x86_64-unknown-linux-gnu");
        let big = record(t, vec![Shape::Int(64); 3]);
        let sig = classify(t, std::slice::from_ref(&big), Some(&big));
        assert_eq!(
            sig.params[0],
            Pass::Indirect {
                byval: true,
                align: 8
            }
        );
        assert_eq!(sig.ret, Return::Indirect { align: 8 });
    }

    #[test]
    fn sysv_runs_out_of_registers() {
        let t = target("x86_64-unknown-linux-gnu");
        let pair = record(t, vec![Shape::Int(64), Shape::Int(64)]);
        let params = vec![pair.clone(), pair.clone(), pair.clone(), pair];
        let sig = classify(t, &params, None);
        assert!(matches!(sig.params[2], Pass::Direct { .. }));
        assert_eq!(
            sig.params[3],
            Pass::Indirect {
                byval: true,
                align: 8
            }
        );
    }

    #[test]
    fn win64_passes_powers_of_two_up_to_eight() {
        let t = target("x86_64-pc-windows-msvc");
        let small = record(t, vec![Shape::Int(32), Shape::Int(32)]);
        let odd = record(t, vec![Shape::Int(8); 3]);
        let sig = classify(t, &[small, odd.clone()], Some(&odd));
        assert_eq!(parts(&sig.params[0]), vec![Piece::Int(64)]);
        assert_eq!(
            sig.params[1],
            Pass::Indirect {
                byval: false,
                align: 1
            }
        );
        assert_eq!(sig.ret, Return::Indirect { align: 1 });
    }

    #[test]
    fn aarch64_hfas_and_integers() {
        let t = target("aarch64-unknown-linux-gnu");
        let hfa = record(t, vec![Shape::Float(32); 3]);
        let sig = classify(t, std::slice::from_ref(&hfa), Some(&hfa));
        assert_eq!(
            sig.params[0],
            Pass::Direct {
                parts: vec![Part {
                    offset: 0,
                    piece: hfa_piece(32, 3)
                }],
                align_stack: Some(8),
            }
        );
        let mixed = record(t, vec![Shape::Int(32), Shape::Float(32), Shape::Int(32)]);
        let sig = classify(t, std::slice::from_ref(&mixed), Some(&mixed));
        assert_eq!(
            parts(&sig.params[0]),
            vec![Piece::Array(Box::new(Piece::Int(64)), 2)]
        );
        assert_eq!(
            sig.ret,
            Return::Direct(vec![Part {
                offset: 0,
                piece: Piece::Array(Box::new(Piece::Int(64)), 2)
            }])
        );
    }

    #[test]
    fn i386_expands_or_copies() {
        let t = target("i686-unknown-linux-gnu");
        let flat = record(t, vec![Shape::Int(32), Shape::Float(32)]);
        let bytes = record(t, vec![Shape::Int(8), Shape::Int(32)]);
        let sig = classify(t, &[flat.clone(), bytes], Some(&flat));
        assert_eq!(
            parts(&sig.params[0]),
            vec![Piece::Int(32), Piece::Float(32)]
        );
        assert_eq!(
            sig.params[1],
            Pass::Indirect {
                byval: true,
                align: 4
            }
        );
        assert_eq!(sig.ret, Return::Indirect { align: 4 });
        let windows = target("i686-pc-windows-msvc");
        let sig = classify(windows, &[], Some(&flat));
        assert_eq!(sig.ret, direct_return(Piece::Int(64)));
    }
}
