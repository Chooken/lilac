//! Portable binary codec for [`LirModule`] (hand-written, no `serde`).
//!
//! This is the wire form reserved for the post-v1 compilation cache. It is
//! deterministic (layout entries are emitted sorted by key) and dependency
//! free. All integers are little-endian; strings are a `u32` byte length
//! followed by UTF-8 bytes; sequences are a `u32` count followed by their
//! elements. [`LirModule::decode`] never panics on malformed input: it maps
//! short reads to [`CodecError::Io`] and bad magic, version, or discriminant
//! tags to [`CodecError::Malformed`].

use std::io::{self, Read, Write};

use lilac_span::{BytePos, FileId, Span};

use crate::{
    ArithOp, ConvertOp, LirBlock, LirCallee, LirData, LirDataKind, LirFunc, LirInst, LirModule,
    LirOperand, LirTerm,
};

/// Wire-format version; bump on any normative codec change.
///
/// Version 2 added the `ArithOp::ILtU` tag (15), used by the LIR runtime's
/// unsigned `format_args` bounds checks. Version 3 appends the unsigned
/// `IGtU`/`IDivU`/`IModU` tags (16–18).
pub const LIR_CODEC_VERSION: u32 = 3;

/// Module magic: `b"LILACIR\0"`.
const MAGIC: &[u8; 8] = b"LILACIR\0";

/// A codec failure: an underlying I/O error or a malformed stream.
#[derive(Debug)]
pub enum CodecError {
    /// Reading or writing the underlying stream failed.
    Io(io::Error),
    /// The bytes are not a valid LIR module encoding.
    Malformed(&'static str),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "LIR codec I/O error: {err}"),
            Self::Malformed(what) => write!(f, "malformed LIR encoding: {what}"),
        }
    }
}

impl std::error::Error for CodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Malformed(_) => None,
        }
    }
}

impl From<io::Error> for CodecError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

/// Writes one raw byte.
fn put_u8(w: &mut impl Write, value: u8) -> io::Result<()> {
    w.write_all(&[value])
}

/// Writes a little-endian `u32`.
fn put_u32(w: &mut impl Write, value: u32) -> io::Result<()> {
    w.write_all(&value.to_le_bytes())
}

/// Writes a `bool` as one byte (`0`/`1`).
fn put_bool(w: &mut impl Write, value: bool) -> io::Result<()> {
    put_u8(w, u8::from(value))
}

/// Writes a `usize` length as a checked little-endian `u32`.
fn put_len(w: &mut impl Write, len: usize) -> io::Result<()> {
    let value = u32::try_from(len)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "length exceeds u32::MAX"))?;
    put_u32(w, value)
}

/// Writes a length-prefixed UTF-8 string.
fn put_str(w: &mut impl Write, text: &str) -> io::Result<()> {
    put_len(w, text.len())?;
    w.write_all(text.as_bytes())
}

/// Writes a [`Span`] as `file`, `lo`, `hi`.
fn put_span(w: &mut impl Write, span: Span) -> io::Result<()> {
    put_u32(w, span.file.0)?;
    put_u32(w, span.lo.0)?;
    put_u32(w, span.hi.0)
}

/// Writes a length-prefixed sequence of strings.
fn put_strings(w: &mut impl Write, items: &[String]) -> io::Result<()> {
    put_len(w, items.len())?;
    for item in items {
        put_str(w, item)?;
    }
    Ok(())
}

/// Writes a length-prefixed sequence of `(name, type)` pairs.
fn put_pairs(w: &mut impl Write, pairs: &[(String, String)]) -> io::Result<()> {
    put_len(w, pairs.len())?;
    for (name, ty) in pairs {
        put_str(w, name)?;
        put_str(w, ty)?;
    }
    Ok(())
}

/// Writes an operand (tag + payload).
fn put_operand(w: &mut impl Write, operand: &LirOperand) -> io::Result<()> {
    match operand {
        LirOperand::Reg(name) => {
            put_u8(w, 0)?;
            put_str(w, name)
        }
        LirOperand::Place(name) => {
            put_u8(w, 1)?;
            put_str(w, name)
        }
    }
}

/// Writes a call target (tag + payload).
fn put_callee(w: &mut impl Write, callee: &LirCallee) -> io::Result<()> {
    match callee {
        LirCallee::Reg(name) => {
            put_u8(w, 0)?;
            put_str(w, name)
        }
        LirCallee::Func(name) => {
            put_u8(w, 1)?;
            put_str(w, name)
        }
        LirCallee::Builtin(name) => {
            put_u8(w, 2)?;
            put_str(w, name)
        }
    }
}

/// Writes an arithmetic operator tag.
fn put_arith(w: &mut impl Write, op: &ArithOp) -> io::Result<()> {
    let tag = match op {
        ArithOp::IAdd => 0,
        ArithOp::ISub => 1,
        ArithOp::IMul => 2,
        ArithOp::IDiv => 3,
        ArithOp::IMod => 4,
        ArithOp::FAdd => 5,
        ArithOp::FSub => 6,
        ArithOp::FMul => 7,
        ArithOp::FDiv => 8,
        ArithOp::FMod => 9,
        ArithOp::Eq => 10,
        ArithOp::ILt => 11,
        ArithOp::IGt => 12,
        ArithOp::FLt => 13,
        ArithOp::FGt => 14,
        ArithOp::ILtU => 15,
        ArithOp::IGtU => 16,
        ArithOp::IDivU => 17,
        ArithOp::IModU => 18,
    };
    put_u8(w, tag)
}

/// Writes a conversion operator tag.
fn put_convert(w: &mut impl Write, op: &ConvertOp) -> io::Result<()> {
    let tag = match op {
        ConvertOp::Trunc => 0,
        ConvertOp::ExtendZero => 1,
    };
    put_u8(w, tag)
}

/// Writes a data-const kind (tag + payload).
fn put_data_kind(w: &mut impl Write, kind: &LirDataKind) -> io::Result<()> {
    match kind {
        LirDataKind::Str(text) => {
            put_u8(w, 0)?;
            put_str(w, text)
        }
        LirDataKind::NumLit(text) => {
            put_u8(w, 1)?;
            put_str(w, text)
        }
    }
}

/// Writes one data const.
fn put_data(w: &mut impl Write, data: &LirData) -> io::Result<()> {
    put_str(w, &data.name)?;
    put_data_kind(w, &data.kind)?;
    put_span(w, data.span)
}

/// Writes one instruction (exhaustive tag + fields in declaration order).
#[allow(clippy::too_many_lines)]
fn put_inst(w: &mut impl Write, inst: &LirInst) -> io::Result<()> {
    match inst {
        LirInst::CopyReg { src, dst, ty, span } => {
            put_u8(w, 0)?;
            put_operand(w, src)?;
            put_str(w, dst)?;
            put_str(w, ty)?;
            put_span(w, *span)
        }
        LirInst::CopyPlace { src, dst, span } => {
            put_u8(w, 1)?;
            put_operand(w, src)?;
            put_str(w, dst)?;
            put_span(w, *span)
        }
        LirInst::Const { dst, lit, ty, span } => {
            put_u8(w, 2)?;
            put_str(w, dst)?;
            put_str(w, lit)?;
            put_str(w, ty)?;
            put_span(w, *span)
        }
        LirInst::Load { src, dst, ty, span } => {
            put_u8(w, 3)?;
            put_str(w, src)?;
            put_str(w, dst)?;
            put_str(w, ty)?;
            put_span(w, *span)
        }
        LirInst::Addr {
            place,
            dst,
            ty,
            span,
        } => {
            put_u8(w, 4)?;
            put_str(w, place)?;
            put_str(w, dst)?;
            put_str(w, ty)?;
            put_span(w, *span)
        }
        LirInst::AddrFunc {
            func,
            dst,
            ty,
            span,
        } => {
            put_u8(w, 5)?;
            put_str(w, func)?;
            put_str(w, dst)?;
            put_str(w, ty)?;
            put_span(w, *span)
        }
        LirInst::Call {
            func,
            ins,
            outs,
            span,
        } => {
            put_u8(w, 6)?;
            put_callee(w, func)?;
            put_strings(w, ins)?;
            put_strings(w, outs)?;
            put_span(w, *span)
        }
        LirInst::Field {
            obj,
            name,
            idx,
            dst,
            ty,
            span,
        } => {
            put_u8(w, 7)?;
            put_str(w, obj)?;
            put_str(w, name)?;
            put_u32(w, *idx)?;
            put_str(w, dst)?;
            put_str(w, ty)?;
            put_span(w, *span)
        }
        LirInst::StoreField {
            value,
            obj,
            name,
            idx,
            span,
        } => {
            put_u8(w, 8)?;
            put_operand(w, value)?;
            put_str(w, obj)?;
            put_str(w, name)?;
            put_u32(w, *idx)?;
            put_span(w, *span)
        }
        LirInst::Store { src, dst, span } => {
            put_u8(w, 9)?;
            put_operand(w, src)?;
            put_str(w, dst)?;
            put_span(w, *span)
        }
        LirInst::Convert {
            op,
            src,
            dst,
            ty,
            span,
        } => {
            put_u8(w, 10)?;
            put_convert(w, op)?;
            put_str(w, src)?;
            put_str(w, dst)?;
            put_str(w, ty)?;
            put_span(w, *span)
        }
        LirInst::FieldAddr {
            obj,
            name,
            idx,
            dst,
            ty,
            span,
        } => {
            put_u8(w, 11)?;
            put_str(w, obj)?;
            put_str(w, name)?;
            put_u32(w, *idx)?;
            put_str(w, dst)?;
            put_str(w, ty)?;
            put_span(w, *span)
        }
        LirInst::Alloca { ty, place, span } => {
            put_u8(w, 12)?;
            put_str(w, ty)?;
            put_str(w, place)?;
            put_span(w, *span)
        }
        LirInst::Drop { place, span } => {
            put_u8(w, 13)?;
            put_str(w, place)?;
            put_span(w, *span)
        }
        LirInst::Arith {
            op,
            lhs,
            rhs,
            dst,
            ty,
            span,
        } => {
            put_u8(w, 14)?;
            put_arith(w, op)?;
            put_str(w, lhs)?;
            put_str(w, rhs)?;
            put_str(w, dst)?;
            put_str(w, ty)?;
            put_span(w, *span)
        }
        LirInst::CompileError {
            msg,
            msg_span,
            call_span,
        } => {
            put_u8(w, 15)?;
            put_str(w, msg)?;
            put_span(w, *msg_span)?;
            put_span(w, *call_span)
        }
        LirInst::FoldIntrinsic {
            name,
            arg,
            dst,
            ty,
            span,
        } => {
            put_u8(w, 16)?;
            put_str(w, name)?;
            put_operand(w, arg)?;
            put_str(w, dst)?;
            put_str(w, ty)?;
            put_span(w, *span)
        }
    }
}

/// Writes one terminator (exhaustive tag + fields in declaration order).
fn put_term(w: &mut impl Write, term: &LirTerm) -> io::Result<()> {
    match term {
        LirTerm::Return { span } => {
            put_u8(w, 0)?;
            put_span(w, *span)
        }
        LirTerm::Branch { target, span } => {
            put_u8(w, 1)?;
            put_str(w, target)?;
            put_span(w, *span)
        }
        LirTerm::BranchIf {
            cond,
            then,
            els,
            span,
        } => {
            put_u8(w, 2)?;
            put_str(w, cond)?;
            put_str(w, then)?;
            match els {
                Some(els) => {
                    put_u8(w, 1)?;
                    put_str(w, els)?;
                }
                None => put_u8(w, 0)?,
            }
            put_span(w, *span)
        }
        LirTerm::Panic { msg, span } => {
            put_u8(w, 3)?;
            put_str(w, msg)?;
            put_span(w, *span)
        }
        LirTerm::Unreachable { span } => {
            put_u8(w, 4)?;
            put_span(w, *span)
        }
    }
}

/// Writes one block.
fn put_block(w: &mut impl Write, block: &LirBlock) -> io::Result<()> {
    put_str(w, &block.label)?;
    put_len(w, block.insts.len())?;
    for inst in &block.insts {
        put_inst(w, inst)?;
    }
    put_term(w, &block.term)?;
    put_span(w, block.span)
}

/// Writes one function (signature, extern flag, then blocks).
fn put_func(w: &mut impl Write, func: &LirFunc) -> io::Result<()> {
    put_str(w, &func.name)?;
    put_pairs(w, &func.ins)?;
    put_pairs(w, &func.outs)?;
    put_bool(w, func.is_extern)?;
    put_len(w, func.blocks.len())?;
    for block in &func.blocks {
        put_block(w, block)?;
    }
    Ok(())
}

/// Reads one raw byte.
fn get_u8(r: &mut impl Read) -> Result<u8, CodecError> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0])
}

/// Reads a little-endian `u32`.
fn get_u32(r: &mut impl Read) -> Result<u32, CodecError> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

/// Reads a `bool` byte, rejecting any value other than `0`/`1`.
fn get_bool(r: &mut impl Read) -> Result<bool, CodecError> {
    match get_u8(r)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(CodecError::Malformed("invalid bool tag")),
    }
}

/// Reads a `u32` length as a `usize`.
fn get_len(r: &mut impl Read) -> Result<usize, CodecError> {
    let value = get_u32(r)?;
    usize::try_from(value).map_err(|_| CodecError::Malformed("length exceeds address space"))
}

/// Reads a length-prefixed UTF-8 string without preallocating on the claimed
/// length (so a bogus huge length cannot OOM before the stream ends).
fn get_str(r: &mut impl Read) -> Result<String, CodecError> {
    let len = get_len(r)?;
    let mut bytes: Vec<u8> = Vec::new();
    let mut remaining = len;
    let mut chunk = [0u8; 4096];
    while remaining > 0 {
        let want = remaining.min(chunk.len());
        r.read_exact(&mut chunk[..want])?;
        bytes.extend_from_slice(&chunk[..want]);
        remaining -= want;
    }
    String::from_utf8(bytes).map_err(|_| CodecError::Malformed("string is not valid UTF-8"))
}

/// Reads a [`Span`] as `file`, `lo`, `hi`.
fn get_span(r: &mut impl Read) -> Result<Span, CodecError> {
    let file = FileId(get_u32(r)?);
    let lo = BytePos(get_u32(r)?);
    let hi = BytePos(get_u32(r)?);
    Ok(Span { lo, hi, file })
}

/// Reads a length-prefixed sequence of strings.
fn get_strings(r: &mut impl Read) -> Result<Vec<String>, CodecError> {
    let count = get_len(r)?;
    let mut items = Vec::new();
    for _ in 0..count {
        items.push(get_str(r)?);
    }
    Ok(items)
}

/// Reads a length-prefixed sequence of `(name, type)` pairs.
fn get_pairs(r: &mut impl Read) -> Result<Vec<(String, String)>, CodecError> {
    let count = get_len(r)?;
    let mut pairs = Vec::new();
    for _ in 0..count {
        let name = get_str(r)?;
        let ty = get_str(r)?;
        pairs.push((name, ty));
    }
    Ok(pairs)
}

/// Reads an operand (tag + payload).
fn get_operand(r: &mut impl Read) -> Result<LirOperand, CodecError> {
    match get_u8(r)? {
        0 => Ok(LirOperand::Reg(get_str(r)?)),
        1 => Ok(LirOperand::Place(get_str(r)?)),
        _ => Err(CodecError::Malformed("invalid LirOperand tag")),
    }
}

/// Reads a call target (tag + payload).
fn get_callee(r: &mut impl Read) -> Result<LirCallee, CodecError> {
    match get_u8(r)? {
        0 => Ok(LirCallee::Reg(get_str(r)?)),
        1 => Ok(LirCallee::Func(get_str(r)?)),
        2 => Ok(LirCallee::Builtin(get_str(r)?)),
        _ => Err(CodecError::Malformed("invalid LirCallee tag")),
    }
}

/// Reads an arithmetic operator tag.
fn get_arith(r: &mut impl Read) -> Result<ArithOp, CodecError> {
    Ok(match get_u8(r)? {
        0 => ArithOp::IAdd,
        1 => ArithOp::ISub,
        2 => ArithOp::IMul,
        3 => ArithOp::IDiv,
        4 => ArithOp::IMod,
        5 => ArithOp::FAdd,
        6 => ArithOp::FSub,
        7 => ArithOp::FMul,
        8 => ArithOp::FDiv,
        9 => ArithOp::FMod,
        10 => ArithOp::Eq,
        11 => ArithOp::ILt,
        12 => ArithOp::IGt,
        13 => ArithOp::FLt,
        14 => ArithOp::FGt,
        15 => ArithOp::ILtU,
        16 => ArithOp::IGtU,
        17 => ArithOp::IDivU,
        18 => ArithOp::IModU,
        _ => return Err(CodecError::Malformed("invalid ArithOp tag")),
    })
}

/// Reads a conversion operator tag.
fn get_convert(r: &mut impl Read) -> Result<ConvertOp, CodecError> {
    Ok(match get_u8(r)? {
        0 => ConvertOp::Trunc,
        1 => ConvertOp::ExtendZero,
        _ => return Err(CodecError::Malformed("invalid ConvertOp tag")),
    })
}

/// Reads a data-const kind (tag + payload).
fn get_data_kind(r: &mut impl Read) -> Result<LirDataKind, CodecError> {
    match get_u8(r)? {
        0 => Ok(LirDataKind::Str(get_str(r)?)),
        1 => Ok(LirDataKind::NumLit(get_str(r)?)),
        _ => Err(CodecError::Malformed("invalid LirDataKind tag")),
    }
}

/// Reads one data const.
fn get_data(r: &mut impl Read) -> Result<LirData, CodecError> {
    let name = get_str(r)?;
    let kind = get_data_kind(r)?;
    let span = get_span(r)?;
    Ok(LirData { name, kind, span })
}

/// Reads one instruction (exhaustive tag match).
#[allow(clippy::too_many_lines)]
fn get_inst(r: &mut impl Read) -> Result<LirInst, CodecError> {
    let tag = get_u8(r)?;
    match tag {
        0 => {
            let src = get_operand(r)?;
            let dst = get_str(r)?;
            let ty = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::CopyReg { src, dst, ty, span })
        }
        1 => {
            let src = get_operand(r)?;
            let dst = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::CopyPlace { src, dst, span })
        }
        2 => {
            let dst = get_str(r)?;
            let lit = get_str(r)?;
            let ty = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::Const { dst, lit, ty, span })
        }
        3 => {
            let src = get_str(r)?;
            let dst = get_str(r)?;
            let ty = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::Load { src, dst, ty, span })
        }
        4 => {
            let place = get_str(r)?;
            let dst = get_str(r)?;
            let ty = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::Addr {
                place,
                dst,
                ty,
                span,
            })
        }
        5 => {
            let func = get_str(r)?;
            let dst = get_str(r)?;
            let ty = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::AddrFunc {
                func,
                dst,
                ty,
                span,
            })
        }
        6 => {
            let func = get_callee(r)?;
            let ins = get_strings(r)?;
            let outs = get_strings(r)?;
            let span = get_span(r)?;
            Ok(LirInst::Call {
                func,
                ins,
                outs,
                span,
            })
        }
        7 => {
            let obj = get_str(r)?;
            let name = get_str(r)?;
            let idx = get_u32(r)?;
            let dst = get_str(r)?;
            let ty = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::Field {
                obj,
                name,
                idx,
                dst,
                ty,
                span,
            })
        }
        8 => {
            let value = get_operand(r)?;
            let obj = get_str(r)?;
            let name = get_str(r)?;
            let idx = get_u32(r)?;
            let span = get_span(r)?;
            Ok(LirInst::StoreField {
                value,
                obj,
                name,
                idx,
                span,
            })
        }
        9 => {
            let src = get_operand(r)?;
            let dst = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::Store { src, dst, span })
        }
        10 => {
            let op = get_convert(r)?;
            let src = get_str(r)?;
            let dst = get_str(r)?;
            let ty = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::Convert {
                op,
                src,
                dst,
                ty,
                span,
            })
        }
        11 => {
            let obj = get_str(r)?;
            let name = get_str(r)?;
            let idx = get_u32(r)?;
            let dst = get_str(r)?;
            let ty = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::FieldAddr {
                obj,
                name,
                idx,
                dst,
                ty,
                span,
            })
        }
        12 => {
            let ty = get_str(r)?;
            let place = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::Alloca { ty, place, span })
        }
        13 => {
            let place = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::Drop { place, span })
        }
        14 => {
            let op = get_arith(r)?;
            let lhs = get_str(r)?;
            let rhs = get_str(r)?;
            let dst = get_str(r)?;
            let ty = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::Arith {
                op,
                lhs,
                rhs,
                dst,
                ty,
                span,
            })
        }
        15 => {
            let msg = get_str(r)?;
            let msg_span = get_span(r)?;
            let call_span = get_span(r)?;
            Ok(LirInst::CompileError {
                msg,
                msg_span,
                call_span,
            })
        }
        16 => {
            let name = get_str(r)?;
            let arg = get_operand(r)?;
            let dst = get_str(r)?;
            let ty = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirInst::FoldIntrinsic {
                name,
                arg,
                dst,
                ty,
                span,
            })
        }
        _ => Err(CodecError::Malformed("invalid LirInst tag")),
    }
}

/// Reads one terminator (exhaustive tag match).
fn get_term(r: &mut impl Read) -> Result<LirTerm, CodecError> {
    let tag = get_u8(r)?;
    match tag {
        0 => {
            let span = get_span(r)?;
            Ok(LirTerm::Return { span })
        }
        1 => {
            let target = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirTerm::Branch { target, span })
        }
        2 => {
            let cond = get_str(r)?;
            let then = get_str(r)?;
            let els = match get_u8(r)? {
                0 => None,
                1 => Some(get_str(r)?),
                _ => return Err(CodecError::Malformed("invalid Option tag")),
            };
            let span = get_span(r)?;
            Ok(LirTerm::BranchIf {
                cond,
                then,
                els,
                span,
            })
        }
        3 => {
            let msg = get_str(r)?;
            let span = get_span(r)?;
            Ok(LirTerm::Panic { msg, span })
        }
        4 => {
            let span = get_span(r)?;
            Ok(LirTerm::Unreachable { span })
        }
        _ => Err(CodecError::Malformed("invalid LirTerm tag")),
    }
}

/// Reads one block.
fn get_block(r: &mut impl Read) -> Result<LirBlock, CodecError> {
    let label = get_str(r)?;
    let count = get_len(r)?;
    let mut insts = Vec::new();
    for _ in 0..count {
        insts.push(get_inst(r)?);
    }
    let term = get_term(r)?;
    let span = get_span(r)?;
    Ok(LirBlock {
        label,
        insts,
        term,
        span,
    })
}

/// Reads one function.
fn get_func(r: &mut impl Read) -> Result<LirFunc, CodecError> {
    let name = get_str(r)?;
    let ins = get_pairs(r)?;
    let outs = get_pairs(r)?;
    let is_extern = get_bool(r)?;
    let count = get_len(r)?;
    let mut blocks = Vec::new();
    for _ in 0..count {
        blocks.push(get_block(r)?);
    }
    Ok(LirFunc {
        name,
        ins,
        outs,
        is_extern,
        blocks,
    })
}

impl LirModule {
    /// Encodes this module to `w` in the portable binary wire format.
    pub fn encode(&self, w: &mut impl Write) -> io::Result<()> {
        w.write_all(MAGIC)?;
        put_u32(w, LIR_CODEC_VERSION)?;
        put_str(w, &self.version)?;
        put_str(w, &self.target)?;
        put_len(w, self.data.len())?;
        for data in &self.data {
            put_data(w, data)?;
        }
        put_len(w, self.funcs.len())?;
        for func in &self.funcs {
            put_func(w, func)?;
        }
        let mut names: Vec<&String> = self.layouts.keys().collect();
        names.sort_unstable();
        put_len(w, names.len())?;
        for name in names {
            put_str(w, name)?;
            put_pairs(w, &self.layouts[name])?;
        }
        Ok(())
    }

    /// Decodes a module from the portable binary wire format.
    ///
    /// Returns [`CodecError::Io`] on a short read and [`CodecError::Malformed`]
    /// for bad magic, an unsupported codec version, or an unknown tag.
    pub fn decode(r: &mut impl Read) -> Result<Self, CodecError> {
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(CodecError::Malformed("bad magic"));
        }
        if get_u32(r)? != LIR_CODEC_VERSION {
            return Err(CodecError::Malformed("unsupported codec version"));
        }
        let version = get_str(r)?;
        let target = get_str(r)?;
        let data_count = get_len(r)?;
        let mut data = Vec::new();
        for _ in 0..data_count {
            data.push(get_data(r)?);
        }
        let func_count = get_len(r)?;
        let mut funcs = Vec::new();
        for _ in 0..func_count {
            funcs.push(get_func(r)?);
        }
        let layout_count = get_len(r)?;
        let mut layouts = std::collections::HashMap::new();
        for _ in 0..layout_count {
            let name = get_str(r)?;
            let members = get_pairs(r)?;
            layouts.insert(name, members);
        }
        Ok(Self {
            version,
            target,
            data,
            funcs,
            layouts,
        })
    }

    /// Deterministic `FNV-1a` 64-bit hash of the encoded module.
    ///
    /// This is a cache key for the post-v1 LIR cache, not a security hash:
    /// it is unkeyed, fast, and trivially collidable by an adversary.
    pub fn content_hash(&self) -> u64 {
        let mut hasher = Fnv64::new();
        let _ = self.encode(&mut hasher);
        hasher.finish()
    }
}

/// `FNV-1a` 64-bit streaming hasher; its `Write` impl never fails.
struct Fnv64 {
    state: u64,
}

impl Fnv64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    /// Fresh hasher seeded with the FNV offset basis.
    fn new() -> Self {
        Self {
            state: Self::OFFSET_BASIS,
        }
    }

    /// Finalizes the hash.
    fn finish(self) -> u64 {
        self.state
    }
}

impl Write for Fnv64 {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &byte in buf {
            self.state ^= u64::from(byte);
            self.state = self.state.wrapping_mul(Self::PRIME);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::canonical_module;
    use crate::{
        ArithOp, ConvertOp, LirBlock, LirCallee, LirData, LirDataKind, LirFunc, LirInst,
        LirOperand, LirTerm,
    };
    use lilac_span::Span;

    fn sample() -> LirModule {
        let mut module = canonical_module();
        module.data.push(LirData {
            name: "c0".to_owned(),
            kind: LirDataKind::Str("hi".to_owned()),
            span: Span::dummy(),
        });
        module.layouts.insert(
            "#Number".to_owned(),
            vec![
                ("tag".to_owned(), "#bit8".to_owned()),
                ("val".to_owned(), "#bit32".to_owned()),
            ],
        );
        module
    }

    #[test]
    fn round_trips_to_identical_text() {
        let module = sample();
        let mut bytes = Vec::new();
        module.encode(&mut bytes).expect("encodes");
        let decoded = LirModule::decode(&mut &bytes[..]).expect("decodes");
        assert_eq!(decoded.to_text(), module.to_text());
    }

    #[test]
    fn hash_is_stable_and_layout_order_independent() {
        let a = sample();
        let mut b = sample();
        b.layouts.insert("#Extra".to_owned(), Vec::new());
        b.layouts.remove("#Extra");
        assert_eq!(a.content_hash(), b.content_hash());
        assert_eq!(a.content_hash(), a.content_hash());
    }

    /// Builds a module exercising every `LirInst` and `LirTerm` variant, both
    /// `ConvertOp`s, all 19 `ArithOp`s, all three `LirCallee`s, both
    /// `LirDataKind`s, and a layout.
    #[allow(clippy::too_many_lines)]
    fn all_variants_module() -> LirModule {
        let span = Span::dummy();
        let mut insts = vec![
            LirInst::CopyReg {
                src: LirOperand::Reg("r0".to_owned()),
                dst: "r1".to_owned(),
                ty: "#bit32".to_owned(),
                span,
            },
            LirInst::CopyPlace {
                src: LirOperand::Place("p0".to_owned()),
                dst: "p1".to_owned(),
                span,
            },
            LirInst::Const {
                dst: "r2".to_owned(),
                lit: "!d0".to_owned(),
                ty: "#byte_buffer".to_owned(),
                span,
            },
            LirInst::Load {
                src: "p1".to_owned(),
                dst: "r3".to_owned(),
                ty: "#bit32".to_owned(),
                span,
            },
            LirInst::Addr {
                place: "p1".to_owned(),
                dst: "r4".to_owned(),
                ty: "ref #bit32".to_owned(),
                span,
            },
            LirInst::AddrFunc {
                func: "f1".to_owned(),
                dst: "r5".to_owned(),
                ty: "ref #bit32".to_owned(),
                span,
            },
            LirInst::Call {
                func: LirCallee::Reg("r0".to_owned()),
                ins: vec!["p0".to_owned()],
                outs: vec!["p1".to_owned()],
                span,
            },
            LirInst::Call {
                func: LirCallee::Func("f1".to_owned()),
                ins: Vec::new(),
                outs: vec!["p0".to_owned()],
                span,
            },
            LirInst::Call {
                func: LirCallee::Builtin("@lilac_abort".to_owned()),
                ins: Vec::new(),
                outs: Vec::new(),
                span,
            },
            LirInst::Field {
                obj: "p1".to_owned(),
                name: "x".to_owned(),
                idx: 3,
                dst: "r6".to_owned(),
                ty: "#bit32".to_owned(),
                span,
            },
            LirInst::StoreField {
                value: LirOperand::Reg("r0".to_owned()),
                obj: "p1".to_owned(),
                name: "x".to_owned(),
                idx: 3,
                span,
            },
            LirInst::Store {
                src: LirOperand::Place("p0".to_owned()),
                dst: "p1".to_owned(),
                span,
            },
            LirInst::Convert {
                op: ConvertOp::Trunc,
                src: "r0".to_owned(),
                dst: "r7".to_owned(),
                ty: "#bit8".to_owned(),
                span,
            },
            LirInst::Convert {
                op: ConvertOp::ExtendZero,
                src: "r0".to_owned(),
                dst: "r8".to_owned(),
                ty: "#bit64".to_owned(),
                span,
            },
            LirInst::FieldAddr {
                obj: "p1".to_owned(),
                name: "x".to_owned(),
                idx: 3,
                dst: "r9".to_owned(),
                ty: "ref #bit32".to_owned(),
                span,
            },
            LirInst::Alloca {
                ty: "#bit32".to_owned(),
                place: "p2".to_owned(),
                span,
            },
            LirInst::Drop {
                place: "p2".to_owned(),
                span,
            },
        ];
        for (i, op) in [
            ArithOp::IAdd,
            ArithOp::ISub,
            ArithOp::IMul,
            ArithOp::IDiv,
            ArithOp::IDivU,
            ArithOp::IMod,
            ArithOp::IModU,
            ArithOp::FAdd,
            ArithOp::FSub,
            ArithOp::FMul,
            ArithOp::FDiv,
            ArithOp::FMod,
            ArithOp::Eq,
            ArithOp::ILt,
            ArithOp::ILtU,
            ArithOp::IGt,
            ArithOp::IGtU,
            ArithOp::FLt,
            ArithOp::FGt,
        ]
        .into_iter()
        .enumerate()
        {
            insts.push(LirInst::Arith {
                op,
                lhs: "r0".to_owned(),
                rhs: "r1".to_owned(),
                dst: format!("a{i}"),
                ty: "#bit32".to_owned(),
                span,
            });
        }
        insts.push(LirInst::CompileError {
            msg: "boom".to_owned(),
            msg_span: span,
            call_span: span,
        });
        insts.push(LirInst::FoldIntrinsic {
            name: "@as_int".to_owned(),
            arg: LirOperand::Reg("r0".to_owned()),
            dst: "r10".to_owned(),
            ty: "#bit8".to_owned(),
            span,
        });

        let blocks = vec![
            LirBlock {
                label: "entry".to_owned(),
                insts,
                term: LirTerm::Return { span },
                span,
            },
            LirBlock {
                label: "b1".to_owned(),
                insts: Vec::new(),
                term: LirTerm::Branch {
                    target: "entry".to_owned(),
                    span,
                },
                span,
            },
            LirBlock {
                label: "b2".to_owned(),
                insts: Vec::new(),
                term: LirTerm::BranchIf {
                    cond: "r1".to_owned(),
                    then: "entry".to_owned(),
                    els: Some("b1".to_owned()),
                    span,
                },
                span,
            },
            LirBlock {
                label: "b3".to_owned(),
                insts: Vec::new(),
                term: LirTerm::BranchIf {
                    cond: "r1".to_owned(),
                    then: "entry".to_owned(),
                    els: None,
                    span,
                },
                span,
            },
            LirBlock {
                label: "b4".to_owned(),
                insts: Vec::new(),
                term: LirTerm::Panic {
                    msg: "d0".to_owned(),
                    span,
                },
                span,
            },
            LirBlock {
                label: "b5".to_owned(),
                insts: Vec::new(),
                term: LirTerm::Unreachable { span },
                span,
            },
        ];

        let mut module = LirModule::new("x86_64-unknown-none");
        module.data.push(LirData {
            name: "d0".to_owned(),
            kind: LirDataKind::Str("hi".to_owned()),
            span,
        });
        module.data.push(LirData {
            name: "d1".to_owned(),
            kind: LirDataKind::NumLit("10.5".to_owned()),
            span,
        });
        module.funcs.push(LirFunc {
            name: "all".to_owned(),
            ins: vec![("p0".to_owned(), "#bit32".to_owned())],
            outs: vec![("p1".to_owned(), "ref #bit32".to_owned())],
            is_extern: false,
            blocks,
        });
        module.layouts.insert(
            "#Number".to_owned(),
            vec![
                ("tag".to_owned(), "#bit8".to_owned()),
                ("val".to_owned(), "#bit32".to_owned()),
            ],
        );
        module
    }

    #[test]
    fn round_trips_every_variant_byte_identical() {
        let module = all_variants_module();
        let mut bytes = Vec::new();
        module.encode(&mut bytes).expect("encodes");
        let decoded = LirModule::decode(&mut &bytes[..]).expect("decodes");
        assert_eq!(decoded.to_text(), module.to_text());
        assert_eq!(decoded.content_hash(), module.content_hash());
        let mut reencoded = Vec::new();
        decoded.encode(&mut reencoded).expect("re-encodes");
        assert_eq!(
            reencoded, bytes,
            "decode(encode(m)) must re-encode byte-identically"
        );
    }

    #[test]
    fn malformed_input_errors_without_panicking() {
        assert!(matches!(
            LirModule::decode(&mut &b"XXXXXXXXgarbage"[..]),
            Err(CodecError::Malformed(_))
        ));
        let mut bytes = Vec::new();
        sample().encode(&mut bytes).expect("encodes");
        bytes.truncate(bytes.len() / 2);
        assert!(LirModule::decode(&mut &bytes[..]).is_err());
    }
}
