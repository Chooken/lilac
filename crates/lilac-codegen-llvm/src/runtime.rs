//! Backend-defined leaf intrinsics.
//!
//! The byte-buffer, format-args, and byte-access runtime is ordinary LIR
//! (`.lilac.*` functions built by `lilac_ir::runtime` and lowered through the
//! normal pointer-ABI function path), so the backend defines only the
//! target-specific leaves: `@ptr_add`, `@memcpy` (`llvm.memcpy`),
//! `@lilac_abort` (`llvm.trap` + `unreachable`), `@syscall` (the Linux
//! system-call instruction, as inline assembly), and `@trace_head` (the
//! address of the shadow-stack head global). Each uses the value ABI from
//! `lilac_ir::builtin_sig`.

use inkwell::builder::Builder;
use inkwell::module::Linkage;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FunctionValue, IntValue, PointerValue, ValueKind,
};

use lilac_error::bug;
use lilac_span::Span;
use lilac_target::Arch;

use crate::lower::{checked, Lower};

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
    if used.contains("@ptr_add") {
        ptr_add(lower);
    }
    if used.contains("@lilac_abort") {
        abort(lower);
    }
    if used.contains("@syscall") {
        syscall(lower);
    }
    if used.contains("@trace_head") {
        trace_head(lower);
    }
}

/// `@trace_head() -> ref ref #.lilac.frame`: the address of the shadow-stack
/// head (`lilac_ir::trace`), a zeroed pointer-sized internal global.
fn trace_head(lower: &Lower<'_, '_>) {
    let ptr = lower.ptr_type();
    let slot = lower
        .module()
        .add_global(ptr, None, ".lilac.trace_head.slot");
    slot.set_linkage(Linkage::Internal);
    slot.set_initializer(&ptr.const_null());
    let function = lower.runtime_fn(".lilac.trace_head", ptr.fn_type(&[], false));
    let builder = entry(lower, function);
    checked(
        builder.build_return(Some(&slot.as_pointer_value())),
        "return trace head",
    );
}

/// Reads the `index`th parameter of a runtime function, raising an ICE when absent.
fn param<'ctx>(function: FunctionValue<'ctx>, index: u32, name: &str) -> BasicValueEnum<'ctx> {
    match function.get_nth_param(index) {
        Some(value) => value,
        None => bug!(
            Span::dummy(),
            "runtime `@{name}` is missing parameter {index}"
        ),
    }
}

/// Creates a builder positioned at a fresh `entry` block of `function`.
fn entry<'ctx>(lower: &Lower<'ctx, '_>, function: FunctionValue<'ctx>) -> Builder<'ctx> {
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

/// `@ptr_add(ref i8, iNative) -> ref i8`: byte-addressed pointer arithmetic.
///
/// The body is `ptrtoint(base) + offset` then `inttoptr`, the same safe
/// integer-arithmetic lowering `byte_ptr` uses in place of a dynamic GEP.
/// `lower::builtin_symbol` maps the LIR builtin name `@ptr_add` here.
fn ptr_add(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let ptr = lower.ptr_type();
    let params = [
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(native),
    ];
    let function = lower.runtime_fn(".lilac.ptr_add", ptr.fn_type(&params, false));
    let builder = entry(lower, function);
    let base = param(function, 0, "ptr_add").into_pointer_value();
    let offset = param(function, 1, "ptr_add").into_int_value();
    base.set_name("base");
    offset.set_name("offset");
    let address = byte_ptr(lower, &builder, base, offset);
    checked(builder.build_return(Some(&address)), "return ptr_add");
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

/// `@syscall(n, a0..a5) -> iNative`: the Linux system-call instruction, with
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
        other => bug!(Span::dummy(), "`@syscall` has no instruction for {other:?}"),
    };
    let native = lower.native_int();
    let params = [BasicMetadataTypeEnum::from(native); 7];
    let fn_type = native.fn_type(&params, false);
    let function = lower.runtime_fn(".lilac.syscall", fn_type);
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
        bug!(Span::dummy(), "`@syscall` returned void");
    };
    checked(builder.build_return(Some(&result)), "return syscall");
}

/// Byte-addressed pointer `base + offset`, using safe integer arithmetic in
/// place of a dynamic GEP.
fn byte_ptr<'ctx>(
    lower: &Lower<'ctx, '_>,
    builder: &Builder<'ctx>,
    base: PointerValue<'ctx>,
    offset: IntValue<'ctx>,
) -> PointerValue<'ctx> {
    let native = lower.native_int();
    let base_address = checked(
        builder.build_ptr_to_int(base, native, ""),
        "byte base address",
    );
    let address = checked(
        builder.build_int_add(base_address, offset, ""),
        "byte address",
    );
    checked(
        builder.build_int_to_ptr(address, lower.ptr_type(), ""),
        "byte pointer",
    )
}
