//! Interactive debugger REPL over a [`Machine`].
//!
//! [`Debugger`] drives a bytecode [`Machine`] from a line-oriented command
//! stream (spec §8). It supports `break`/`delete`/`clear`, `run`/`continue`,
//! `step`/`stepi`/`next`/`finish`, `print`, `info`, `backtrace`, `disasm`,
//! `list`, and `quit`. Source text is loaded from the line table's recorded
//! path (or an explicit override); when it is unavailable, `list` falls back to
//! disassembly and stepping still works at instruction granularity.
//!
//! The REPL reports the source line of the next instruction to execute
//! (computed from the current frame's program counter) for stops, while
//! [`Machine::current_line`]/[`Machine::backtrace`] report the instruction a
//! frame is currently executing; see [`crate::exec`] for that distinction.

use std::io::{BufRead, Write};

use crate::decode::decode;
use crate::exec::{Machine, Step, VmError};
use crate::isa::{Func, Line};

/// A requested stop location.
enum Breakpoint {
    /// Stop when the function at `index` is entered.
    Func { index: u32, name: String },
    /// Stop on `line` of `file` (matched by exact name or path suffix).
    Line { file: String, line: u32 },
}

/// Why a resume returned control to the REPL.
enum Stop {
    /// A breakpoint matched.
    Breakpoint,
    /// The program returned or panicked.
    Halted,
}

/// An interactive debugger over one [`Machine`].
pub struct Debugger<'a> {
    machine: Machine<'a>,
    breakpoints: Vec<Breakpoint>,
    /// Explicit source text override (from `--source`).
    source: Option<String>,
    /// Set once the program has returned or panicked.
    halted: bool,
    /// The program's exit code once halted.
    exit: Option<i32>,
    /// Set when a stop is returned to the REPL, so the next `run`/`continue`
    /// executes one instruction before re-arming breakpoints instead of
    /// immediately re-triggering the breakpoint the debugger is sitting on.
    ///
    /// Cleared by the first [`Debugger::advance`]; it must not be keyed on a
    /// stale location or breakpoints silently stop working (a loop that
    /// branches back to the stop pc, or a recursive re-entry at `pc == 0`).
    skip_hit: bool,
}

impl<'a> Debugger<'a> {
    /// Creates a debugger for `machine`.
    ///
    /// `source` is optional source text overriding the line table's recorded
    /// path; pass `None` to let the debugger read the recorded file.
    #[must_use]
    pub fn new(machine: Machine<'a>, source: Option<String>) -> Self {
        Self {
            machine,
            breakpoints: Vec::new(),
            source,
            halted: false,
            exit: None,
            skip_hit: false,
        }
    }

    /// Adds a breakpoint at `"<fn>"` or `"<file>:<line>"`.
    ///
    /// # Errors
    ///
    /// Returns a message when `loc` is empty, malformed, or names an unknown
    /// function.
    pub fn set_breakpoint(&mut self, loc: &str) -> Result<(), String> {
        let loc = loc.trim();
        if loc.is_empty() {
            return Err("break needs a function name or file:line".to_string());
        }
        if let Some((file, line)) = parse_line_loc(loc) {
            if line == 0 {
                return Err("line numbers are 1-based".to_string());
            }
            self.breakpoints.push(Breakpoint::Line { file, line });
            return Ok(());
        }
        let index = self
            .machine
            .program()
            .funcs
            .iter()
            .position(|f| f.name == loc)
            .ok_or_else(|| format!("no function `{loc}`"))?;
        let index = u32::try_from(index).map_err(|_| "too many functions".to_string())?;
        self.breakpoints.push(Breakpoint::Func {
            index,
            name: loc.to_string(),
        });
        Ok(())
    }

    /// Runs the command loop until `quit` or end of input.
    ///
    /// At end of input the program is resumed to completion, so a
    /// non-interactive invocation with debug flags behaves like a plain run.
    ///
    /// # Errors
    ///
    /// Propagates any [`VmError`] raised while stepping the machine.
    pub fn run_repl(
        &mut self,
        input: impl BufRead,
        output: &mut impl Write,
    ) -> Result<i32, VmError> {
        if self.halted {
            self.print_halt(output);
        } else {
            self.print_here(output);
        }
        let mut lines = input.lines();
        loop {
            let _ = write!(output, "(lilac-vm) ");
            if let Some(Ok(raw)) = lines.next() {
                if self.dispatch(raw.trim(), output)? {
                    return Ok(self.exit.unwrap_or(0));
                }
            } else {
                // End of input (or a read error): run the rest of the program,
                // ignoring breakpoints, so piped/no input terminates.
                while !self.halted {
                    self.advance()?;
                }
                return Ok(self.exit.unwrap_or(0));
            }
        }
    }

    /// Dispatches one command line; returns `true` when the user asked to quit.
    fn dispatch(&mut self, line: &str, output: &mut impl Write) -> Result<bool, VmError> {
        let (head, rest) = split_head(line);
        match head {
            "" => {}
            "break" | "b" => self.cmd_break(rest, output),
            "delete" | "d" => self.cmd_delete(rest, output),
            "clear" => {
                self.breakpoints.clear();
                let _ = writeln!(output, "cleared breakpoints");
            }
            "run" | "r" | "continue" | "c" => self.cmd_run(output)?,
            "step" | "s" => self.cmd_step(output)?,
            "stepi" | "si" => self.cmd_stepi(output)?,
            "next" | "n" => self.cmd_next(output)?,
            "finish" | "fin" => self.cmd_finish(output)?,
            "print" | "p" => self.cmd_print(rest, output),
            "info" | "i" => self.cmd_info(rest, output),
            "backtrace" | "bt" | "where" => self.cmd_backtrace(output),
            "disasm" | "dis" => self.cmd_disasm(rest, output),
            "list" | "l" => self.cmd_list(rest, output),
            "quit" | "q" | "exit" => return Ok(true),
            other => {
                let _ = writeln!(output, "unknown command `{other}`");
            }
        }
        Ok(false)
    }

    /// `break <loc>`.
    fn cmd_break(&mut self, loc: &str, output: &mut impl Write) {
        match self.set_breakpoint(loc) {
            Ok(()) => {
                let _ = writeln!(
                    output,
                    "breakpoint {} set at {}",
                    self.breakpoints.len(),
                    loc.trim()
                );
                self.print_breakpoints(output);
            }
            Err(err) => {
                let _ = writeln!(output, "error: {err}");
            }
        }
    }

    /// `delete <n>`.
    fn cmd_delete(&mut self, rest: &str, output: &mut impl Write) {
        let index = rest.trim().parse::<usize>().ok();
        match index {
            Some(n) if n >= 1 && n <= self.breakpoints.len() => {
                self.breakpoints.remove(n - 1);
                let _ = writeln!(output, "deleted breakpoint {n}");
                self.print_breakpoints(output);
            }
            _ => {
                let _ = writeln!(
                    output,
                    "delete needs a breakpoint number (see `info breaks`)"
                );
            }
        }
    }

    /// `run` / `continue`.
    fn cmd_run(&mut self, output: &mut impl Write) -> Result<(), VmError> {
        match self.resume()? {
            Stop::Breakpoint => self.print_here(output),
            Stop::Halted => self.print_halt(output),
        }
        Ok(())
    }

    /// `stepi`: execute exactly one instruction.
    fn cmd_stepi(&mut self, output: &mut impl Write) -> Result<(), VmError> {
        if self.halted {
            self.print_halt(output);
            return Ok(());
        }
        self.advance()?;
        self.finish_stop(output);
        Ok(())
    }

    /// `step`: run to the next source line, entering calls.
    fn cmd_step(&mut self, output: &mut impl Write) -> Result<(), VmError> {
        if self.halted {
            self.print_halt(output);
            return Ok(());
        }
        let start = self.about_key();
        loop {
            self.advance()?;
            if self.halted || self.about_key() != start {
                break;
            }
        }
        self.finish_stop(output);
        Ok(())
    }

    /// `next`: run to the next source line, stepping over calls.
    fn cmd_next(&mut self, output: &mut impl Write) -> Result<(), VmError> {
        if self.halted {
            self.print_halt(output);
            return Ok(());
        }
        let start_depth = self.depth();
        let start = self.about_key();
        loop {
            self.advance()?;
            if self.halted || (self.depth() <= start_depth && self.about_key() != start) {
                break;
            }
        }
        self.finish_stop(output);
        Ok(())
    }

    /// `finish`: run until the current frame returns to its caller.
    fn cmd_finish(&mut self, output: &mut impl Write) -> Result<(), VmError> {
        if self.halted {
            self.print_halt(output);
            return Ok(());
        }
        let start_depth = self.depth();
        loop {
            self.advance()?;
            if self.halted || self.depth() < start_depth {
                break;
            }
        }
        self.finish_stop(output);
        Ok(())
    }

    /// `print <reg>` or `print %<place>`.
    fn cmd_print(&mut self, rest: &str, output: &mut impl Write) {
        let arg = rest.trim();
        if arg.is_empty() {
            let _ = writeln!(output, "usage: print <reg> | print %<place>");
            return;
        }
        if let Some(place) = arg.strip_prefix('%') {
            match self.machine.read_place(place) {
                Some(value) => {
                    let _ = writeln!(output, "%{place} = {value} (0x{value:x})");
                }
                None => {
                    let _ = writeln!(output, "no place %{place} in the current frame");
                }
            }
            return;
        }
        let reg = arg.strip_prefix('&').unwrap_or(arg);
        match reg.parse::<u32>() {
            Ok(reg) => match self.machine.read_reg(reg) {
                Some(value) => {
                    let _ = writeln!(output, "&{reg} = {value} (0x{value:x})");
                }
                None => {
                    let _ = writeln!(output, "register &{reg} is not live");
                }
            },
            Err(_) => {
                let _ = writeln!(output, "print expects a register index or %place");
            }
        }
    }

    /// `info regs|stack|funcs|breaks`.
    fn cmd_info(&mut self, rest: &str, output: &mut impl Write) {
        match rest.trim() {
            "regs" | "registers" => self.info_regs(output),
            "stack" => self.cmd_backtrace(output),
            "funcs" | "functions" => {
                let _ = writeln!(output, "funcs:");
                for (index, func) in self.machine.program().funcs.iter().enumerate() {
                    let _ = writeln!(output, "  #{index} {}", func.name);
                }
            }
            "breaks" | "breakpoints" => self.print_breakpoints(output),
            _ => {
                let _ = writeln!(output, "info topics: regs, stack, funcs, breaks");
            }
        }
    }

    /// Prints the current frame's registers.
    fn info_regs(&self, output: &mut impl Write) {
        let _ = writeln!(output, "regs:");
        if let Some(func) = self.current_func() {
            for reg in 0..func.n_regs {
                let value = self.machine.read_reg(reg).unwrap_or(0);
                let _ = writeln!(output, "  &{reg} = {value} (0x{value:x})");
            }
        }
    }

    /// `backtrace`: one line per live frame.
    fn cmd_backtrace(&self, output: &mut impl Write) {
        let trace = self.machine.backtrace();
        let _ = writeln!(output, "backtrace:");
        for (index, frame) in trace.iter().enumerate() {
            let location = frame.line.as_ref().map_or_else(
                || format!("@{}", frame.pc),
                |line| format!("{}:{}", line.file, line.line),
            );
            let _ = writeln!(output, "  #{index} {} at {location}", frame.name);
        }
    }

    /// `disasm [pc]`: a window of canonical disassembly around `pc`.
    fn cmd_disasm(&self, rest: &str, output: &mut impl Write) {
        let Some(func) = self.current_func() else {
            let _ = writeln!(output, "no current function");
            return;
        };
        let program = self.machine.program();
        let pc = rest
            .trim()
            .parse::<usize>()
            .unwrap_or_else(|_| self.machine.pc() as usize);
        let start = pc.saturating_sub(3);
        let end = pc.saturating_add(4);
        let _ = writeln!(output, "disasm of {} (pc {pc}):", func.name);
        // Decode boundaries only, so a window never starts mid-instruction.
        let mut word_pc = 0_usize;
        while word_pc < func.code.len() {
            let Ok(text) = program.disasm_at(func, word_pc) else {
                break;
            };
            if word_pc >= start && word_pc < end {
                let marker = if word_pc == pc { "=>" } else { "  " };
                let _ = writeln!(output, "{marker} {word_pc}  {text}");
            }
            let next = match decode(&func.code, word_pc, &func.call_sites, &func.operands) {
                Ok((_, next)) => next as usize,
                Err(_) => break,
            };
            if next <= word_pc {
                break;
            }
            word_pc = next;
        }
    }

    /// `list [file:line]`: source lines around the current location.
    fn cmd_list(&mut self, rest: &str, output: &mut impl Write) {
        let source = self.locate_source();
        let Some(source) = source else {
            let _ = writeln!(output, "(source unavailable; showing disassembly)");
            self.cmd_disasm("", output);
            return;
        };
        let lines: Vec<&str> = source.lines().collect();
        let requested = rest.trim();
        let target = if requested.is_empty() {
            self.about_line().map(|line| line.line)
        } else {
            parse_line_loc(requested)
                .map(|(_, line)| line)
                .or_else(|| requested.parse::<u32>().ok())
        };
        let Some(target) = target else {
            let _ = writeln!(output, "list needs a line number or file:line");
            return;
        };
        let start = target.saturating_sub(2).max(1);
        let end = target.saturating_add(3);
        for number in start..=end {
            if let Some(text) = lines.get((number - 1) as usize) {
                let _ = writeln!(output, "{number}\t{text}");
            }
        }
    }

    /// Whether any breakpoint matches the current location.
    ///
    /// This is purely positional: the one-hit suppression that stops a resume
    /// from re-triggering the location it is parked on lives in
    /// [`Debugger::skip_hit`], not here.
    fn hit_breakpoint(&self) -> bool {
        let (func_index, pc) = (self.machine.func_index(), self.machine.pc());
        for breakpoint in &self.breakpoints {
            match breakpoint {
                Breakpoint::Func { index, .. } => {
                    if *index == func_index && pc == 0 {
                        return true;
                    }
                }
                Breakpoint::Line { file, line } => {
                    let Some(current) = self.about_line() else {
                        continue;
                    };
                    if current.line != *line || !file_matches(&current.file, file) {
                        continue;
                    }
                    // Match only the line's entry instruction so `continue`
                    // re-arms on the next visit (e.g. the next loop iteration)
                    // rather than stopping on every instruction of the line.
                    let Some(func) = self.func(func_index) else {
                        continue;
                    };
                    if is_line_start(func, pc, &current.file, current.line) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Resumes until a breakpoint matches or the program halts.
    ///
    /// When the debugger is parked on a breakpoint just hit, the first resume
    /// steps once before checking again ([`Debugger::skip_hit`]); after that
    /// single step every breakpoint is fully re-armed.
    fn resume(&mut self) -> Result<Stop, VmError> {
        loop {
            if self.halted {
                return Ok(Stop::Halted);
            }
            if !self.skip_hit && self.hit_breakpoint() {
                self.skip_hit = true;
                return Ok(Stop::Breakpoint);
            }
            self.advance()?;
        }
    }

    /// Executes one instruction, recording halt state and re-arming breakpoints.
    fn advance(&mut self) -> Result<(), VmError> {
        self.skip_hit = false;
        match self.machine.step()? {
            Step::Advanced => {}
            Step::Returned => {
                self.halted = true;
                self.exit = Some(0);
            }
            Step::Panicked(code) => {
                self.halted = true;
                self.exit = Some(code);
            }
        }
        Ok(())
    }

    /// Prints the location of a step stop.
    fn finish_stop(&self, output: &mut impl Write) {
        if self.halted {
            self.print_halt(output);
        } else {
            self.print_here(output);
        }
    }

    /// Prints the next instruction's location.
    fn print_here(&self, output: &mut impl Write) {
        match self.about_line() {
            Some(line) => {
                let _ = writeln!(
                    output,
                    "stopped at {}:{}:{} in {}",
                    line.file,
                    line.line,
                    line.col,
                    self.current_name()
                );
            }
            None => {
                let _ = writeln!(
                    output,
                    "stopped at @{}:{}",
                    self.current_name(),
                    self.machine.pc()
                );
            }
        }
    }

    /// Prints the exit status.
    fn print_halt(&self, output: &mut impl Write) {
        match self.exit {
            Some(code) => {
                let _ = writeln!(output, "program exited with code {code}");
            }
            None => {
                let _ = writeln!(output, "program halted");
            }
        }
    }

    /// Prints the breakpoint table with 1-based numbers.
    fn print_breakpoints(&self, output: &mut impl Write) {
        if self.breakpoints.is_empty() {
            let _ = writeln!(output, "no breakpoints");
            return;
        }
        let _ = writeln!(output, "breakpoints:");
        for (index, breakpoint) in self.breakpoints.iter().enumerate() {
            match breakpoint {
                Breakpoint::Func { name, .. } => {
                    let _ = writeln!(output, "  {}: break {}", index + 1, name);
                }
                Breakpoint::Line { file, line } => {
                    let _ = writeln!(output, "  {}: break {file}:{line}", index + 1);
                }
            }
        }
    }

    /// The source line of the next instruction to execute.
    fn about_line(&self) -> Option<Line> {
        line_at(self.current_func()?, self.machine.pc())
    }

    /// A comparable `(file, line)` key for the current location.
    fn about_key(&self) -> Option<(String, u32)> {
        self.about_line().map(|line| (line.file, line.line))
    }

    /// The current frame's function, or `None` when the program is halted.
    fn current_func(&self) -> Option<&Func> {
        if self.halted {
            return None;
        }
        self.func(self.machine.func_index())
    }

    /// The function at `index`.
    fn func(&self, index: u32) -> Option<&Func> {
        self.machine.program().funcs.get(index as usize)
    }

    /// The current function's name.
    fn current_name(&self) -> String {
        self.current_func()
            .map_or_else(|| "<none>".to_string(), |func| func.name.clone())
    }

    /// The number of live frames.
    fn depth(&self) -> usize {
        self.machine.backtrace().len()
    }

    /// Loads source text from the override or the line table's recorded path.
    fn locate_source(&self) -> Option<String> {
        if let Some(source) = &self.source {
            return Some(source.clone());
        }
        let path = self
            .machine
            .program()
            .funcs
            .iter()
            .find_map(|func| func.lines.first().map(|line| line.file.clone()))?;
        std::fs::read_to_string(path).ok()
    }
}

/// Splits a `file:line` location; the suffix must parse as a `u32`.
fn parse_line_loc(loc: &str) -> Option<(String, u32)> {
    let (file, line) = loc.rsplit_once(':')?;
    let line = line.parse::<u32>().ok()?;
    Some((file.to_string(), line))
}

/// Whether a recorded file name matches a breakpoint's requested file.
fn file_matches(actual: &str, wanted: &str) -> bool {
    actual == wanted || actual.ends_with(wanted)
}

/// The source line whose `pc` is the greatest not exceeding `pc`.
fn line_at(func: &Func, pc: u32) -> Option<Line> {
    func.lines.iter().rev().find(|line| line.pc <= pc).cloned()
}

/// Whether `pc` is the first instruction attributed to `(file, line)`.
///
/// A line breakpoint matches only the line's entry instruction, so a loop that
/// branches back to that instruction fires once per iteration rather than once
/// per instruction that shares the line.
fn is_line_start(func: &Func, pc: u32, file: &str, line: u32) -> bool {
    let here = func
        .lines
        .iter()
        .any(|entry| entry.pc == pc && entry.line == line && entry.file == file);
    here && !func
        .lines
        .iter()
        .any(|entry| entry.pc < pc && entry.line == line && entry.file == file)
}

/// Splits off the first whitespace-delimited token.
fn split_head(text: &str) -> (&str, &str) {
    text.split_once(char::is_whitespace)
        .map_or((text, ""), |(head, rest)| (head, rest.trim_start()))
}

#[cfg(test)]
mod tests {
    use super::Debugger;
    use crate::compile::compile_src;
    use crate::exec::Machine;
    use crate::isa::{self, Instr, Operand};

    /// `main` calls the recursive `f`, which recurses while `n < 3`.
    ///
    /// Built directly (not via `compile_src`) so the constant folder cannot
    /// evaluate the recursion and prune `f` — the reason the original
    /// `break_at_function_stops` test was vacuous.
    fn recursive_program() -> isa::Program {
        let mut program = isa::sample_program(vec![
            Instr::Const {
                dst: 0,
                imm: 0,
                ty: 0,
            },
            Instr::Copy {
                dst: Operand::Slot(0),
                src: Operand::Reg(0),
                ty: 0,
            },
            Instr::Call {
                func: 1,
                ins: vec![Operand::Slot(0)],
                outs: Vec::new(),
            },
            Instr::Return,
        ]);
        let (fcode, fcall, fops) = isa::words_of(
            &program.types,
            &[
                Instr::Copy {
                    dst: Operand::Reg(1),
                    src: Operand::Slot(0),
                    ty: 0,
                },
                Instr::Const {
                    dst: 2,
                    imm: 3,
                    ty: 0,
                },
                Instr::Arith {
                    op: isa::ArithOp::ILt,
                    dst: 3,
                    lhs: 1,
                    rhs: 2,
                    operand_ty: 0,
                    ty: 0,
                },
                Instr::BranchIf {
                    cond: 3,
                    then: 4,
                    els: 8,
                },
                Instr::Const {
                    dst: 2,
                    imm: 1,
                    ty: 0,
                },
                Instr::Arith {
                    op: isa::ArithOp::IAdd,
                    dst: 3,
                    lhs: 1,
                    rhs: 2,
                    operand_ty: 0,
                    ty: 0,
                },
                Instr::Call {
                    func: 1,
                    ins: vec![Operand::Reg(3)],
                    outs: Vec::new(),
                },
                Instr::Return,
                Instr::Return,
            ],
        );
        program.funcs.push(isa::Func {
            name: "f".to_string(),
            is_extern: false,
            ins: vec![0],
            outs: Vec::new(),
            in_offsets: Vec::new(),
            in_sizes: Vec::new(),
            out_offset: 0,
            out_sizes: Vec::new(),
            n_regs: 4,
            slot_bytes: 8,
            arg_reg: None,
            ret_reg: None,
            code: fcode,
            call_sites: fcall,
            operands: fops,
            lines: vec![isa::Line {
                pc: 0,
                file: "t.lilac".to_string(),
                line: 1,
                col: 1,
            }],
            place_names: Vec::new(),
        });
        program.rebuild_layouts();
        program.verify().expect("verify");
        program
    }

    #[test]
    fn stepi_advances_one_instruction() {
        let program =
            compile_src("main: function () nothing {\n    x: @bit_native = @i_add(0x1, 0x2)\n}\n");
        let machine = Machine::new(&program).expect("entry");
        let mut dbg = Debugger::new(machine, None);
        let mut out = Vec::new();
        dbg.run_repl(
            std::io::Cursor::new(b"stepi\ndisasm\ninfo regs\nquit\n"),
            &mut out,
        )
        .expect("repl");
        let text = String::from_utf8(out).expect("utf8");
        // One `stepi` runs the entry instruction (word 0, a three-word `const`)
        // and no more: the disassembly marker sits on word pc 3, not the
        // following word, and the value written by pc 0 is live.
        assert!(text.contains("=> 3"), "transcript:\n{text}");
        assert!(!text.contains("=> 6"), "transcript:\n{text}");
        assert!(text.contains("&0 = 1 (0x1)"), "transcript:\n{text}");
    }

    #[test]
    fn break_at_function_stops() {
        let program = recursive_program();
        let machine = Machine::new(&program).expect("entry");
        let mut dbg = Debugger::new(machine, None);
        dbg.set_breakpoint("f").expect("breakpoint");
        let mut out = Vec::new();
        dbg.run_repl(std::io::Cursor::new(b"run\nbacktrace\nquit\n"), &mut out)
            .expect("repl");
        let text = String::from_utf8(out).expect("utf8");
        // `run` stops at f's entry, so the current frame is f and the
        // backtrace lists f as the running frame under main.
        assert!(text.contains("in f"), "transcript:\n{text}");
        assert!(text.contains("#1 f at"), "transcript:\n{text}");
    }

    /// A line breakpoint on a loop body must fire on every iteration, not just
    /// the first: `continue` must re-arm it once execution leaves the stopped
    /// instruction.
    #[test]
    fn line_breakpoint_fires_on_each_loop_iteration() {
        let program = compile_src(
            "main: function () nothing {\n    n: @bit_native = 0x0\n    loop {\n        n = @i_add(n, 0x1)\n        if @i_less_than(n, 0x3) {\n            keep: @bit_native = n\n        } else {\n            break\n        }\n    }\n}\n",
        );
        let machine = Machine::new(&program).expect("entry");
        let mut dbg = Debugger::new(machine, None);
        dbg.set_breakpoint("t.lilac:4").expect("breakpoint");
        let mut out = Vec::new();
        dbg.run_repl(
            std::io::Cursor::new(b"run\ncontinue\ncontinue\ncontinue\nquit\n"),
            &mut out,
        )
        .expect("repl");
        let text = String::from_utf8(out).expect("utf8");
        // The body line executes for n = 1, 2, 3.
        let hits = text.matches("stopped at t.lilac:4").count();
        assert!(hits >= 3, "line breakpoint fired {hits} times:\n{text}");
    }

    /// A function breakpoint must fire on every entry, including recursive
    /// re-entries that restart at the same `pc == 0`.
    #[test]
    fn function_breakpoint_fires_on_recursive_reentry() {
        let program = recursive_program();
        let machine = Machine::new(&program).expect("entry");
        let mut dbg = Debugger::new(machine, None);
        dbg.set_breakpoint("f").expect("breakpoint");
        let mut out = Vec::new();
        dbg.run_repl(
            std::io::Cursor::new(b"run\ncontinue\ncontinue\ncontinue\nquit\n"),
            &mut out,
        )
        .expect("repl");
        let text = String::from_utf8(out).expect("utf8");
        // f is entered for n = 0, 1, 2, 3.
        let hits = text.matches("in f").count();
        assert!(hits >= 3, "function breakpoint fired {hits} times:\n{text}");
    }
}
