//! Builds the runtime functions as ordinary LIR.
//!
//! The functions are not injected here; the driver appends `build_runtime` to
//! a module between lowering and dead-code pruning, where DCE keeps
//! only the ones a program reaches.
//!
//! Besides the byte-buffer and format-args helpers, the runtime holds the
//! process entry stub `.lilac.start` (see [`entry_point`]) and the panic
//! handler `.lilac.panic` (see [`panic_runtime`]). Neither calls the OS: they
//! go through the program's platform hooks (`.lilac.entry`, `.lilac.exit`,
//! `.lilac.compiler_print`), which a runtime library such as `Standard`
//! defines.
//!
//! Every place is addressed (the pointer place ABI), so the bodies only need
//! `alloca`/`field`/`field.addr`, `copy`/`store`, `convert`, `arith`,
//! `branch`, and the `@ptr_add` and `@lilac_abort` leaves. Registers are
//! block-local in the verifier, so each block re-materializes the values it
//! uses.

use lilac_span::Span;

use crate::{
    ArithOp, ConvertOp, LirBlock, LirCallee, LirConst, LirData, LirFunc, LirInst, LirOperand,
    LirTerm, LirTy,
};

/// How a program enters: which symbol calls the entry stub `.lilac.start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Entry {
    /// A C `i32 main()` that calls `.lilac.start`, entered through the C
    /// runtime's startup code (`--libc` builds, `check` and `emit-llvm`).
    #[default]
    CMain,
    /// `.lilac.start` ([`entry_point`]) is the process entry, with no C
    /// runtime.
    Native,
}

/// Builds the runtime prelude for a target with `ptr_size`-byte pointers.
///
/// The functions are returned in call-dependency order: `byte_buffer_from`,
/// `byte_buffer_ptr`, `byte_buffer_len`, `load_u8`, `store_u8`,
/// `format_args_new`, `format_args_push`, `format_args_count`,
/// `format_args_piece`. The piece stride is `#byte_buffer`'s size,
/// `2 * ptr_size` bytes.
#[must_use]
pub fn build_runtime(ptr_size: u32) -> Vec<LirFunc> {
    let stride = 2 * u64::from(ptr_size);
    vec![
        byte_buffer_from(),
        byte_buffer_ptr(),
        byte_buffer_len(),
        load_u8(),
        store_u8(),
        format_args_new(),
        format_args_push(stride),
        format_args_count(),
        format_args_piece(stride),
    ]
}

/// The process entry stub `.lilac.start(count, arguments, environment)`:
/// calls the program's `@entry` hook (`.lilac.entry`) with the process's
/// argument count, argument vector and environment vector (C's `argc`,
/// `argv` and `envp`; zero and null where the OS passes none), then
/// `@exit(0)` (`.lilac.exit`), then traps, so an `@exit` that returns stops
/// rather than running on. It makes no OS call itself. The backend's entry
/// wrapper (the C `main`, or the native entry) calls it.
#[must_use]
pub fn entry_point() -> LirFunc {
    let mut insts = vec![call(ENTRY_HOOK, &ENTRY_PARAMS.map(|(name, _)| name), &[])];
    insts.extend(const_place("code", 0, LirTy::Bit(32)));
    insts.push(call(EXIT_HOOK, &["code"], &[]));
    insts.push(call_builtin("@lilac_abort", &[], &[]));
    lir_func(
        ".lilac.start",
        &entry_params(),
        &[],
        vec![block("entry", insts, unreachable())],
    )
}

/// The parameters of `.lilac.start` and `@entry`, as `(name, is a ref)`:
/// the count is a `#bit_native`, the vectors `ref #unknown`.
const ENTRY_PARAMS: [(&str, bool); 3] =
    [("count", false), ("arguments", true), ("environment", true)];

/// [`ENTRY_PARAMS`] typed.
fn entry_params() -> Vec<(&'static str, LirTy)> {
    ENTRY_PARAMS
        .iter()
        .map(|(name, is_ref)| {
            let ty = if *is_ref {
                LirTy::opaque_ref()
            } else {
                LirTy::BitNative
            };
            (*name, ty)
        })
        .collect()
}

/// The platform hooks' fixed symbols (`lilac_hir::PLATFORM_HOOKS`).
const ENTRY_HOOK: &str = ".lilac.entry";
const EXIT_HOOK: &str = ".lilac.exit";
const PRINT_HOOK: &str = ".lilac.compiler_print";

/// `extern` declarations of the `@exit` and `@compiler_print` hooks, for a
/// program that defines neither (outside an executable): its panics then
/// resolve against the hooks of whatever it is linked into.
#[must_use]
pub fn hook_externs() -> [LirFunc; 2] {
    [
        extern_func(EXIT_HOOK, &[("code", LirTy::Bit(32))], &[]),
        extern_func(PRINT_HOOK, &[("text", LirTy::byte_buffer())], &[]),
    ]
}

/// Data written around a panic message.
const PANIC_HEAD: &str = ".lilac.panic.head";
const PANIC_NEWLINE: &str = ".lilac.panic.newline";
const PANIC_ELIDED: &str = ".lilac.panic.elided";
const PANIC_NO_TRACE: &str = ".lilac.panic.no_trace";
/// Frames written before a trace is cut short (deep recursion).
const PANIC_MAX_FRAMES: u64 = 128;
/// A panicking process's exit code (101, Rust's convention).
const PANIC_EXIT_CODE: u64 = 101;

/// The panic handler `.lilac.panic(#byte_buffer %msg, #byte_buffer %site)`
/// and the data it writes through the `@compiler_print` hook: `panic:
/// <msg>`, then the stack trace. It then calls `@exit(101)` and traps, so it
/// never returns.
///
/// The trace walks the shadow stack `@trace_head` points at (see
/// [`crate::trace`]), writing each frame's `site` line, innermost first, and
/// `  ...` after 128 frames. With no frame pushed (a build without stack
/// traces) it writes the panic's own `site` and a hint instead. Every line
/// already ends in a newline.
///
/// It is kept apart from [`build_runtime`] because it carries data; DCE drops
/// both from a program that never panics.
#[must_use]
pub fn panic_runtime() -> (LirFunc, Vec<LirData>) {
    let data = [
        (PANIC_HEAD, "panic: "),
        (PANIC_NEWLINE, "\n"),
        (PANIC_ELIDED, "  ...\n"),
        (
            PANIC_NO_TRACE,
            "  (stack trace disabled; build with --stack-traces=on)\n",
        ),
    ]
    .into_iter()
    .map(|(name, text)| LirData {
        name: name.into(),
        bytes: text.as_bytes().to_vec(),
        span: span(),
    })
    .collect();
    (panic_handler(), data)
}

fn panic_handler() -> LirFunc {
    let frame_ref = LirTy::trace_frame().to_ref();
    let mut entry = vec![
        alloca(LirTy::byte_buffer(), "text"),
        alloca(frame_ref.clone().to_ref(), "head"),
        alloca(frame_ref.clone(), "frame"),
        alloca(LirTy::BitNative, "count"),
    ];
    entry.extend(write_data(PANIC_HEAD, "a"));
    entry.push(write_buffer("msg"));
    entry.extend(write_data(PANIC_NEWLINE, "c"));
    entry.extend([
        call_builtin("@trace_head", &[], &["head"]),
        load("head", "top", frame_ref.clone()),
        copy_place(LirOperand::Reg("top".into()), "frame"),
    ]);
    entry.extend(is_null("top", "untraced"));

    let mut untraced = vec![write_buffer("site")];
    untraced.extend(write_data(PANIC_NO_TRACE, "b"));

    let mut walk = vec![copy_reg(
        LirOperand::Place("frame".into()),
        "cur",
        frame_ref.clone(),
    )];
    walk.extend(is_null("cur", "done"));

    let check = vec![
        copy_reg(LirOperand::Place("count".into()), "n", LirTy::BitNative),
        constant("max", LirConst::Int(PANIC_MAX_FRAMES), LirTy::BitNative),
        arith(ArithOp::Eq, "n", "max", "full", LirTy::Bit(8)),
    ];

    let mut print = vec![
        field("frame", "site", 1, "site", LirTy::byte_buffer()),
        copy_place(LirOperand::Reg("site".into()), "text"),
    ];
    print.push(write_buffer("text"));
    print.extend([
        field("frame", "prev", 0, "prev", frame_ref),
        copy_place(LirOperand::Reg("prev".into()), "frame"),
        copy_reg(LirOperand::Place("count".into()), "n", LirTy::BitNative),
        constant("one", LirConst::Int(1), LirTy::BitNative),
        arith(ArithOp::IAdd, "n", "one", "next", LirTy::BitNative),
        copy_place(LirOperand::Reg("next".into()), "count"),
    ]);

    let mut exit = const_place("code", PANIC_EXIT_CODE, LirTy::Bit(32)).to_vec();
    exit.push(call(EXIT_HOOK, &["code"], &[]));
    exit.push(call_builtin("@lilac_abort", &[], &[]));

    lir_func(
        ".lilac.panic",
        &[
            ("msg", LirTy::byte_buffer()),
            ("site", LirTy::byte_buffer()),
        ],
        &[],
        vec![
            block("entry", entry, branch_if("untraced", "untraced", "walk")),
            block("untraced", untraced, branch("exit")),
            block("walk", walk, branch_if("done", "exit", "check")),
            block("check", check, branch_if("full", "elided", "print")),
            block("print", print, branch("walk")),
            block("elided", write_data(PANIC_ELIDED, "a"), branch("exit")),
            block("exit", exit, unreachable()),
        ],
    )
}

/// Writes the `#byte_buffer` place `buffer` through `@compiler_print`.
fn write_buffer(buffer: &str) -> LirInst {
    call(PRINT_HOOK, &[buffer], &[])
}

/// Writes the data constant `name` through `%text`.
fn write_data(name: &str, tag: &str) -> Vec<LirInst> {
    let reg = format!("{tag}.text");
    vec![
        constant(&reg, LirConst::Data(name.into()), LirTy::byte_buffer()),
        copy_place(LirOperand::Reg(reg.into()), "text"),
        write_buffer("text"),
    ]
}

/// `&out = (&ptr as address) == 0`.
fn is_null(ptr: &str, out: &str) -> [LirInst; 3] {
    let (address, zero) = (format!("{out}.address"), format!("{out}.zero"));
    [
        convert(ConvertOp::Bitcast, ptr, &address, LirTy::BitNative),
        constant(&zero, LirConst::Int(0), LirTy::BitNative),
        arith(ArithOp::Eq, &address, &zero, out, LirTy::Bit(8)),
    ]
}

fn span() -> Span {
    Span::dummy()
}

fn alloca(ty: LirTy, place: &str) -> LirInst {
    LirInst::Alloca {
        ty,
        place: place.into(),
        span: span(),
    }
}

fn field(obj: &str, name: &str, idx: u32, dst: &str, ty: LirTy) -> LirInst {
    LirInst::Field {
        obj: obj.into(),
        name: name.to_owned(),
        idx,
        dst: dst.into(),
        ty,
        span: span(),
    }
}

fn field_addr(obj: &str, name: &str, idx: u32, dst: &str, ty: LirTy) -> LirInst {
    LirInst::FieldAddr {
        obj: obj.into(),
        name: name.to_owned(),
        idx,
        dst: dst.into(),
        ty,
        span: span(),
    }
}

fn store_field(value: LirOperand, obj: &str, name: &str, idx: u32) -> LirInst {
    LirInst::StoreField {
        value,
        obj: obj.into(),
        name: name.to_owned(),
        idx,
        span: span(),
    }
}

fn store(src: LirOperand, dst: &str) -> LirInst {
    LirInst::Store {
        src,
        dst: dst.into(),
        span: span(),
    }
}

fn copy_place(src: LirOperand, dst: &str) -> LirInst {
    LirInst::CopyPlace {
        src,
        dst: dst.into(),
        span: span(),
    }
}

fn copy_reg(src: LirOperand, dst: &str, ty: LirTy) -> LirInst {
    LirInst::CopyReg {
        src,
        dst: dst.into(),
        ty,
        span: span(),
    }
}

fn load(src: &str, dst: &str, ty: LirTy) -> LirInst {
    LirInst::Load {
        src: src.into(),
        dst: dst.into(),
        ty,
        span: span(),
    }
}

fn constant(dst: &str, value: LirConst, ty: LirTy) -> LirInst {
    LirInst::Const {
        dst: dst.into(),
        value,
        ty,
        span: span(),
    }
}

fn arith(op: ArithOp, lhs: &str, rhs: &str, dst: &str, ty: LirTy) -> LirInst {
    LirInst::Arith {
        op,
        lhs: lhs.into(),
        rhs: rhs.into(),
        dst: dst.into(),
        ty,
        span: span(),
    }
}

fn call(name: &str, ins: &[&str], outs: &[&str]) -> LirInst {
    LirInst::Call {
        func: LirCallee::Func(name.into()),
        ins: ins.iter().map(|place| (*place).into()).collect(),
        outs: outs.iter().map(|place| (*place).into()).collect(),
        span: span(),
    }
}

fn call_builtin(name: &str, ins: &[&str], outs: &[&str]) -> LirInst {
    LirInst::Call {
        func: LirCallee::Builtin(name.to_owned()),
        ins: ins.iter().map(|place| (*place).into()).collect(),
        outs: outs.iter().map(|place| (*place).into()).collect(),
        span: span(),
    }
}

fn block(label: &str, insts: Vec<LirInst>, term: LirTerm) -> LirBlock {
    LirBlock {
        label: label.into(),
        insts,
        term,
        span: span(),
    }
}

fn ret() -> LirTerm {
    LirTerm::Return { span: span() }
}

fn unreachable() -> LirTerm {
    LirTerm::Unreachable { span: span() }
}

fn branch(target: &str) -> LirTerm {
    LirTerm::Branch {
        target: target.into(),
        span: span(),
    }
}

fn branch_if(cond: &str, then: &str, els: &str) -> LirTerm {
    LirTerm::BranchIf {
        cond: cond.into(),
        then: then.into(),
        els: Some(els.into()),
        span: span(),
    }
}

fn lir_func(
    name: &str,
    ins: &[(&str, LirTy)],
    outs: &[(&str, LirTy)],
    blocks: Vec<LirBlock>,
) -> LirFunc {
    LirFunc {
        name: name.into(),
        ins: ins
            .iter()
            .map(|(place, ty)| ((*place).into(), ty.clone()))
            .collect(),
        outs: outs
            .iter()
            .map(|(place, ty)| ((*place).into(), ty.clone()))
            .collect(),
        is_extern: false,
        export: None,
        variadic: false,
        inline: false,
        blocks,
    }
}

fn extern_func(name: &str, ins: &[(&str, LirTy)], outs: &[(&str, LirTy)]) -> LirFunc {
    LirFunc {
        is_extern: true,
        export: None,
        variadic: false,
        ..lir_func(name, ins, outs, Vec::new())
    }
}

/// A place holding the integer `value`.
fn const_place(place: &str, value: u64, ty: LirTy) -> [LirInst; 3] {
    let reg = format!("{place}.value");
    [
        alloca(ty.clone(), place),
        constant(&reg, LirConst::Int(value), ty),
        copy_place(LirOperand::Reg(reg.into()), place),
    ]
}

fn convert(op: ConvertOp, src: &str, dst: &str, ty: LirTy) -> LirInst {
    LirInst::Convert {
        op,
        src: src.into(),
        dst: dst.into(),
        ty,
        span: span(),
    }
}

/// A `#bit_native` place `place` holding register `reg` (of type `from`)
/// converted with `op`.
/// `.lilac.byte_buffer_from(ref #bit8 %data, #bit_native %len) -> #byte_buffer`.
fn byte_buffer_from() -> LirFunc {
    lir_func(
        ".lilac.byte_buffer_from",
        &[("data", LirTy::byte_ref()), ("len", LirTy::BitNative)],
        &[("out", LirTy::byte_buffer())],
        vec![block(
            "entry",
            vec![
                alloca(LirTy::byte_buffer(), "tmp"),
                store_field(LirOperand::Place("len".into()), "tmp", "len", 0),
                store_field(LirOperand::Place("data".into()), "tmp", "data", 1),
                copy_place(LirOperand::Place("tmp".into()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.byte_buffer_ptr(#byte_buffer %buffer) -> ref #bit8`.
fn byte_buffer_ptr() -> LirFunc {
    lir_func(
        ".lilac.byte_buffer_ptr",
        &[("buffer", LirTy::byte_buffer())],
        &[("out", LirTy::byte_ref())],
        vec![block(
            "entry",
            vec![
                field("buffer", "data", 1, "r", LirTy::byte_ref()),
                copy_place(LirOperand::Reg("r".into()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.byte_buffer_len(#byte_buffer %buffer) -> #bit_native`.
fn byte_buffer_len() -> LirFunc {
    lir_func(
        ".lilac.byte_buffer_len",
        &[("buffer", LirTy::byte_buffer())],
        &[("out", LirTy::BitNative)],
        vec![block(
            "entry",
            vec![
                field("buffer", "len", 0, "r", LirTy::BitNative),
                copy_place(LirOperand::Reg("r".into()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.load_u8(ref #bit8 %base, #bit_native %index) -> #bit8`.
///
/// Addresses `base + index` with `@ptr_add`, then loads the byte through the
/// resulting `ref #bit8` place.
fn load_u8() -> LirFunc {
    lir_func(
        ".lilac.load_u8",
        &[("base", LirTy::byte_ref()), ("index", LirTy::BitNative)],
        &[("out", LirTy::Bit(8))],
        vec![block(
            "entry",
            vec![
                alloca(LirTy::byte_ref(), "slot"),
                call_builtin("@ptr_add", &["base", "index"], &["slot"]),
                load("slot", "r", LirTy::Bit(8)),
                copy_place(LirOperand::Reg("r".into()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.store_u8(ref #bit8 %base, #bit_native %index, #bit8 %value)`.
///
/// Addresses `base + index` with `@ptr_add`, then stores the byte through the
/// resulting `ref #bit8` place.
fn store_u8() -> LirFunc {
    lir_func(
        ".lilac.store_u8",
        &[
            ("base", LirTy::byte_ref()),
            ("index", LirTy::BitNative),
            ("value", LirTy::Bit(8)),
        ],
        &[],
        vec![block(
            "entry",
            vec![
                alloca(LirTy::byte_ref(), "slot"),
                call_builtin("@ptr_add", &["base", "index"], &["slot"]),
                store(LirOperand::Place("value".into()), "slot"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.format_args_new() -> #format_args` (a zeroed piece table).
fn format_args_new() -> LirFunc {
    lir_func(
        ".lilac.format_args_new",
        &[],
        &[("out", LirTy::format_args())],
        vec![block(
            "entry",
            vec![
                alloca(LirTy::format_args(), "tmp"),
                copy_place(LirOperand::Place("tmp".into()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.format_args_push(ref #format_args %table, #byte_buffer %piece)`.
///
/// Appends `piece` while `count < 16`, then increments `count`.
fn format_args_push(stride: u64) -> LirFunc {
    lir_func(
        ".lilac.format_args_push",
        &[
            ("table", LirTy::format_args().to_ref()),
            ("piece", LirTy::byte_buffer()),
        ],
        &[],
        vec![
            block(
                "entry",
                vec![
                    field("table", "count", 0, "c", LirTy::BitNative),
                    constant("cap", LirConst::Int(16), LirTy::BitNative),
                    arith(ArithOp::ILtU, "c", "cap", "ok", LirTy::Bit(8)),
                ],
                branch_if("ok", "append", "done"),
            ),
            block(
                "append",
                vec![
                    field("table", "count", 0, "c", LirTy::BitNative),
                    field_addr("table", "p0", 1, "p0", LirTy::byte_buffer().to_ref()),
                    alloca(LirTy::byte_ref(), "base"),
                    copy_place(LirOperand::Reg("p0".into()), "base"),
                    constant("stride", LirConst::Int(stride), LirTy::BitNative),
                    arith(ArithOp::IMul, "c", "stride", "off", LirTy::BitNative),
                    alloca(LirTy::BitNative, "offp"),
                    copy_place(LirOperand::Reg("off".into()), "offp"),
                    alloca(LirTy::byte_ref(), "slot"),
                    call_builtin("@ptr_add", &["base", "offp"], &["slot"]),
                    alloca(LirTy::byte_buffer().to_ref(), "s"),
                    copy_place(LirOperand::Place("slot".into()), "s"),
                    store(LirOperand::Place("piece".into()), "s"),
                    constant("one", LirConst::Int(1), LirTy::BitNative),
                    arith(ArithOp::IAdd, "c", "one", "n", LirTy::BitNative),
                    store_field(LirOperand::Reg("n".into()), "table", "count", 0),
                ],
                branch("done"),
            ),
            block("done", Vec::new(), ret()),
        ],
    )
}

/// `.lilac.format_args_count(#format_args %table) -> #bit_native`.
fn format_args_count() -> LirFunc {
    lir_func(
        ".lilac.format_args_count",
        &[("table", LirTy::format_args())],
        &[("out", LirTy::BitNative)],
        vec![block(
            "entry",
            vec![
                field("table", "count", 0, "r", LirTy::BitNative),
                copy_place(LirOperand::Reg("r".into()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.format_args_piece(#format_args %table, #bit_native %index) ->
/// #byte_buffer`.
///
/// Reads `p0[index]`, trapping through `@lilac_abort` when `index >= count`.
fn format_args_piece(stride: u64) -> LirFunc {
    lir_func(
        ".lilac.format_args_piece",
        &[("table", LirTy::format_args()), ("index", LirTy::BitNative)],
        &[("out", LirTy::byte_buffer())],
        vec![
            block(
                "entry",
                vec![
                    field("table", "count", 0, "c", LirTy::BitNative),
                    copy_reg(LirOperand::Place("index".into()), "idx", LirTy::BitNative),
                    arith(ArithOp::ILtU, "idx", "c", "ok", LirTy::Bit(8)),
                ],
                branch_if("ok", "load", "trap"),
            ),
            block(
                "trap",
                vec![call_builtin("@lilac_abort", &[], &[])],
                unreachable(),
            ),
            block(
                "load",
                vec![
                    copy_reg(LirOperand::Place("index".into()), "idx", LirTy::BitNative),
                    field_addr("table", "p0", 1, "p0", LirTy::byte_buffer().to_ref()),
                    alloca(LirTy::byte_ref(), "base"),
                    copy_place(LirOperand::Reg("p0".into()), "base"),
                    constant("stride", LirConst::Int(stride), LirTy::BitNative),
                    arith(ArithOp::IMul, "idx", "stride", "off", LirTy::BitNative),
                    alloca(LirTy::BitNative, "offp"),
                    copy_place(LirOperand::Reg("off".into()), "offp"),
                    alloca(LirTy::byte_ref(), "slot"),
                    call_builtin("@ptr_add", &["base", "offp"], &["slot"]),
                    alloca(LirTy::byte_buffer().to_ref(), "s"),
                    copy_place(LirOperand::Place("slot".into()), "s"),
                    load("s", "v", LirTy::byte_buffer()),
                    copy_place(LirOperand::Reg("v".into()), "out"),
                ],
                ret(),
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::{build_runtime, entry_point, hook_externs};
    use crate::{verify, ArithOp, LirConst, LirFunc, LirInst, LirModule};

    /// The panic handler and the entry stub verify against the hooks'
    /// declarations.
    #[test]
    fn panic_handler_and_entry_verify() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.funcs = runtime(8);
        module.funcs.extend(hook_externs());
        module.funcs.push(LirFunc {
            is_extern: true,
            export: None,
            ..super::lir_func(".lilac.entry", &super::entry_params(), &[], Vec::new())
        });
        module.funcs.push(entry_point());
        let (handler, data) = super::panic_runtime();
        module.funcs.push(handler);
        module.data = data;
        let mut diags = lilac_error::Diags::new();
        assert!(
            verify(&module, &mut diags).is_ok(),
            "{:?}",
            diags.diagnostics()
        );
    }

    /// The entry stub calls `@entry`, then `@exit(0)`, then traps.
    #[test]
    fn entry_stub_calls_the_hooks_then_traps() {
        let start = entry_point();
        let calls: Vec<String> = start.blocks[0]
            .insts
            .iter()
            .filter_map(|inst| match inst {
                LirInst::Call { func, .. } => Some(format!("{func:?}")),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls[0].contains(".lilac.entry"), "{calls:?}");
        assert!(calls[1].contains(".lilac.exit"), "{calls:?}");
        assert!(calls[2].contains("@lilac_abort"), "{calls:?}");
    }

    fn runtime(ptr_size: u32) -> Vec<LirFunc> {
        build_runtime(ptr_size)
    }

    fn e070_count(diags: &lilac_error::Diags) -> usize {
        diags
            .diagnostics()
            .iter()
            .filter(|d| d.code == "E070")
            .count()
    }

    #[test]
    fn runtime_has_nine_functions_in_order() {
        let funcs = runtime(8);
        let names: Vec<&str> = funcs.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                ".lilac.byte_buffer_from",
                ".lilac.byte_buffer_ptr",
                ".lilac.byte_buffer_len",
                ".lilac.load_u8",
                ".lilac.store_u8",
                ".lilac.format_args_new",
                ".lilac.format_args_push",
                ".lilac.format_args_count",
                ".lilac.format_args_piece",
            ]
        );
        // The two byte-access bodies the backend no longer defines.
        assert!(names.contains(&".lilac.load_u8"));
        assert!(names.contains(&".lilac.store_u8"));
    }

    /// Both `format_args` bounds checks use unsigned less-than, so a negative
    /// (high-bit-set) index cannot slip past `index < count`.
    #[test]
    fn runtime_bounds_checks_are_unsigned() {
        let funcs = runtime(8);
        for name in [".lilac.format_args_push", ".lilac.format_args_piece"] {
            let func = funcs
                .iter()
                .find(|f| f.name == name)
                .expect("runtime function");
            let ops: Vec<&ArithOp> = func
                .blocks
                .iter()
                .flat_map(|b| &b.insts)
                .filter_map(|inst| match inst {
                    LirInst::Arith { op, .. } => Some(op),
                    _ => None,
                })
                .collect();
            assert!(
                ops.iter().any(|op| matches!(op, ArithOp::ILtU)),
                "{name} must bounds-check with ILtU"
            );
            assert!(
                !ops.iter().any(|op| matches!(op, ArithOp::ILt)),
                "{name} must not use signed ILt"
            );
        }
    }

    #[test]
    fn runtime_module_verifies() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-unknown-none").expect("triple"));
        module.funcs = runtime(8);
        let mut diags = lilac_error::Diags::new();
        assert!(
            verify(&module, &mut diags).is_ok(),
            "{:?}",
            diags.diagnostics()
        );
        assert_eq!(e070_count(&diags), 0);
    }

    fn stride_lits(funcs: &[LirFunc], name: &str) -> Vec<LirConst> {
        funcs
            .iter()
            .find(|f| f.name == name)
            .map(|f| {
                f.blocks
                    .iter()
                    .flat_map(|b| &b.insts)
                    .filter_map(|inst| match inst {
                        LirInst::Const { dst, value, ty, .. }
                            if *dst == "stride" && *ty == crate::LirTy::BitNative =>
                        {
                            Some(value.clone())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn runtime_embeds_pointer_stride() {
        for (ptr_size, expected) in [(8u32, 16), (4, 8)] {
            let funcs = runtime(ptr_size);
            for name in [".lilac.format_args_push", ".lilac.format_args_piece"] {
                assert_eq!(
                    stride_lits(&funcs, name),
                    vec![LirConst::Int(expected)],
                    "{name} at ptr_size {ptr_size}"
                );
            }
        }
    }
}
