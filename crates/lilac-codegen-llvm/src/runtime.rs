//! Backend-defined leaf intrinsics.
//!
//! The byte-buffer, format-args, and byte-access runtime is ordinary LIR
//! (`.lilac.*` functions built by `lilac_ir::runtime` and lowered through the
//! normal pointer-ABI function path), so the backend defines only the
//! target-specific leaves: `@ptr_add` (a `getelementptr`), `@memcpy`
//! (`llvm.memcpy`), `@memmove`/`@memset` (`llvm.memmove`/`llvm.memset`),
//! `@memcmp` (C's `memcmp`),
//! `@lilac_abort` (`llvm.trap` + `unreachable`), `@spin_hint` (`pause` or
//! `yield`), `@linux_syscall` (the Linux
//! system-call instruction, as inline assembly), `@trace_head` (the
//! address of the shadow-stack head global) and `@panic_depth` (the address
//! of the panic depth). Each uses the value ABI from
//! `lilac_ir::builtin_sig`.

use inkwell::builder::Builder;
use inkwell::module::Linkage;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, ValueKind};
use inkwell::ThreadLocalMode;

use lilac_error::bug;
use lilac_span::Span;
use lilac_target::{Arch, Os};

use crate::lower::{checked, Lower};
use crate::threads::append_ir;

/// Emits each leaf runtime symbol a module actually calls, in the fixed order.
///
/// The definitions must precede `lower_func` so `lower_builtin_call` reuses
/// them; a module that calls no leaf emits none, and the `llvm.trap`-backed
/// `.lilac.abort` is emitted when `@lilac_abort` needs it.
pub(crate) fn emit_runtime(lower: &Lower<'_, '_>) {
    let used = lower.used_builtins();
    if used.contains("@memcpy") {
        memcpy(lower);
    }
    if used.contains("@memmove") {
        memmove(lower);
    }
    if used.contains("@memset") {
        memset(lower);
    }
    if used.contains("@memcmp") {
        memcmp(lower);
    }
    if used.contains("@ptr_add") {
        ptr_add(lower);
    }
    if used.contains("@lilac_abort") {
        abort(lower);
    }
    if used.contains("@linux_syscall") {
        syscall(lower);
    }
    if used.contains("@spin_hint") {
        spin_hint(lower);
    }
    if used.contains("@trace_head") {
        trace_head(lower);
    }
    if used.contains("@panic_depth") {
        panic_depth(lower);
    }
    if used.contains("@linux_thread_clone") {
        crate::threads::thread_clone(lower);
    }
}

/// `@trace_head() -> ref ref #.lilac.frame`: the address of the shadow-stack
/// head (`lilac_ir::trace`), a zeroed pointer-sized internal global. Each
/// thread has its own on an operating system (`thread_local`); Linux uses
/// the local-exec model, which a static executable with no C library
/// supports ([`crate::threads::main_tls`]).
fn trace_head(lower: &Lower<'_, '_>) {
    let ptr = lower.ptr_type();
    thread_slot(lower, ".lilac.trace_head", ptr.into());
}

/// `@panic_depth() -> ref #bit_native`: the address of the thread's panic
/// depth (`lilac_ir::panic_runtime`), a zeroed native integer kept like
/// the trace head.
fn panic_depth(lower: &Lower<'_, '_>) {
    thread_slot(lower, ".lilac.panic_depth", lower.native_int().into());
}

/// Defines `name`, returning the address of `name.slot`, a zeroed internal
/// global of `ty`, one per thread on an operating system.
fn thread_slot<'ctx>(lower: &Lower<'ctx, '_>, name: &str, ty: BasicTypeEnum<'ctx>) {
    let ptr = lower.ptr_type();
    let slot = lower.module().add_global(ty, None, &format!("{name}.slot"));
    slot.set_linkage(Linkage::Internal);
    slot.set_initializer(&ty.const_zero());
    match lower.os() {
        Some(Os::Linux) => {
            slot.set_thread_local_mode(Some(ThreadLocalMode::LocalExecTLSModel));
        }
        Some(Os::Windows | Os::Macos) => slot.set_thread_local(true),
        _ => {}
    }

    let function = lower.runtime_fn(name, ptr.fn_type(&[], false));
    let builder = entry(lower, function);
    checked(
        builder.build_return(Some(&slot.as_pointer_value())),
        "return thread slot",
    );
}

/// Reads the `index`th parameter of a runtime function, raising an ICE when absent.
pub(crate) fn param<'ctx>(
    function: FunctionValue<'ctx>,
    index: u32,
    name: &str,
) -> BasicValueEnum<'ctx> {
    match function.get_nth_param(index) {
        Some(value) => value,
        None => bug!(
            Span::dummy(),
            "runtime `@{name}` is missing parameter {index}"
        ),
    }
}

/// Creates a builder positioned at a fresh `entry` block of `function`.
pub(crate) fn entry<'ctx>(lower: &Lower<'ctx, '_>, function: FunctionValue<'ctx>) -> Builder<'ctx> {
    let builder = lower.context().create_builder();
    let block = lower.context().append_basic_block(function, "entry");
    builder.position_at_end(block);
    builder
}

/// `@memcpy(ref i8, ref i8, iNative)`: `llvm.memcpy`.
///
/// The emitted symbol is `.lilac.memcpy`, not `memcpy`: the in-module
/// definition must not collide with the host C library's `memcpy` at link time
/// (and the `llvm.memcpy` inside must lower to the C library, not recurse into
/// itself). `lower::builtin_symbol` maps the LIR builtin name `@memcpy` here.
fn memcpy(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let ptr = lower.ptr_type();
    let void = lower.context().void_type();
    let params = [
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(native),
    ];
    let function = lower.runtime_fn(".lilac.memcpy", void.fn_type(&params, false));
    let builder = entry(lower, function);
    let dest = param(function, 0, "memcpy").into_pointer_value();
    let src = param(function, 1, "memcpy").into_pointer_value();
    let len = param(function, 2, "memcpy").into_int_value();
    dest.set_name("dest");
    src.set_name("src");
    len.set_name("len");
    checked(builder.build_memcpy(dest, 1, src, 1, len), "llvm.memcpy");
    checked(builder.build_return(None), "return void");
}

/// `@memmove(ref i8, ref i8, iNative)`: `llvm.memmove`, under
/// `.lilac.memmove` for the same reason as [`memcpy`].
fn memmove(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let ptr = lower.ptr_type();
    let void = lower.context().void_type();
    let params = [
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(native),
    ];
    let function = lower.runtime_fn(".lilac.memmove", void.fn_type(&params, false));
    let builder = entry(lower, function);
    let dest = param(function, 0, "memmove").into_pointer_value();
    let src = param(function, 1, "memmove").into_pointer_value();
    let len = param(function, 2, "memmove").into_int_value();
    checked(builder.build_memmove(dest, 1, src, 1, len), "llvm.memmove");
    checked(builder.build_return(None), "return void");
}

/// `@memset(ref i8, i8, iNative)`: `llvm.memset`, under `.lilac.memset`.
fn memset(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let ptr = lower.ptr_type();
    let byte = lower.context().i8_type();
    let void = lower.context().void_type();
    let params = [
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(byte),
        BasicMetadataTypeEnum::from(native),
    ];
    let function = lower.runtime_fn(".lilac.memset", void.fn_type(&params, false));
    let builder = entry(lower, function);
    let dest = param(function, 0, "memset").into_pointer_value();
    let value = param(function, 1, "memset").into_int_value();
    let len = param(function, 2, "memset").into_int_value();
    checked(builder.build_memset(dest, 1, value, len), "llvm.memset");
    checked(builder.build_return(None), "return void");
}

/// `@ptr_add(ref i8, iNative) -> ref i8`: `base + offset`, a plain
/// `getelementptr i8`, so the result keeps `base`'s provenance and may
/// wrap or leave its object as integer arithmetic could. inkwell's GEP
/// builders are `unsafe`, so it is linked in as IR text.
/// `lower::builtin_symbol` maps the LIR builtin name `@ptr_add` here.
fn ptr_add(lower: &Lower<'_, '_>) {
    let native = lower.native_int().get_bit_width();
    let text = format!(
        "define ptr @.lilac.ptr_add(ptr %base, i{native} %offset) alwaysinline nounwind {{\n\
         entry:\n  \
         %address = getelementptr i8, ptr %base, i{native} %offset\n  \
         ret ptr %address\n\
         }}\n"
    );
    append_ir(lower.module(), ".lilac.ptr_add", &text);
}

/// Defines `.lilac.offset(ptr %base, iNative %offset) -> ptr`, an internal,
/// always-inline `getelementptr inbounds i8`: a field's or a bounds-checked
/// element's address, which stays in its object (`Lower::offset_ptr`).
pub(crate) fn offset_helper<'ctx>(lower: &Lower<'ctx, '_>) -> FunctionValue<'ctx> {
    let native = lower.native_int().get_bit_width();
    let text = format!(
        "define ptr @.lilac.offset(ptr %base, i{native} %offset) alwaysinline nounwind {{\n\
         entry:\n  \
         %address = getelementptr inbounds i8, ptr %base, i{native} %offset\n  \
         ret ptr %address\n\
         }}\n"
    );
    // Linked as an external definition (the linker drops an unreferenced
    // internal one), then made internal.
    append_ir(lower.module(), ".lilac.offset", &text);
    match lower.module().get_function(".lilac.offset") {
        Some(function) => {
            function.set_linkage(Linkage::Internal);
            function
        }
        None => bug!(Span::dummy(), "`.lilac.offset` was not defined"),
    }
}

/// `@memcmp(ref i8, ref i8, iNative) -> i32`: a declaration of C's
/// `memcmp`, which aggregate `==` calls. The optimizer expands a small
/// constant-size one inline; otherwise the C library defines it, or the
/// object itself in a native build ([`crate::native`]).
fn memcmp(lower: &Lower<'_, '_>) {
    let ptr = lower.ptr_type();
    let params = [
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(lower.native_int()),
    ];
    let int = lower.context().i32_type();
    lower.runtime_fn("memcmp", int.fn_type(&params, false));
}

/// `@.lilac.abort()`: `llvm.trap` then `unreachable`.
fn abort(lower: &Lower<'_, '_>) {
    let void = lower.context().void_type();
    let trap = lower.runtime_fn("llvm.trap", void.fn_type(&[], false));
    let function = lower.runtime_fn(".lilac.abort", void.fn_type(&[], false));
    let builder = entry(lower, function);
    checked(builder.build_call(trap, &[], ""), "trap call");
    checked(builder.build_unreachable(), "abort unreachable");
}

/// `@spin_hint()`: the spin-loop hint, always inline: `pause` on x86,
/// `yield` on 64-bit Arm, and nothing elsewhere (32-bit Arm's `yield` needs
/// `ARMv6K` and RISC-V's `pause` an extension the baseline does not assume).
fn spin_hint(lower: &Lower<'_, '_>) {
    let (declare, call) = match lower.arch() {
        Some(Arch::X86 | Arch::X86_64) => (
            "declare void @llvm.x86.sse2.pause()
",
            "  call void @llvm.x86.sse2.pause()
",
        ),
        Some(Arch::Aarch64) => (
            "declare void @llvm.aarch64.hint(i32)
",
            "  call void @llvm.aarch64.hint(i32 1)
",
        ),
        _ => ("", ""),
    };
    let text = format!(
        "{declare}define void @.lilac.spin_hint() alwaysinline nounwind {{
         entry:
         {call}  ret void
         }}
"
    );
    append_ir(lower.module(), ".lilac.spin_hint", &text);
}

/// `@linux_syscall(n, a0..a5) -> iNative`: the Linux system-call instruction, with
/// the number and six arguments in the kernel's registers and the result
/// (or `-errno`) in the first. Only 64-bit x86 and Arm have one.
fn syscall(lower: &Lower<'_, '_>) {
    let (assembly, constraints) = match lower.arch() {
        Some(Arch::X86_64) => (
            "syscall",
            "={rax},{rax},{rdi},{rsi},{rdx},{r10},{r8},{r9},~{rcx},~{r11},~{memory}",
        ),
        Some(Arch::Aarch64) => (
            "svc #0",
            "={x0},{x8},{x0},{x1},{x2},{x3},{x4},{x5},~{memory}",
        ),
        other => bug!(
            Span::dummy(),
            "`@linux_syscall` has no instruction for {other:?}"
        ),
    };
    let native = lower.native_int();
    let params = [BasicMetadataTypeEnum::from(native); 7];
    let fn_type = native.fn_type(&params, false);
    let function = lower.runtime_fn(".lilac.linux_syscall", fn_type);
    let builder = entry(lower, function);
    let instruction = lower.context().create_inline_asm(
        fn_type,
        assembly.to_owned(),
        constraints.to_owned(),
        true,
        false,
        None,
        false,
    );
    let args: Vec<BasicMetadataValueEnum<'_>> = (0..7)
        .map(|index| param(function, index, "syscall").into())
        .collect();
    let call = checked(
        builder.build_indirect_call(fn_type, instruction, &args, "result"),
        "syscall",
    );
    let ValueKind::Basic(result) = call.try_as_basic_value() else {
        bug!(Span::dummy(), "`@linux_syscall` returned void");
    };
    checked(builder.build_return(Some(&result)), "return syscall");
}
