//! Builds the runtime functions as ordinary LIR.
//!
//! The functions are not injected here; the driver appends `build_runtime` to
//! a module between constant folding and dead-code pruning, where DCE keeps
//! only the ones a program reaches.
//!
//! Besides the byte-buffer and format-args helpers, the runtime holds the OS
//! layer: `.lilac.write`, `.lilac.exit`, `.lilac.alloc` and `.lilac.free`,
//! written as plain calls to `extern` system functions (see [`System`]), and
//! the native process entry `.lilac.start` (see [`entry_point`]).
//!
//! Every place is addressed (the pointer place ABI), so the bodies only need
//! `alloca`/`field`/`field.addr`, `copy`/`store`, `arith`, `branch`, and the
//! `@ptr_add` and `@lilac_abort` leaves. Registers are block-local in the
//! verifier, so each block re-materializes the values it uses.

use lilac_span::Span;

use crate::{
    ArithOp, ConvertOp, LirBlock, LirCallee, LirFunc, LirInst, LirOperand, LirTerm, LirTy,
};

/// How a program enters: which entry symbol wraps the Lilac `main`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Entry {
    /// A C `i32 main()` wrapper, entered through the C runtime's startup code
    /// (`--libc` builds, `check` and `emit-llvm`).
    #[default]
    CMain,
    /// Lilac's own process entry, `.lilac.start` ([`entry_point`]), with no
    /// C runtime.
    Native,
}

/// The system interface the OS layer calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    /// No operating system (bare or unknown targets): the four OS functions
    /// are `extern` declarations for the platform to define, so a program
    /// that reaches one checks but fails to link without it.
    None,
    /// `kernel32.dll`: `GetStdHandle`/`WriteFile`, `GetProcessHeap` with
    /// `HeapAlloc`/`HeapFree`, and `ExitProcess`.
    Windows,
    /// The C library's `write`, `calloc`/`free` and `exit`. This is macOS's
    /// `libSystem`, and Linux's interface until its system-call layer lands.
    Libc,
}

/// The `STD_INPUT_HANDLE` id (`-10` as a `DWORD`). `GetStdHandle` takes
/// `-10 - fd`, so stdout (1) is `-11` and stderr (2) is `-12`.
const STD_HANDLE_BASE: &str = "0xfffffff6";
/// `HEAP_ZERO_MEMORY`.
const HEAP_ZERO_MEMORY: &str = "0x8";

/// Builds the runtime prelude for a target with `ptr_size`-byte pointers,
/// calling `system` for the OS layer. `entry` picks how `.lilac.exit` ends
/// the process: with a C entry it calls the C library's `exit`, which
/// flushes C `stdio` buffers.
///
/// The functions are returned in call-dependency order: `byte_buffer_from`,
/// `byte_buffer_ptr`, `byte_buffer_len`, `load_u8`, `store_u8`,
/// `format_args_new`, `format_args_push`, `format_args_count`,
/// `format_args_piece`, then the OS layer's `extern` declarations and
/// functions. The piece stride is `#byte_buffer`'s size, `2 * ptr_size`
/// bytes.
#[must_use]
pub fn build_runtime(ptr_size: u32, system: System, entry: Entry) -> Vec<LirFunc> {
    let stride = format!("0x{:x}", 2 * ptr_size);
    let mut funcs = vec![
        byte_buffer_from(),
        byte_buffer_ptr(),
        byte_buffer_len(),
        load_u8(),
        store_u8(),
        format_args_new(),
        format_args_push(&stride),
        format_args_count(),
        format_args_piece(&stride),
    ];
    funcs.extend(os_layer(system, entry));
    funcs
}

/// The native process entry `.lilac.start`: calls `main` (with a zeroed
/// place for each of its ins and outs), then exits with code 0 through
/// `.lilac.exit`. The OS passes it nothing the program needs.
#[must_use]
pub fn entry_point(main: &LirFunc) -> LirFunc {
    let mut insts = Vec::new();
    let mut ins = Vec::new();
    let mut outs = Vec::new();
    for (index, (_, ty)) in main.ins.iter().chain(&main.outs).enumerate() {
        let place = format!("arg{index}");
        insts.push(alloca(ty.clone(), &place));
        if index < main.ins.len() {
            ins.push(place);
        } else {
            outs.push(place);
        }
    }
    insts.push(LirInst::Call {
        func: LirCallee::Func(main.name.clone()),
        ins,
        outs,
        span: span(),
    });
    insts.push(alloca(LirTy::Bit(32), "code"));
    insts.push(call(".lilac.exit", &["code"], &[]));
    lir_func(
        ".lilac.start",
        &[],
        &[],
        vec![block("entry", insts, unreachable())],
    )
}

/// The OS layer for `system`: `extern` declarations first, then the four
/// functions the `@os_*` intrinsics call.
fn os_layer(system: System, entry: Entry) -> Vec<LirFunc> {
    let byte_ref = LirTy::byte_ref;
    let c_exit = || extern_func("exit", &[("code", LirTy::Bit(32))], &[]);
    match system {
        System::None => vec![
            extern_func(".lilac.write", &write_sig(), &[]),
            extern_func(".lilac.exit", &[("code", LirTy::Bit(32))], &[]),
            extern_func(
                ".lilac.alloc",
                &[("size", LirTy::BitNative)],
                &[("out", byte_ref())],
            ),
            extern_func(".lilac.free", &[("ptr", byte_ref())], &[]),
        ],
        System::Windows => {
            let mut funcs = vec![
                extern_func(
                    "GetStdHandle",
                    &[("id", LirTy::Bit(32))],
                    &[("handle", byte_ref())],
                ),
                extern_func(
                    "WriteFile",
                    &[
                        ("handle", byte_ref()),
                        ("data", byte_ref()),
                        ("len", LirTy::Bit(32)),
                        ("written", LirTy::Bit(32).to_ref()),
                        ("overlapped", byte_ref()),
                    ],
                    &[("ok", LirTy::Bit(32))],
                ),
                extern_func("GetProcessHeap", &[], &[("heap", byte_ref())]),
                extern_func(
                    "HeapAlloc",
                    &[
                        ("heap", byte_ref()),
                        ("flags", LirTy::Bit(32)),
                        ("size", LirTy::BitNative),
                    ],
                    &[("ptr", byte_ref())],
                ),
                extern_func(
                    "HeapFree",
                    &[
                        ("heap", byte_ref()),
                        ("flags", LirTy::Bit(32)),
                        ("ptr", byte_ref()),
                    ],
                    &[("ok", LirTy::Bit(32))],
                ),
            ];
            let exit = if entry == Entry::Native {
                funcs.push(extern_func("ExitProcess", &[("code", LirTy::Bit(32))], &[]));
                "ExitProcess"
            } else {
                funcs.push(c_exit());
                "exit"
            };
            funcs.extend([
                windows_write(),
                os_exit(exit),
                windows_alloc(),
                windows_free(),
            ]);
            funcs
        }
        System::Libc => vec![
            extern_func(
                "write",
                &[
                    ("fd", LirTy::Bit(32)),
                    ("data", byte_ref()),
                    ("len", LirTy::BitNative),
                ],
                &[("written", LirTy::BitNative)],
            ),
            extern_func(
                "calloc",
                &[("count", LirTy::BitNative), ("size", LirTy::BitNative)],
                &[("ptr", byte_ref())],
            ),
            extern_func("free", &[("ptr", byte_ref())], &[]),
            c_exit(),
            libc_write(),
            os_exit("exit"),
            libc_alloc(),
            libc_free(),
        ],
    }
}

fn span() -> Span {
    Span::dummy()
}

fn alloca(ty: LirTy, place: &str) -> LirInst {
    LirInst::Alloca {
        ty,
        place: place.to_owned(),
        span: span(),
    }
}

fn field(obj: &str, name: &str, idx: u32, dst: &str, ty: LirTy) -> LirInst {
    LirInst::Field {
        obj: obj.to_owned(),
        name: name.to_owned(),
        idx,
        dst: dst.to_owned(),
        ty,
        span: span(),
    }
}

fn field_addr(obj: &str, name: &str, idx: u32, dst: &str, ty: LirTy) -> LirInst {
    LirInst::FieldAddr {
        obj: obj.to_owned(),
        name: name.to_owned(),
        idx,
        dst: dst.to_owned(),
        ty,
        span: span(),
    }
}

fn store_field(value: LirOperand, obj: &str, name: &str, idx: u32) -> LirInst {
    LirInst::StoreField {
        value,
        obj: obj.to_owned(),
        name: name.to_owned(),
        idx,
        span: span(),
    }
}

fn store(src: LirOperand, dst: &str) -> LirInst {
    LirInst::Store {
        src,
        dst: dst.to_owned(),
        span: span(),
    }
}

fn copy_place(src: LirOperand, dst: &str) -> LirInst {
    LirInst::CopyPlace {
        src,
        dst: dst.to_owned(),
        span: span(),
    }
}

fn copy_reg(src: LirOperand, dst: &str, ty: LirTy) -> LirInst {
    LirInst::CopyReg {
        src,
        dst: dst.to_owned(),
        ty,
        span: span(),
    }
}

fn load(src: &str, dst: &str, ty: LirTy) -> LirInst {
    LirInst::Load {
        src: src.to_owned(),
        dst: dst.to_owned(),
        ty,
        span: span(),
    }
}

fn constant(dst: &str, lit: &str, ty: LirTy) -> LirInst {
    LirInst::Const {
        dst: dst.to_owned(),
        lit: lit.to_owned(),
        ty,
        span: span(),
    }
}

fn arith(op: ArithOp, lhs: &str, rhs: &str, dst: &str, ty: LirTy) -> LirInst {
    LirInst::Arith {
        op,
        lhs: lhs.to_owned(),
        rhs: rhs.to_owned(),
        dst: dst.to_owned(),
        ty,
        span: span(),
    }
}

fn call(name: &str, ins: &[&str], outs: &[&str]) -> LirInst {
    LirInst::Call {
        func: LirCallee::Func(name.to_owned()),
        ins: ins.iter().map(|place| (*place).to_owned()).collect(),
        outs: outs.iter().map(|place| (*place).to_owned()).collect(),
        span: span(),
    }
}

fn call_builtin(name: &str, ins: &[&str], outs: &[&str]) -> LirInst {
    LirInst::Call {
        func: LirCallee::Builtin(name.to_owned()),
        ins: ins.iter().map(|place| (*place).to_owned()).collect(),
        outs: outs.iter().map(|place| (*place).to_owned()).collect(),
        span: span(),
    }
}

fn block(label: &str, insts: Vec<LirInst>, term: LirTerm) -> LirBlock {
    LirBlock {
        label: label.to_owned(),
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
        target: target.to_owned(),
        span: span(),
    }
}

fn branch_if(cond: &str, then: &str, els: &str) -> LirTerm {
    LirTerm::BranchIf {
        cond: cond.to_owned(),
        then: then.to_owned(),
        els: Some(els.to_owned()),
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
        name: name.to_owned(),
        ins: ins
            .iter()
            .map(|(place, ty)| ((*place).to_owned(), ty.clone()))
            .collect(),
        outs: outs
            .iter()
            .map(|(place, ty)| ((*place).to_owned(), ty.clone()))
            .collect(),
        is_extern: false,
        inline: false,
        blocks,
    }
}

fn extern_func(name: &str, ins: &[(&str, LirTy)], outs: &[(&str, LirTy)]) -> LirFunc {
    LirFunc {
        is_extern: true,
        ..lir_func(name, ins, outs, Vec::new())
    }
}

/// A place holding the constant `lit`.
fn const_place(place: &str, lit: &str, ty: LirTy) -> [LirInst; 3] {
    let reg = format!("{place}.value");
    [
        alloca(ty.clone(), place),
        constant(&reg, lit, ty),
        copy_place(LirOperand::Reg(reg), place),
    ]
}

fn write_sig() -> [(&'static str, LirTy); 3] {
    [
        ("fd", LirTy::Bit(32)),
        ("data", LirTy::byte_ref()),
        ("len", LirTy::BitNative),
    ]
}

/// `.lilac.write(#bit32 %fd, ref #bit8 %data, #bit_native %len)` on
/// Windows: `WriteFile(GetStdHandle(-10 - fd), data, len, &written, null)`.
/// The length is truncated to a `DWORD`.
fn windows_write() -> LirFunc {
    let insts = vec![
        copy_reg(LirOperand::Place("fd".to_owned()), "fd", LirTy::Bit(32)),
        constant("base", STD_HANDLE_BASE, LirTy::Bit(32)),
        arith(ArithOp::ISub, "base", "fd", "id", LirTy::Bit(32)),
        alloca(LirTy::Bit(32), "idp"),
        copy_place(LirOperand::Reg("id".to_owned()), "idp"),
        alloca(LirTy::byte_ref(), "handle"),
        call("GetStdHandle", &["idp"], &["handle"]),
        copy_reg(LirOperand::Place("len".to_owned()), "len", LirTy::BitNative),
        LirInst::Convert {
            op: ConvertOp::Trunc,
            src: "len".to_owned(),
            dst: "len32".to_owned(),
            ty: LirTy::Bit(32),
            span: span(),
        },
        alloca(LirTy::Bit(32), "lenp"),
        copy_place(LirOperand::Reg("len32".to_owned()), "lenp"),
        alloca(LirTy::Bit(32), "written"),
        LirInst::Addr {
            place: "written".to_owned(),
            dst: "wa".to_owned(),
            ty: LirTy::Bit(32).to_ref(),
            span: span(),
        },
        alloca(LirTy::Bit(32).to_ref(), "wp"),
        copy_place(LirOperand::Reg("wa".to_owned()), "wp"),
        alloca(LirTy::byte_ref(), "null"),
        alloca(LirTy::Bit(32), "ok"),
        call(
            "WriteFile",
            &["handle", "data", "lenp", "wp", "null"],
            &["ok"],
        ),
    ];
    lir_func(
        ".lilac.write",
        &write_sig(),
        &[],
        vec![block("entry", insts, ret())],
    )
}

/// `.lilac.write` over the C library's `write(fd, data, len)`.
fn libc_write() -> LirFunc {
    lir_func(
        ".lilac.write",
        &write_sig(),
        &[],
        vec![block(
            "entry",
            vec![
                alloca(LirTy::BitNative, "written"),
                call("write", &["fd", "data", "len"], &["written"]),
            ],
            ret(),
        )],
    )
}

/// `.lilac.exit(#bit32 %code)`: calls `exit_fn`, which does not return.
fn os_exit(exit_fn: &str) -> LirFunc {
    lir_func(
        ".lilac.exit",
        &[("code", LirTy::Bit(32))],
        &[],
        vec![block(
            "entry",
            vec![call(exit_fn, &["code"], &[])],
            unreachable(),
        )],
    )
}

/// `.lilac.alloc(#bit_native %size) -> ref #bit8` on Windows:
/// `HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, size)`.
fn windows_alloc() -> LirFunc {
    let mut insts = vec![
        alloca(LirTy::byte_ref(), "heap"),
        call("GetProcessHeap", &[], &["heap"]),
    ];
    insts.extend(const_place("flags", HEAP_ZERO_MEMORY, LirTy::Bit(32)));
    insts.push(call("HeapAlloc", &["heap", "flags", "size"], &["out"]));
    lir_func(
        ".lilac.alloc",
        &[("size", LirTy::BitNative)],
        &[("out", LirTy::byte_ref())],
        vec![block("entry", insts, ret())],
    )
}

/// `.lilac.alloc` over the C library's `calloc(1, size)`.
fn libc_alloc() -> LirFunc {
    let mut insts = const_place("count", "0x1", LirTy::BitNative).to_vec();
    insts.push(call("calloc", &["count", "size"], &["out"]));
    lir_func(
        ".lilac.alloc",
        &[("size", LirTy::BitNative)],
        &[("out", LirTy::byte_ref())],
        vec![block("entry", insts, ret())],
    )
}

/// `.lilac.free(ref #bit8 %ptr)` on Windows:
/// `HeapFree(GetProcessHeap(), 0, ptr)`.
fn windows_free() -> LirFunc {
    lir_func(
        ".lilac.free",
        &[("ptr", LirTy::byte_ref())],
        &[],
        vec![block(
            "entry",
            vec![
                alloca(LirTy::byte_ref(), "heap"),
                call("GetProcessHeap", &[], &["heap"]),
                alloca(LirTy::Bit(32), "flags"),
                alloca(LirTy::Bit(32), "ok"),
                call("HeapFree", &["heap", "flags", "ptr"], &["ok"]),
            ],
            ret(),
        )],
    )
}

/// `.lilac.free` over the C library's `free`.
fn libc_free() -> LirFunc {
    lir_func(
        ".lilac.free",
        &[("ptr", LirTy::byte_ref())],
        &[],
        vec![block("entry", vec![call("free", &["ptr"], &[])], ret())],
    )
}

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
                store_field(LirOperand::Place("len".to_owned()), "tmp", "len", 0),
                store_field(LirOperand::Place("data".to_owned()), "tmp", "data", 1),
                copy_place(LirOperand::Place("tmp".to_owned()), "out"),
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
                copy_place(LirOperand::Reg("r".to_owned()), "out"),
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
                copy_place(LirOperand::Reg("r".to_owned()), "out"),
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
                copy_place(LirOperand::Reg("r".to_owned()), "out"),
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
                store(LirOperand::Place("value".to_owned()), "slot"),
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
                copy_place(LirOperand::Place("tmp".to_owned()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.format_args_push(ref #format_args %table, #byte_buffer %piece)`.
///
/// Appends `piece` while `count < 16`, then increments `count`.
fn format_args_push(stride: &str) -> LirFunc {
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
                    constant("cap", "0x10", LirTy::BitNative),
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
                    copy_place(LirOperand::Reg("p0".to_owned()), "base"),
                    constant("stride", stride, LirTy::BitNative),
                    arith(ArithOp::IMul, "c", "stride", "off", LirTy::BitNative),
                    alloca(LirTy::BitNative, "offp"),
                    copy_place(LirOperand::Reg("off".to_owned()), "offp"),
                    alloca(LirTy::byte_ref(), "slot"),
                    call_builtin("@ptr_add", &["base", "offp"], &["slot"]),
                    alloca(LirTy::byte_buffer().to_ref(), "s"),
                    copy_place(LirOperand::Place("slot".to_owned()), "s"),
                    store(LirOperand::Place("piece".to_owned()), "s"),
                    constant("one", "0x1", LirTy::BitNative),
                    arith(ArithOp::IAdd, "c", "one", "n", LirTy::BitNative),
                    store_field(LirOperand::Reg("n".to_owned()), "table", "count", 0),
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
                copy_place(LirOperand::Reg("r".to_owned()), "out"),
            ],
            ret(),
        )],
    )
}

/// `.lilac.format_args_piece(#format_args %table, #bit_native %index) ->
/// #byte_buffer`.
///
/// Reads `p0[index]`, trapping through `@lilac_abort` when `index >= count`.
fn format_args_piece(stride: &str) -> LirFunc {
    lir_func(
        ".lilac.format_args_piece",
        &[("table", LirTy::format_args()), ("index", LirTy::BitNative)],
        &[("out", LirTy::byte_buffer())],
        vec![
            block(
                "entry",
                vec![
                    field("table", "count", 0, "c", LirTy::BitNative),
                    copy_reg(
                        LirOperand::Place("index".to_owned()),
                        "idx",
                        LirTy::BitNative,
                    ),
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
                    copy_reg(
                        LirOperand::Place("index".to_owned()),
                        "idx",
                        LirTy::BitNative,
                    ),
                    field_addr("table", "p0", 1, "p0", LirTy::byte_buffer().to_ref()),
                    alloca(LirTy::byte_ref(), "base"),
                    copy_place(LirOperand::Reg("p0".to_owned()), "base"),
                    constant("stride", stride, LirTy::BitNative),
                    arith(ArithOp::IMul, "idx", "stride", "off", LirTy::BitNative),
                    alloca(LirTy::BitNative, "offp"),
                    copy_place(LirOperand::Reg("off".to_owned()), "offp"),
                    alloca(LirTy::byte_ref(), "slot"),
                    call_builtin("@ptr_add", &["base", "offp"], &["slot"]),
                    alloca(LirTy::byte_buffer().to_ref(), "s"),
                    copy_place(LirOperand::Place("slot".to_owned()), "s"),
                    load("s", "v", LirTy::byte_buffer()),
                    copy_place(LirOperand::Reg("v".to_owned()), "out"),
                ],
                ret(),
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::{build_runtime, entry_point, Entry, System};
    use crate::{verify, ArithOp, LirFunc, LirInst, LirModule};

    fn runtime(ptr_size: u32) -> Vec<LirFunc> {
        build_runtime(ptr_size, System::None, Entry::CMain)
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
        let mut funcs = runtime(8);
        funcs.retain(|f| !f.is_extern);
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
        let mut module = LirModule::new("x86_64-unknown-none");
        module.funcs = runtime(8);
        let mut diags = lilac_error::Diags::new();
        assert!(
            verify(&module, &mut diags).is_ok(),
            "{:?}",
            diags.diagnostics()
        );
        assert_eq!(e070_count(&diags), 0);
    }

    fn names(funcs: &[LirFunc]) -> Vec<&str> {
        funcs.iter().map(|f| f.name.as_str()).collect()
    }

    fn verifies(funcs: Vec<LirFunc>) {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.funcs = funcs;
        let mut diags = lilac_error::Diags::new();
        assert!(
            verify(&module, &mut diags).is_ok(),
            "{:?}",
            diags.diagnostics()
        );
    }

    /// Every OS layer, with both entries and the native start, verifies.
    #[test]
    fn os_layers_verify() {
        let main = LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            inline: false,
            blocks: vec![super::block("entry", Vec::new(), super::ret())],
        };
        for system in [System::Windows, System::Libc] {
            for entry in [Entry::CMain, Entry::Native] {
                let mut funcs = build_runtime(8, system, entry);
                funcs.push(main.clone());
                funcs.push(entry_point(&main));
                verifies(funcs);
            }
        }
    }

    /// Windows exits through `ExitProcess` natively, but through the C
    /// library's `exit` (which flushes `stdio`) under a C entry.
    #[test]
    fn windows_exit_depends_on_entry() {
        let native = build_runtime(8, System::Windows, Entry::Native);
        assert!(names(&native).contains(&"ExitProcess"));
        assert!(!names(&native).contains(&"exit"));
        let c = build_runtime(8, System::Windows, Entry::CMain);
        assert!(names(&c).contains(&"exit"));
        assert!(!names(&c).contains(&"ExitProcess"));
        for funcs in [&native, &c] {
            for name in [".lilac.write", ".lilac.exit", ".lilac.alloc", ".lilac.free"] {
                assert!(names(funcs).contains(&name), "{name}");
            }
        }
    }

    /// With no OS, the OS functions are declared for the platform to define.
    #[test]
    fn no_os_declares_the_os_layer() {
        let funcs = runtime(8);
        let externs: Vec<&str> = funcs
            .iter()
            .filter(|f| f.is_extern)
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(
            externs,
            [".lilac.write", ".lilac.exit", ".lilac.alloc", ".lilac.free"]
        );
    }

    fn stride_lits(funcs: &[LirFunc], name: &str) -> Vec<String> {
        funcs
            .iter()
            .find(|f| f.name == name)
            .map(|f| {
                f.blocks
                    .iter()
                    .flat_map(|b| &b.insts)
                    .filter_map(|inst| match inst {
                        LirInst::Const { dst, lit, ty, .. }
                            if dst == "stride" && *ty == crate::LirTy::BitNative =>
                        {
                            Some(lit.clone())
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn runtime_embeds_pointer_stride() {
        for (ptr_size, expected) in [(8u32, "0x10"), (4, "0x8")] {
            let funcs = runtime(ptr_size);
            for name in [".lilac.format_args_push", ".lilac.format_args_piece"] {
                assert_eq!(
                    stride_lits(&funcs, name),
                    vec![expected.to_owned()],
                    "{name} at ptr_size {ptr_size}"
                );
            }
        }
    }
}
