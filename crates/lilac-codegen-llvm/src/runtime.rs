//! Backend-defined leaf intrinsics.
//!
//! The byte-buffer, format-args, and byte-access runtime is ordinary LIR
//! (`.lilac.*` functions built by `lilac_ir::runtime` and lowered through the
//! normal pointer-ABI function path), so the backend defines only the three
//! target-specific leaves: `@ptr_add`, `@memcpy` (`llvm.memcpy`), and
//! `@lilac_abort` (`llvm.trap` + `unreachable`). Each uses the value ABI from
//! `lilac_ir::builtin_sig`.

use inkwell::builder::Builder;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};

use lilac_error::bug;
use lilac_span::Span;

use crate::lower::{checked, Lower};

/// Emits each leaf runtime symbol a module actually calls, in the fixed order.
///
/// The definitions must precede `lower_func` so `lower_builtin_call` reuses
/// them; a module that calls no leaf emits none, and the `llvm.trap`-backed
/// `.lilac.abort` is emitted when `@lilac_abort` or a `panic` terminator needs
/// it.
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
