//! Canonical `.lbc` text codec.
//!
//! [`Program::to_text`] renders a program as deterministic, LF-only, BOM-free
//! text and [`Program::parse_text`] is its inverse. The exact per-instruction
//! syntax is defined by `to_text`; every instruction prints its resolved
//! operands.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::isa::{
    verify, ArithOp, Builtin, ConvertOp, Data, Func, Instr, Line, Operand, Program, TypeId,
    TypeInfo, LBC_VERSION,
};

impl Program {
    /// Verifies this program (see [`crate::isa::verify`]).
    ///
    /// # Errors
    ///
    /// Returns a human-readable message describing the first violation.
    pub fn verify(&self) -> Result<(), String> {
        verify(self)
    }

    /// Renders the canonical `.lbc` text form.
    ///
    /// The output is deterministic (types, data, and functions in index order;
    /// code in pc order), LF-only, and BOM-free, so
    /// [`Program::parse_text`]`(`[`Program::to_text`]`(p))` reproduces the text.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "lbc = \"{}\"", escape_str(&self.version));
        let _ = writeln!(out, "target = \"{}\"", escape_str(&self.target));
        if !self.types.is_empty() {
            out.push('\n');
            for (index, info) in self.types.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "type !t{index} = \"{}\" size {} ref {}",
                    escape_str(&info.name),
                    info.size,
                    u8::from(info.is_ref)
                );
            }
        }
        if !self.data.is_empty() {
            out.push('\n');
            for (index, data) in self.data.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "data !c{index} = \"{}\" @ 0x{:x}",
                    escape_bytes(&data.bytes),
                    data.global_off
                );
            }
        }
        for func in &self.funcs {
            out.push('\n');
            self.write_func(&mut out, func);
        }
        out
    }

    /// Parses canonical `.lbc` text into a verified program.
    ///
    /// # Errors
    ///
    /// Returns a human-readable message if the version is unsupported or any
    /// header, type, data, function, or instruction is malformed. The
    /// reconstructed program is passed through [`Program::verify`].
    pub fn parse_text(text: &str) -> Result<Program, String> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let lines: Vec<&str> = text.lines().collect();
        let mut version = None;
        let mut target = None;
        let mut types: Vec<TypeInfo> = Vec::new();
        let mut type_ids: HashMap<String, TypeId> = HashMap::new();
        let mut data: Vec<Data> = Vec::new();
        let mut funcs: Vec<Func> = Vec::new();
        let mut func_refs: Vec<Vec<PendingFuncRef>> = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index].trim();
            if line.is_empty() {
                index += 1;
                continue;
            }
            let keyword = line.split_whitespace().next().unwrap_or("");
            match keyword {
                "lbc" => {
                    version = Some(parse_assignment(line, "lbc")?);
                    index += 1;
                }
                "target" => {
                    target = Some(parse_assignment(line, "target")?);
                    index += 1;
                }
                "type" => {
                    let (declared, info) = parse_type_line(line)?;
                    expect_next_index(declared, types.len(), "type")?;
                    type_ids.insert(info.name.clone(), declared);
                    types.push(info);
                    index += 1;
                }
                "data" => {
                    let (declared, entry) = parse_data_line(line)?;
                    expect_next_index(declared, data.len(), "data")?;
                    data.push(entry);
                    index += 1;
                }
                "func" => {
                    let (func, next, refs) = parse_func(&lines, index, &type_ids)?;
                    funcs.push(func);
                    func_refs.push(refs);
                    index = next;
                }
                other => return Err(format!("unexpected line `{other}`")),
            }
        }
        let version = version.ok_or_else(|| "missing `lbc` header".to_string())?;
        if version != LBC_VERSION {
            return Err(format!(
                "unsupported .lbc version `{version}` (expected `{LBC_VERSION}`)"
            ));
        }
        let target = target.ok_or_else(|| "missing `target` header".to_string())?;
        resolve_func_refs(&mut funcs, &func_refs)?;
        let program = Program {
            version,
            target,
            types,
            globals: crate::isa::derive_globals(&data)?,
            data,
            funcs,
        };
        program.verify()?;
        Ok(program)
    }

    /// Writes one function body (header, places, lines, and code).
    fn write_func(&self, out: &mut String, func: &Func) {
        let extern_kw = if func.is_extern { " extern" } else { "" };
        let _ = writeln!(
            out,
            "func @{} {} -> {} regs {} slots {}{extern_kw}",
            func.name,
            self.signature_ins(&func.ins),
            self.signature_outs(&func.outs),
            func.n_regs,
            func.slot_bytes
        );
        if !func.place_names.is_empty() {
            out.push_str("  places");
            for (name, off) in &func.place_names {
                let _ = write!(out, " %{name}={off}");
            }
            out.push('\n');
        }
        for line in &func.lines {
            let _ = writeln!(
                out,
                "  line {}:{}:{}:{}",
                line.pc, line.file, line.line, line.col
            );
        }
        let labels = func.labels();
        for (index, instr) in func.code.iter().enumerate() {
            let pc = u32::try_from(index).unwrap_or(u32::MAX);
            if labels.contains(&pc) {
                let _ = writeln!(out, "  $L{pc}:");
            }
            let _ = writeln!(out, "    {pc}  {}", self.instr_text(instr));
        }
    }

    /// Renders a function signature's input list: `()` or `(in t, ...)`.
    fn signature_ins(&self, types: &[TypeId]) -> String {
        if types.is_empty() {
            "()".to_string()
        } else {
            format!("(in {})", self.join_types(types))
        }
    }

    /// Renders a function signature's output list: `()` or `(t, ...)`.
    fn signature_outs(&self, types: &[TypeId]) -> String {
        format!("({})", self.join_types(types))
    }

    /// Joins type names with `, `.
    fn join_types(&self, types: &[TypeId]) -> String {
        types
            .iter()
            .map(|ty| self.type_name(*ty))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Returns a type's printable name, or a placeholder if the id is invalid.
    fn type_name(&self, ty: TypeId) -> String {
        self.types
            .get(ty as usize)
            .map_or_else(|| format!("<missing:{ty}>"), |info| info.name.clone())
    }

    /// Returns a function's name, or a placeholder if the id is invalid.
    fn func_name(&self, func: u32) -> String {
        self.funcs
            .get(func as usize)
            .map_or_else(|| format!("<missing:{func}>"), |entry| entry.name.clone())
    }

    /// Renders one instruction with resolved operands.
    fn instr_text(&self, instr: &Instr) -> String {
        match instr {
            Instr::Const { dst, imm, ty } => {
                format!("&{dst}: {} = const 0x{imm:x}", self.type_name(*ty))
            }
            Instr::ConstData { dst, data, ty } => {
                format!("&{dst}: {} = const.data !c{data}", self.type_name(*ty))
            }
            Instr::Copy { dst, src, ty } => format!(
                "{}: {} = copy {}",
                operand_text(*dst),
                self.type_name(*ty),
                operand_text(*src)
            ),
            Instr::CopyBytes { dst, src, size } => {
                format!("copy.bytes %{dst} <- %{src}, {size}")
            }
            Instr::Load { dst, src, ty } => {
                format!("&{dst}: {} = load %{src}", self.type_name(*ty))
            }
            Instr::Store { src, dst, ty } => format!(
                "store {} -> %{dst} : {}",
                operand_text(*src),
                self.type_name(*ty)
            ),
            Instr::Addr { dst, place } => format!("&{dst} = addr %{place}"),
            Instr::AddrField {
                dst,
                place,
                off,
                indirect,
            } => format!(
                "&{dst} = addr.field %{place} + {off}{}",
                indirect_text(*indirect)
            ),
            Instr::AddrFunc { dst, func } => {
                format!("&{dst} = addr.func @{}", self.func_name(*func))
            }
            Instr::Field {
                dst,
                place,
                off,
                ty,
                indirect,
            } => format!(
                "&{dst}: {} = field %{place} + {off}{}",
                self.type_name(*ty),
                indirect_text(*indirect)
            ),
            Instr::StoreField {
                src,
                place,
                off,
                ty,
                indirect,
            } => format!(
                "store.field {} -> %{place} + {off}{} : {}",
                operand_text(*src),
                indirect_text(*indirect),
                self.type_name(*ty)
            ),
            Instr::Arith {
                op,
                dst,
                lhs,
                rhs,
                ty,
            } => format!(
                "&{dst}: {} = {} &{lhs}, &{rhs}",
                self.type_name(*ty),
                op.as_str()
            ),
            Instr::Convert { op, dst, src, ty } => {
                format!("&{dst}: {} = {} &{src}", self.type_name(*ty), op.as_str())
            }
            Instr::Call { func, ins, outs } => format!(
                "call @{} (in {}) (out {})",
                self.func_name(*func),
                operand_list(ins),
                slot_list(outs)
            ),
            Instr::CallIndirect { callee, ins, outs } => format!(
                "call.indirect &{callee} (in {}) (out {})",
                operand_list(ins),
                slot_list(outs)
            ),
            Instr::CallBuiltin { builtin, ins, outs } => format!(
                "call.builtin @{} (in {}) (out {})",
                builtin.as_str(),
                operand_list(ins),
                slot_list(outs)
            ),
            Instr::Branch { target } => format!("branch $L{target}"),
            Instr::BranchIf { cond, then, els } => {
                format!("branch.if &{cond}, $L{then}, $L{els}")
            }
            Instr::Return => "return".to_string(),
            Instr::Panic { data } => format!("panic !c{data}"),
            Instr::Unreachable => "unreachable".to_string(),
        }
    }
}

/// A parsed instruction whose function references are not yet resolved.
enum ParsedInstr {
    /// A fully resolved instruction.
    Ready(Instr),
    /// A branch to a label.
    Branch(String),
    /// A conditional branch with two labels.
    BranchIf {
        cond: u32,
        then: String,
        els: String,
    },
    /// A direct call to a named function.
    Call {
        name: String,
        ins: Vec<Operand>,
        outs: Vec<u32>,
    },
    /// A `addr.func` to a named function.
    AddrFunc { dst: u32, name: String },
}

/// A function-name reference awaiting program-wide resolution.
enum PendingFuncRef {
    /// Call instruction index referencing a function name.
    Call(usize, String),
    /// `AddrFunc` instruction index referencing a function name.
    AddrFunc(usize, String),
}

/// Which branch field a label fixes up.
enum BranchSlot {
    Target,
    Then,
    Els,
}

/// A parsed function header.
struct FuncHeader {
    name: String,
    ins: Vec<TypeId>,
    outs: Vec<TypeId>,
    n_regs: u32,
    slot_bytes: u32,
    is_extern: bool,
}

/// Parses one function block, returning the function, the next line index, and
/// any unresolved function-name references.
fn parse_func(
    lines: &[&str],
    start: usize,
    type_ids: &HashMap<String, TypeId>,
) -> Result<(Func, usize, Vec<PendingFuncRef>), String> {
    let header = parse_func_header(lines[start].trim(), type_ids)?;
    let mut code: Vec<Instr> = Vec::new();
    let mut line_table: Vec<Line> = Vec::new();
    let mut place_names: Vec<(String, u32)> = Vec::new();
    let mut labels: HashMap<String, u32> = HashMap::new();
    let mut branch_fixups: Vec<(usize, BranchSlot, String)> = Vec::new();
    let mut func_refs: Vec<PendingFuncRef> = Vec::new();
    let mut pending_label: Option<String> = None;
    let mut index = start + 1;
    while index < lines.len() {
        let line = lines[index].trim();
        if line.is_empty() {
            index += 1;
            continue;
        }
        if line.starts_with("func ") {
            break;
        }
        if let Some(rest) = line.strip_prefix("places") {
            place_names = parse_places(rest)?;
            index += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix("line ") {
            line_table.push(parse_line_entry(rest)?);
            index += 1;
            continue;
        }
        if let Some(name) = line
            .strip_prefix('$')
            .and_then(|text| text.strip_suffix(':'))
        {
            pending_label = Some(name.to_string());
            index += 1;
            continue;
        }
        let (pc, text) = split_pc(line)?;
        let expected = u32::try_from(code.len()).map_err(|_| "code too large".to_string())?;
        if pc != expected {
            return Err(format!("expected pc {expected}, found {pc}"));
        }
        if let Some(label) = pending_label.take() {
            labels.insert(label, pc);
        }
        let instr_index = code.len();
        match parse_instr(text, type_ids)? {
            ParsedInstr::Ready(instr) => code.push(instr),
            ParsedInstr::Branch(label) => {
                code.push(Instr::Branch { target: 0 });
                branch_fixups.push((instr_index, BranchSlot::Target, label));
            }
            ParsedInstr::BranchIf { cond, then, els } => {
                code.push(Instr::BranchIf {
                    cond,
                    then: 0,
                    els: 0,
                });
                branch_fixups.push((instr_index, BranchSlot::Then, then));
                branch_fixups.push((instr_index, BranchSlot::Els, els));
            }
            ParsedInstr::Call { name, ins, outs } => {
                code.push(Instr::Call { func: 0, ins, outs });
                func_refs.push(PendingFuncRef::Call(instr_index, name));
            }
            ParsedInstr::AddrFunc { dst, name } => {
                code.push(Instr::AddrFunc { dst, func: 0 });
                func_refs.push(PendingFuncRef::AddrFunc(instr_index, name));
            }
        }
        index += 1;
    }
    resolve_labels(&mut code, &branch_fixups, &labels)?;
    let func = Func {
        name: header.name,
        is_extern: header.is_extern,
        ins: header.ins,
        outs: header.outs,
        n_regs: header.n_regs,
        slot_bytes: header.slot_bytes,
        code,
        lines: line_table,
        place_names,
    };
    Ok((func, index, func_refs))
}

/// Patches symbolic branch targets to pcs.
fn resolve_labels(
    code: &mut [Instr],
    fixups: &[(usize, BranchSlot, String)],
    labels: &HashMap<String, u32>,
) -> Result<(), String> {
    for (index, slot, label) in fixups {
        let pc = labels
            .get(label)
            .copied()
            .ok_or_else(|| format!("unknown label `${label}`"))?;
        match (&mut code[*index], slot) {
            (Instr::Branch { target }, BranchSlot::Target) => *target = pc,
            (Instr::BranchIf { then, .. }, BranchSlot::Then) => *then = pc,
            (Instr::BranchIf { els, .. }, BranchSlot::Els) => *els = pc,
            _ => return Err("internal: branch fixup mismatch".to_string()),
        }
    }
    Ok(())
}

/// Resolves function-name references to function indices.
fn resolve_func_refs(funcs: &mut [Func], func_refs: &[Vec<PendingFuncRef>]) -> Result<(), String> {
    let ids: HashMap<String, u32> = funcs
        .iter()
        .enumerate()
        .map(|(index, func)| (func.name.clone(), u32::try_from(index).unwrap_or(u32::MAX)))
        .collect();
    for (func_index, refs) in func_refs.iter().enumerate() {
        for reference in refs {
            let (instr_index, name, is_addr_func) = match reference {
                PendingFuncRef::Call(index, name) => (*index, name, false),
                PendingFuncRef::AddrFunc(index, name) => (*index, name, true),
            };
            let target = ids
                .get(name)
                .copied()
                .ok_or_else(|| format!("unknown function `@{name}`"))?;
            match (&mut funcs[func_index].code[instr_index], is_addr_func) {
                (Instr::Call { func, .. }, false) | (Instr::AddrFunc { func, .. }, true) => {
                    *func = target;
                }
                _ => return Err("internal: function reference mismatch".to_string()),
            }
        }
    }
    Ok(())
}

/// Parses a `key = "value"` header assignment.
fn parse_assignment(line: &str, key: &str) -> Result<String, String> {
    let rest = line
        .strip_prefix(key)
        .ok_or_else(|| format!("expected `{key}`"))?
        .trim_start();
    let rest = rest
        .strip_prefix('=')
        .ok_or_else(|| format!("expected `{key} = ...`"))?
        .trim_start();
    let (value, rest) = parse_quoted_str(rest)?;
    if !rest.trim().is_empty() {
        return Err(format!("trailing text after `{key}` header"));
    }
    Ok(value)
}

/// Parses a `type` line, returning its declared index and entry.
fn parse_type_line(line: &str) -> Result<(u32, TypeInfo), String> {
    let rest = line
        .strip_prefix("type")
        .ok_or_else(|| "expected `type`".to_string())?
        .trim_start();
    let rest = rest
        .strip_prefix('!')
        .and_then(|text| text.strip_prefix('t'))
        .ok_or_else(|| "expected `type !tN`".to_string())?;
    let (index_text, rest) = rest
        .split_once(char::is_whitespace)
        .ok_or_else(|| "expected a type index".to_string())?;
    let index = parse_u32(Some(index_text), "type index")?;
    let rest = rest
        .trim_start()
        .strip_prefix('=')
        .ok_or_else(|| "expected `=`".to_string())?
        .trim_start();
    let (name, rest) = parse_quoted_str(rest)?;
    let mut tokens = rest.split_whitespace();
    if tokens.next() != Some("size") {
        return Err("expected `size`".to_string());
    }
    let size = parse_u32(tokens.next(), "type size")?;
    if tokens.next() != Some("ref") {
        return Err("expected `ref`".to_string());
    }
    let is_ref = match tokens.next() {
        Some("0") => false,
        Some("1") => true,
        _ => return Err("expected `ref 0` or `ref 1`".to_string()),
    };
    if tokens.next().is_some() {
        return Err("trailing tokens on type line".to_string());
    }
    Ok((index, TypeInfo { name, size, is_ref }))
}

/// Parses a `data` line, returning its declared index and entry.
fn parse_data_line(line: &str) -> Result<(u32, Data), String> {
    let rest = line
        .strip_prefix("data")
        .ok_or_else(|| "expected `data`".to_string())?
        .trim_start();
    let rest = rest
        .strip_prefix('!')
        .and_then(|text| text.strip_prefix('c'))
        .ok_or_else(|| "expected `data !cN`".to_string())?;
    let (index_text, rest) = rest
        .split_once(char::is_whitespace)
        .ok_or_else(|| "expected a data index".to_string())?;
    let index = parse_u32(Some(index_text), "data index")?;
    let rest = rest
        .trim_start()
        .strip_prefix('=')
        .ok_or_else(|| "expected `=`".to_string())?
        .trim_start();
    let (bytes, rest) = parse_quoted_bytes(rest)?;
    let mut tokens = rest.split_whitespace();
    if tokens.next() != Some("@") {
        return Err("expected `@`".to_string());
    }
    let off_text = tokens
        .next()
        .ok_or_else(|| "expected a data offset".to_string())?;
    let global_off = parse_hex_u32(off_text)?;
    if tokens.next().is_some() {
        return Err("trailing tokens on data line".to_string());
    }
    Ok((index, Data { bytes, global_off }))
}

/// Parses a `func` header.
fn parse_func_header(line: &str, type_ids: &HashMap<String, TypeId>) -> Result<FuncHeader, String> {
    let rest = line
        .strip_prefix("func @")
        .ok_or_else(|| "expected `func @name`".to_string())?;
    let (name, rest) = rest
        .split_once(' ')
        .ok_or_else(|| "expected a space after the function name".to_string())?;
    let (ins_text, rest) = parse_paren_group(rest.trim_start())?;
    let rest = rest
        .trim_start()
        .strip_prefix("->")
        .ok_or_else(|| "expected `->`".to_string())?
        .trim_start();
    let (outs_text, rest) = parse_paren_group(rest)?;
    let ins = parse_signature_types(ins_text, true, type_ids)?;
    let outs = parse_signature_types(outs_text, false, type_ids)?;
    let mut tokens = rest.split_whitespace();
    if tokens.next() != Some("regs") {
        return Err("expected `regs`".to_string());
    }
    let n_regs = parse_u32(tokens.next(), "regs")?;
    if tokens.next() != Some("slots") {
        return Err("expected `slots`".to_string());
    }
    let slot_bytes = parse_u32(tokens.next(), "slots")?;
    let is_extern = match tokens.next() {
        None => false,
        Some("extern") => true,
        Some(other) => return Err(format!("unexpected `{other}` in function header")),
    };
    if tokens.next().is_some() {
        return Err("trailing tokens in function header".to_string());
    }
    Ok(FuncHeader {
        name: name.to_string(),
        ins,
        outs,
        n_regs,
        slot_bytes,
        is_extern,
    })
}

/// Parses a parenthesised type list inside a function signature.
fn parse_signature_types(
    text: &str,
    is_ins: bool,
    type_ids: &HashMap<String, TypeId>,
) -> Result<Vec<TypeId>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let text = if is_ins {
        text.strip_prefix("in")
            .ok_or_else(|| "expected `in` in signature".to_string())?
            .trim()
    } else {
        text
    };
    text.split(',')
        .map(|name| {
            let name = name.trim();
            type_ids
                .get(name)
                .copied()
                .ok_or_else(|| format!("unknown type `{name}`"))
        })
        .collect()
}

/// Parses a `places` line body.
fn parse_places(text: &str) -> Result<Vec<(String, u32)>, String> {
    text.split_whitespace()
        .map(|token| {
            let (name, off) = token
                .split_once('=')
                .ok_or_else(|| format!("malformed place `{token}`"))?;
            let name = name
                .strip_prefix('%')
                .ok_or_else(|| format!("expected `%name` in place `{token}`"))?;
            Ok((name.to_string(), parse_u32(Some(off), "place offset")?))
        })
        .collect()
}

/// Parses a `line` line body.
fn parse_line_entry(text: &str) -> Result<Line, String> {
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() < 3 {
        return Err("malformed line entry".to_string());
    }
    let last = parts.len() - 1;
    Ok(Line {
        pc: parse_u32(Some(parts[0]), "line pc")?,
        file: parts[1..last - 1].join(":"),
        line: parse_u32(Some(parts[last - 1]), "line line")?,
        col: parse_u32(Some(parts[last]), "line col")?,
    })
}

/// Parses one instruction's text.
fn parse_instr(text: &str, type_ids: &HashMap<String, TypeId>) -> Result<ParsedInstr, String> {
    if text.starts_with('&') || text.starts_with('%') {
        return parse_definition(text, type_ids);
    }
    let (head, rest) = split_head(text);
    match head {
        "copy.bytes" => parse_copy_bytes(rest),
        "store" => parse_store(rest, type_ids, false),
        "store.field" => parse_store(rest, type_ids, true),
        "call" => parse_call(rest),
        "call.indirect" => parse_call_indirect(rest),
        "call.builtin" => parse_call_builtin(rest),
        "branch" => Ok(ParsedInstr::Branch(parse_label_ref(rest)?)),
        "branch.if" => parse_branch_if(rest),
        "return" => Ok(ParsedInstr::Ready(Instr::Return)),
        "panic" => Ok(ParsedInstr::Ready(Instr::Panic {
            data: parse_data_ref(rest)?,
        })),
        "unreachable" => Ok(ParsedInstr::Ready(Instr::Unreachable)),
        other => Err(format!("unknown opcode `{other}`")),
    }
}

/// Parses an instruction that begins with a `&reg`/`%slot` destination.
fn parse_definition(text: &str, type_ids: &HashMap<String, TypeId>) -> Result<ParsedInstr, String> {
    let (lhs, rhs) = text
        .split_once(" = ")
        .ok_or_else(|| "expected ` = `".to_string())?;
    if let Some((dst_text, ty_text)) = lhs.split_once(": ") {
        let dst = parse_operand(dst_text.trim())?;
        let ty = parse_type(ty_text.trim(), type_ids)?;
        parse_typed_definition(dst, ty, rhs)
    } else {
        parse_addr_definition(lhs.trim(), rhs)
    }
}

/// Parses a typed definition (`&d: ty = op ...`).
fn parse_typed_definition(dst: Operand, ty: TypeId, rhs: &str) -> Result<ParsedInstr, String> {
    let (head, args) = split_head(rhs);
    match head {
        "const" => Ok(ParsedInstr::Ready(Instr::Const {
            dst: as_reg(dst)?,
            imm: parse_hex_u64(args.trim())?,
            ty,
        })),
        "const.data" => Ok(ParsedInstr::Ready(Instr::ConstData {
            dst: as_reg(dst)?,
            data: parse_data_ref(args)?,
            ty,
        })),
        "copy" => Ok(ParsedInstr::Ready(Instr::Copy {
            dst,
            src: parse_operand(args.trim())?,
            ty,
        })),
        "load" => Ok(ParsedInstr::Ready(Instr::Load {
            dst: as_reg(dst)?,
            src: parse_slot(args.trim())?,
            ty,
        })),
        "field" => {
            let (place, off, indirect) = parse_place_off(args)?;
            Ok(ParsedInstr::Ready(Instr::Field {
                dst: as_reg(dst)?,
                place,
                off,
                ty,
                indirect,
            }))
        }
        other => {
            if let Some(op) = ArithOp::parse(other) {
                let (lhs, rhs) = parse_reg_pair(args)?;
                return Ok(ParsedInstr::Ready(Instr::Arith {
                    op,
                    dst: as_reg(dst)?,
                    lhs,
                    rhs,
                    ty,
                }));
            }
            if let Some(op) = ConvertOp::parse(other) {
                return Ok(ParsedInstr::Ready(Instr::Convert {
                    op,
                    dst: as_reg(dst)?,
                    src: parse_reg(args.trim())?,
                    ty,
                }));
            }
            Err(format!("unknown typed opcode `{other}`"))
        }
    }
}

/// Parses an address definition (`&d = addr ...`).
fn parse_addr_definition(lhs: &str, rhs: &str) -> Result<ParsedInstr, String> {
    let dst = parse_reg(lhs)?;
    let (head, args) = split_head(rhs);
    match head {
        "addr" => Ok(ParsedInstr::Ready(Instr::Addr {
            dst,
            place: parse_slot(args.trim())?,
        })),
        "addr.field" => {
            let (place, off, indirect) = parse_place_off(args)?;
            Ok(ParsedInstr::Ready(Instr::AddrField {
                dst,
                place,
                off,
                indirect,
            }))
        }
        "addr.func" => Ok(ParsedInstr::AddrFunc {
            dst,
            name: parse_func_ref(args)?,
        }),
        other => Err(format!("unknown address opcode `{other}`")),
    }
}

/// Parses a `store`/`store.field` instruction.
fn parse_store(
    rest: &str,
    type_ids: &HashMap<String, TypeId>,
    is_field: bool,
) -> Result<ParsedInstr, String> {
    let (left, ty_text) = rest
        .rsplit_once(" : ")
        .ok_or_else(|| "expected ` : type`".to_string())?;
    let ty = parse_type(ty_text.trim(), type_ids)?;
    let (src_text, dst_text) = left
        .split_once(" -> ")
        .ok_or_else(|| "expected ` -> `".to_string())?;
    let src = parse_operand(src_text.trim())?;
    if is_field {
        let (place, off, indirect) = parse_place_off(dst_text)?;
        Ok(ParsedInstr::Ready(Instr::StoreField {
            src,
            place,
            off,
            ty,
            indirect,
        }))
    } else {
        Ok(ParsedInstr::Ready(Instr::Store {
            src,
            dst: parse_slot(dst_text.trim())?,
            ty,
        }))
    }
}

/// Parses a `copy.bytes` instruction.
fn parse_copy_bytes(rest: &str) -> Result<ParsedInstr, String> {
    let (left, size_text) = rest
        .rsplit_once(',')
        .ok_or_else(|| "expected `, size`".to_string())?;
    let (dst_text, src_text) = left
        .split_once(" <- ")
        .ok_or_else(|| "expected ` <- `".to_string())?;
    Ok(ParsedInstr::Ready(Instr::CopyBytes {
        dst: parse_slot(dst_text.trim())?,
        src: parse_slot(src_text.trim())?,
        size: parse_u32(Some(size_text.trim()), "size")?,
    }))
}

/// Parses a direct `call` instruction.
fn parse_call(rest: &str) -> Result<ParsedInstr, String> {
    let (target, args) = split_head(rest);
    let name = target
        .strip_prefix('@')
        .ok_or_else(|| "expected `@name`".to_string())?;
    let (ins, outs) = parse_call_args(args)?;
    Ok(ParsedInstr::Call {
        name: name.to_string(),
        ins,
        outs,
    })
}

/// Parses a `call.indirect` instruction.
fn parse_call_indirect(rest: &str) -> Result<ParsedInstr, String> {
    let (callee_text, args) = split_head(rest);
    let (ins, outs) = parse_call_args(args)?;
    Ok(ParsedInstr::Ready(Instr::CallIndirect {
        callee: parse_reg(callee_text)?,
        ins,
        outs,
    }))
}

/// Parses a `call.builtin` instruction.
fn parse_call_builtin(rest: &str) -> Result<ParsedInstr, String> {
    let (target, args) = split_head(rest);
    let name = target
        .strip_prefix('@')
        .ok_or_else(|| "expected `@builtin`".to_string())?;
    let builtin = Builtin::parse(name).ok_or_else(|| format!("unknown builtin `{name}`"))?;
    let (ins, outs) = parse_call_args(args)?;
    Ok(ParsedInstr::Ready(Instr::CallBuiltin {
        builtin,
        ins,
        outs,
    }))
}

/// Parses `(in ...) (out ...)` call argument lists.
fn parse_call_args(text: &str) -> Result<(Vec<Operand>, Vec<u32>), String> {
    let (ins_text, rest) = parse_paren_group(text.trim_start())?;
    let (outs_text, rest) = parse_paren_group(rest.trim_start())?;
    if !rest.trim().is_empty() {
        return Err("trailing text after call arguments".to_string());
    }
    let ins_body = ins_text
        .trim()
        .strip_prefix("in")
        .ok_or_else(|| "expected `(in ...)`".to_string())?;
    let outs_body = outs_text
        .trim()
        .strip_prefix("out")
        .ok_or_else(|| "expected `(out ...)`".to_string())?;
    Ok((parse_operand_list(ins_body)?, parse_slot_list(outs_body)?))
}

/// Parses a `branch.if` instruction.
fn parse_branch_if(rest: &str) -> Result<ParsedInstr, String> {
    let (cond_text, rest) = rest
        .split_once(',')
        .ok_or_else(|| "expected a condition register".to_string())?;
    let (then_text, els_text) = rest
        .split_once(',')
        .ok_or_else(|| "expected two branch labels".to_string())?;
    Ok(ParsedInstr::BranchIf {
        cond: parse_reg(cond_text.trim())?,
        then: parse_label_ref(then_text)?,
        els: parse_label_ref(els_text)?,
    })
}

/// Parses a comma-separated operand list; empty input yields no operands.
fn parse_operand_list(text: &str) -> Result<Vec<Operand>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|item| parse_operand(item.trim()))
        .collect()
}

/// Parses a comma-separated slot list; empty input yields no slots.
fn parse_slot_list(text: &str) -> Result<Vec<u32>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|item| parse_slot(item.trim()))
        .collect()
}

/// Parses a `%slot + offset` suffix.
fn parse_place_off(text: &str) -> Result<(u32, u32, bool), String> {
    let mut tokens = text.split_whitespace();
    let place = parse_slot(
        tokens
            .next()
            .ok_or_else(|| "expected a place".to_string())?,
    )?;
    if tokens.next() != Some("+") {
        return Err("expected `+`".to_string());
    }
    let off = parse_u32(tokens.next(), "offset")?;
    let indirect = match tokens.next() {
        Some("direct") => false,
        Some("indirect") => true,
        _ => return Err("expected `direct` or `indirect`".to_string()),
    };
    if tokens.next().is_some() {
        return Err("trailing tokens after field base".to_string());
    }
    Ok((place, off, indirect))
}

/// Parses a pair of comma-separated registers.
fn parse_reg_pair(text: &str) -> Result<(u32, u32), String> {
    let (left, right) = text
        .split_once(',')
        .ok_or_else(|| "expected two registers".to_string())?;
    Ok((parse_reg(left.trim())?, parse_reg(right.trim())?))
}

/// Parses an operand (`&reg` or `%slot`).
fn parse_operand(text: &str) -> Result<Operand, String> {
    let text = text.trim();
    if let Some(reg) = text.strip_prefix('&') {
        Ok(Operand::Reg(parse_u32(Some(reg), "register")?))
    } else if let Some(slot) = text.strip_prefix('%') {
        Ok(Operand::Slot(parse_u32(Some(slot), "slot")?))
    } else {
        Err(format!("expected `&reg` or `%slot`, got `{text}`"))
    }
}

/// Parses a `&reg` token.
fn parse_reg(text: &str) -> Result<u32, String> {
    let text = text.trim();
    let digits = text
        .strip_prefix('&')
        .ok_or_else(|| format!("expected `&reg`, got `{text}`"))?;
    parse_u32(Some(digits), "register")
}

/// Parses a `%slot` token.
fn parse_slot(text: &str) -> Result<u32, String> {
    let text = text.trim();
    let digits = text
        .strip_prefix('%')
        .ok_or_else(|| format!("expected `%slot`, got `{text}`"))?;
    parse_u32(Some(digits), "slot")
}

/// Extracts the register from a register-destination operand.
fn as_reg(operand: Operand) -> Result<u32, String> {
    match operand {
        Operand::Reg(reg) => Ok(reg),
        Operand::Slot(_) => Err("expected a register destination".to_string()),
    }
}

/// Parses a `$label` reference (without the sigil).
fn parse_label_ref(text: &str) -> Result<String, String> {
    let text = text.trim();
    text.strip_prefix('$')
        .map(str::to_string)
        .ok_or_else(|| format!("expected `$label`, got `{text}`"))
}

/// Parses an `@func` reference (without the sigil).
fn parse_func_ref(text: &str) -> Result<String, String> {
    let text = text.trim();
    text.strip_prefix('@')
        .map(str::to_string)
        .ok_or_else(|| format!("expected `@func`, got `{text}`"))
}

/// Parses a `!cN` data reference.
fn parse_data_ref(text: &str) -> Result<u32, String> {
    let text = text.trim();
    let digits = text
        .strip_prefix("!c")
        .ok_or_else(|| format!("expected `!cN`, got `{text}`"))?;
    parse_u32(Some(digits), "data index")
}

/// Resolves a type name to its id.
fn parse_type(text: &str, type_ids: &HashMap<String, TypeId>) -> Result<TypeId, String> {
    let text = text.trim();
    type_ids
        .get(text)
        .copied()
        .ok_or_else(|| format!("unknown type `{text}`"))
}

/// Splits a `<pc>  <instruction>` line.
fn split_pc(line: &str) -> Result<(u32, &str), String> {
    let (pc_text, rest) = line
        .split_once(char::is_whitespace)
        .ok_or_else(|| "expected `<pc>  <instruction>`".to_string())?;
    Ok((parse_u32(Some(pc_text), "pc")?, rest.trim_start()))
}

/// Splits off the first whitespace-delimited token.
fn split_head(text: &str) -> (&str, &str) {
    text.split_once(char::is_whitespace)
        .map_or((text, ""), |(head, rest)| (head, rest.trim_start()))
}

/// Parses a parenthesised group, returning the body and the remainder.
fn parse_paren_group(text: &str) -> Result<(&str, &str), String> {
    let text = text.trim_start();
    let body = text
        .strip_prefix('(')
        .ok_or_else(|| "expected `(`".to_string())?;
    let end = body
        .find(')')
        .ok_or_else(|| "unterminated `(`".to_string())?;
    Ok((&body[..end], &body[end + 1..]))
}

/// Parses an optional `u32`.
fn parse_u32(value: Option<&str>, what: &str) -> Result<u32, String> {
    value
        .ok_or_else(|| format!("missing {what}"))?
        .parse()
        .map_err(|_| format!("invalid {what}"))
}

/// Parses a `0x`-prefixed `u32`.
fn parse_hex_u32(text: &str) -> Result<u32, String> {
    let digits = text
        .strip_prefix("0x")
        .ok_or_else(|| format!("expected hex `0x...`, got `{text}`"))?;
    u32::from_str_radix(digits, 16).map_err(|_| format!("invalid hex `{text}`"))
}

/// Parses a `0x`-prefixed `u64`.
fn parse_hex_u64(text: &str) -> Result<u64, String> {
    let digits = text
        .strip_prefix("0x")
        .ok_or_else(|| format!("expected hex `0x...`, got `{text}`"))?;
    u64::from_str_radix(digits, 16).map_err(|_| format!("invalid hex `{text}`"))
}

/// Validates that a declared index is the next sequential index.
fn expect_next_index(declared: u32, len: usize, what: &str) -> Result<(), String> {
    let expected = u32::try_from(len).map_err(|_| format!("too many {what}s"))?;
    if declared == expected {
        Ok(())
    } else {
        Err(format!(
            "{what} index {declared} out of order (expected {expected})"
        ))
    }
}

/// Renders a field instruction's base-selection token.
fn indirect_text(indirect: bool) -> &'static str {
    if indirect {
        " indirect"
    } else {
        " direct"
    }
}

/// Renders an operand.
fn operand_text(operand: Operand) -> String {
    match operand {
        Operand::Reg(reg) => format!("&{reg}"),
        Operand::Slot(slot) => format!("%{slot}"),
    }
}

/// Renders a `, `-joined operand list.
fn operand_list(ins: &[Operand]) -> String {
    ins.iter()
        .map(|operand| operand_text(*operand))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Renders a `, `-joined slot list.
fn slot_list(outs: &[u32]) -> String {
    outs.iter()
        .map(|slot| format!("%{slot}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Escapes a string for a quoted `.lbc` field.
fn escape_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            other => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(other));
            }
        }
    }
    out
}

/// Escapes raw bytes for a quoted `.lbc` field.
fn escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &byte in bytes {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(char::from(byte)),
            other => {
                let _ = write!(out, "\\x{other:02x}");
            }
        }
    }
    out
}

/// Parses a quoted string, returning the value and the remainder.
fn parse_quoted_str(text: &str) -> Result<(String, &str), String> {
    let body = text
        .trim_start()
        .strip_prefix('"')
        .ok_or_else(|| "expected `\"`".to_string())?;
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '"' => return Ok((out, &body[index + 1..])),
            '\\' => {
                let (_, escaped) = chars
                    .next()
                    .ok_or_else(|| "unterminated escape".to_string())?;
                match escaped {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    '\\' => out.push('\\'),
                    '"' => out.push('"'),
                    'u' => out.push(parse_unicode_escape(&mut chars)?),
                    other => return Err(format!("unknown escape `\\{other}`")),
                }
            }
            _ => out.push(ch),
        }
    }
    Err("unterminated string literal".to_string())
}

/// Parses a quoted byte string, returning the bytes and the remainder.
fn parse_quoted_bytes(text: &str) -> Result<(Vec<u8>, &str), String> {
    let body = text
        .trim_start()
        .strip_prefix('"')
        .ok_or_else(|| "expected `\"`".to_string())?;
    let mut out = Vec::new();
    let mut chars = body.char_indices();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '"' => return Ok((out, &body[index + 1..])),
            '\\' => {
                let (_, escaped) = chars
                    .next()
                    .ok_or_else(|| "unterminated escape".to_string())?;
                match escaped {
                    'n' => out.push(b'\n'),
                    'r' => out.push(b'\r'),
                    't' => out.push(b'\t'),
                    '\\' => out.push(b'\\'),
                    '"' => out.push(b'"'),
                    'x' => out.push(parse_byte_escape(&mut chars)?),
                    other => return Err(format!("unknown byte escape `\\{other}`")),
                }
            }
            _ => {
                let mut buf = [0_u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    Err("unterminated byte string literal".to_string())
}

/// Parses a `\u{...}` escape body.
fn parse_unicode_escape(chars: &mut std::str::CharIndices<'_>) -> Result<char, String> {
    let mut hex = String::new();
    let mut opened = false;
    let mut closed = false;
    for (_, ch) in chars.by_ref() {
        match ch {
            '{' if !opened => opened = true,
            '}' if opened => {
                closed = true;
                break;
            }
            c if opened && c.is_ascii_hexdigit() => hex.push(c),
            _ => return Err("malformed `\\u{...}` escape".to_string()),
        }
    }
    if !opened || !closed || hex.is_empty() {
        return Err("malformed `\\u{...}` escape".to_string());
    }
    let code =
        u32::from_str_radix(&hex, 16).map_err(|_| "malformed `\\u{...}` escape".to_string())?;
    char::from_u32(code).ok_or_else(|| "invalid unicode scalar value".to_string())
}

/// Parses a `\xNN` escape body.
fn parse_byte_escape(chars: &mut std::str::CharIndices<'_>) -> Result<u8, String> {
    let high = chars
        .next()
        .ok_or_else(|| "truncated `\\x` escape".to_string())?
        .1;
    let low = chars
        .next()
        .ok_or_else(|| "truncated `\\x` escape".to_string())?
        .1;
    let digits: String = [high, low].iter().collect();
    u8::from_str_radix(&digits, 16).map_err(|_| "malformed `\\xNN` escape".to_string())
}

#[cfg(test)]
mod tests {
    use crate::isa::{sample_program, Data, Func, Line, TypeInfo};

    use super::*;

    #[test]
    fn text_round_trips() {
        let program = sample_program(vec![
            Instr::Const {
                dst: 0,
                imm: 0x2a,
                ty: 0,
            },
            Instr::Return,
        ]);
        let text = program.to_text();
        let parsed = Program::parse_text(&text).expect("parse");
        assert_eq!(parsed.to_text(), text);
    }

    #[test]
    fn parse_rejects_unknown_version() {
        let text = "lbc = \"9.9\"\ntarget = \"lilacvm-unknown-none\"\n";
        assert!(Program::parse_text(text).is_err());
    }

    #[test]
    fn text_round_trips_two_data_entries() {
        let mut program = sample_program(vec![Instr::Return]);
        program.data = vec![
            Data {
                bytes: b"hi".to_vec(),
                global_off: 0,
            },
            Data {
                bytes: b"5.25".to_vec(),
                global_off: 2,
            },
        ];
        program.globals = b"hi5.25".to_vec();
        let text = program.to_text();
        let parsed = Program::parse_text(&text).expect("parse");
        assert_eq!(parsed, program);
    }

    #[test]
    fn parse_rejects_enormous_data_offset() {
        let text =
            "lbc = \"0.1\"\ntarget = \"lilacvm-unknown-none\"\n\ndata !c0 = \"hi\" @ 0xffffffff\n";
        assert!(Program::parse_text(text).is_err());
    }

    /// A program with both `indirect` and direct field instructions must
    /// survive `parse_text(to_text(p)) == p` unchanged.
    #[test]
    fn text_round_trips_field_indirection() {
        let program = crate::compile::compile_src(
            "main: function () nothing {\n    part: @byte_buffer = \"x\"\n    msg: @format_args = $\"a{part}b\"\n    n: @bit_native = @format_args_count(msg)\n    first: @byte_buffer = @format_args_piece(msg, 0x0)\n}\n",
        );
        let saw_indirect = program.funcs.iter().flat_map(|f| &f.code).any(|i| {
            matches!(
                i,
                Instr::Field { indirect: true, .. }
                    | Instr::AddrField { indirect: true, .. }
                    | Instr::StoreField { indirect: true, .. }
            )
        });
        let saw_direct = program.funcs.iter().flat_map(|f| &f.code).any(|i| {
            matches!(
                i,
                Instr::Field {
                    indirect: false,
                    ..
                }
            )
        });
        assert!(saw_indirect && saw_direct, "test program lacks both flags");
        let text = program.to_text();
        let parsed = Program::parse_text(&text).expect("parse");
        assert_eq!(parsed, program);
        assert_eq!(parsed.to_text(), text);
    }

    #[test]
    fn text_round_trips_rich_program() {
        let program = Program {
            version: LBC_VERSION.to_string(),
            target: crate::isa::VM_TARGET.to_string(),
            types: vec![TypeInfo {
                name: "#bit_native".to_string(),
                size: 8,
                is_ref: false,
            }],
            data: vec![Data {
                bytes: b"hi".to_vec(),
                global_off: 0,
            }],
            globals: b"hi".to_vec(),
            funcs: vec![
                Func {
                    name: "main".to_string(),
                    is_extern: false,
                    ins: Vec::new(),
                    outs: Vec::new(),
                    n_regs: 4,
                    slot_bytes: 16,
                    code: vec![
                        Instr::Const {
                            dst: 0,
                            imm: 0x2a,
                            ty: 0,
                        },
                        Instr::ConstData {
                            dst: 1,
                            data: 0,
                            ty: 0,
                        },
                        Instr::Arith {
                            op: ArithOp::IAdd,
                            dst: 2,
                            lhs: 0,
                            rhs: 1,
                            ty: 0,
                        },
                        Instr::Call {
                            func: 1,
                            ins: vec![Operand::Reg(2)],
                            outs: vec![3],
                        },
                        Instr::BranchIf {
                            cond: 0,
                            then: 5,
                            els: 6,
                        },
                        Instr::Return,
                        Instr::Return,
                    ],
                    lines: vec![Line {
                        pc: 0,
                        file: "t.lilac".to_string(),
                        line: 1,
                        col: 1,
                    }],
                    place_names: vec![("x".to_string(), 0)],
                },
                Func {
                    name: "helper".to_string(),
                    is_extern: false,
                    ins: vec![0],
                    outs: vec![0],
                    n_regs: 1,
                    slot_bytes: 8,
                    code: vec![Instr::Return],
                    lines: Vec::new(),
                    place_names: Vec::new(),
                },
            ],
        };
        let text = program.to_text();
        let parsed = Program::parse_text(&text).expect("parse");
        assert_eq!(parsed.to_text(), text);
        assert_eq!(parsed, program);
    }
}
