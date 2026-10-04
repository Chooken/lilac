//! In-module LLVM runtime for the builtin aggregate and memory operations.
//!
//! Task 5 emits one definition per runtime symbol, in the fixed order the M5b
//! spec pins, using the value ABI from `lilac_ir::builtin_sig` (not the
//! pointer-based place ABI used for user functions). Emitting the runtime
//! before user functions lets Task 4's `LirCallee::Builtin` calls reuse these
//! definitions instead of re-declaring them.
//!
//! `#byte_buffer` is `{ iNative, ptr }` and `#format_args` is
//! `{ iNative, [16 x { iNative, ptr }] }`. Because inkwell only exposes dynamic
//! array indexing through an `unsafe` GEP (and this crate forbids `unsafe`),
//! the two dynamic piece-table accesses compute the element address with safe
//! `ptrtoint`/`inttoptr` integer arithmetic. The piece stride is
//! `2 * sizeof(iNative)` because both struct members are pointer-width.

use inkwell::builder::Builder;
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::IntPredicate;

use lilac_error::bug;
use lilac_span::Span;

use crate::lower::{checked, Lower};

/// Emits every runtime symbol exactly once, in the fixed order.
pub(crate) fn emit_runtime(lower: &Lower<'_, '_>) {
    byte_buffer_from(lower);
    byte_buffer_ptr(lower);
    byte_buffer_len(lower);
    load_u8(lower);
    store_u8(lower);
    memcpy(lower);
    format_args_new(lower);
    format_args_push(lower);
    format_args_count(lower);
    format_args_piece(lower);
    abort(lower);
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

/// `@byte_buffer_from(ref i8, iNative) -> { iNative, ptr }`.
fn byte_buffer_from(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let ptr = lower.ptr_type();
    let buffer_ty = lower.byte_buffer_type();
    let params = [
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(native),
    ];
    let function = lower.runtime_fn("byte_buffer_from", buffer_ty.fn_type(&params, false));
    let builder = entry(lower, function);
    let data = param(function, 0, "byte_buffer_from").into_pointer_value();
    let len = param(function, 1, "byte_buffer_from").into_int_value();
    data.set_name("data");
    len.set_name("len");
    let with_len = checked(
        builder.build_insert_value(buffer_ty.get_undef(), len, 0, ""),
        "buffer len",
    );
    let buffer = checked(
        builder.build_insert_value(with_len, data, 1, ""),
        "buffer ptr",
    );
    checked(builder.build_return(Some(&buffer)), "return byte_buffer");
}

/// `@byte_buffer_ptr({ iNative, ptr }) -> ref i8`.
fn byte_buffer_ptr(lower: &Lower<'_, '_>) {
    let ptr = lower.ptr_type();
    let buffer_ty = lower.byte_buffer_type();
    let params = [BasicMetadataTypeEnum::from(buffer_ty)];
    let function = lower.runtime_fn("byte_buffer_ptr", ptr.fn_type(&params, false));
    let builder = entry(lower, function);
    let buffer = param(function, 0, "byte_buffer_ptr");
    buffer.set_name("buffer");
    let data = checked(
        builder.build_extract_value(buffer.into_struct_value(), 1, ""),
        "buffer ptr",
    );
    checked(builder.build_return(Some(&data)), "return buffer ptr");
}

/// `@byte_buffer_len({ iNative, ptr }) -> iNative`.
fn byte_buffer_len(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let buffer_ty = lower.byte_buffer_type();
    let params = [BasicMetadataTypeEnum::from(buffer_ty)];
    let function = lower.runtime_fn("byte_buffer_len", native.fn_type(&params, false));
    let builder = entry(lower, function);
    let buffer = param(function, 0, "byte_buffer_len");
    buffer.set_name("buffer");
    let len = checked(
        builder.build_extract_value(buffer.into_struct_value(), 0, ""),
        "buffer len",
    );
    checked(builder.build_return(Some(&len)), "return buffer len");
}

/// `@load_u8(ref i8, iNative) -> i8`: byte-address + load.
fn load_u8(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let ptr = lower.ptr_type();
    let byte = lower.context().i8_type();
    let params = [
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(native),
    ];
    let function = lower.runtime_fn("load_u8", byte.fn_type(&params, false));
    let builder = entry(lower, function);
    let data = param(function, 0, "load_u8").into_pointer_value();
    let index = param(function, 1, "load_u8").into_int_value();
    data.set_name("data");
    index.set_name("index");
    let address = byte_ptr(lower, &builder, data, index);
    let value = checked(builder.build_load(byte, address, ""), "load_u8 load");
    checked(builder.build_return(Some(&value)), "return u8");
}

/// `@store_u8(ref i8, iNative, i8)`: byte-address + store.
fn store_u8(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let ptr = lower.ptr_type();
    let byte = lower.context().i8_type();
    let void = lower.context().void_type();
    let params = [
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(native),
        BasicMetadataTypeEnum::from(byte),
    ];
    let function = lower.runtime_fn("store_u8", void.fn_type(&params, false));
    let builder = entry(lower, function);
    let data = param(function, 0, "store_u8").into_pointer_value();
    let index = param(function, 1, "store_u8").into_int_value();
    let value = param(function, 2, "store_u8");
    data.set_name("data");
    index.set_name("index");
    value.set_name("value");
    let address = byte_ptr(lower, &builder, data, index);
    checked(builder.build_store(address, value), "store_u8 store");
    checked(builder.build_return(None), "return void");
}

/// `@memcpy(ref i8, ref i8, iNative)`: `llvm.memcpy`.
fn memcpy(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let ptr = lower.ptr_type();
    let void = lower.context().void_type();
    let params = [
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(native),
    ];
    let function = lower.runtime_fn("memcpy", void.fn_type(&params, false));
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

/// `@format_args_new() -> #format_args`: a zeroed piece table.
fn format_args_new(lower: &Lower<'_, '_>) {
    let table_ty = lower.format_args_type();
    let function = lower.runtime_fn("format_args_new", table_ty.fn_type(&[], false));
    let builder = entry(lower, function);
    checked(
        builder.build_return(Some(&table_ty.const_zero())),
        "return format_args",
    );
}

/// `@format_args_push(ref #format_args, #byte_buffer)`: append while `count < 16`.
fn format_args_push(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let ptr = lower.ptr_type();
    let table_ty = lower.format_args_type();
    let buffer_ty = lower.byte_buffer_type();
    let void = lower.context().void_type();
    let params = [
        BasicMetadataTypeEnum::from(ptr),
        BasicMetadataTypeEnum::from(buffer_ty),
    ];
    let function = lower.runtime_fn("format_args_push", void.fn_type(&params, false));

    let builder = lower.context().create_builder();
    let entry_block = lower.context().append_basic_block(function, "entry");
    let append_block = lower.context().append_basic_block(function, "append");
    let done_block = lower.context().append_basic_block(function, "done");

    builder.position_at_end(entry_block);
    let table = param(function, 0, "format_args_push").into_pointer_value();
    let piece = param(function, 1, "format_args_push");
    table.set_name("table");
    piece.set_name("piece");
    let count_ptr = checked(
        builder.build_struct_gep(table_ty, table, 0, ""),
        "count gep",
    );
    let count = checked(builder.build_load(native, count_ptr, ""), "count load").into_int_value();
    let has_room = checked(
        builder.build_int_compare(IntPredicate::ULT, count, native.const_int(16, false), ""),
        "capacity check",
    );
    checked(
        builder.build_conditional_branch(has_room, append_block, done_block),
        "capacity branch",
    );

    builder.position_at_end(append_block);
    let pieces = checked(
        builder.build_struct_gep(table_ty, table, 1, ""),
        "pieces gep",
    );
    let slot = piece_ptr(lower, &builder, pieces, count);
    checked(builder.build_store(slot, piece), "piece store");
    let next = checked(
        builder.build_int_add(count, native.const_int(1, false), ""),
        "count increment",
    );
    checked(builder.build_store(count_ptr, next), "count store");
    checked(
        builder.build_unconditional_branch(done_block),
        "append branch",
    );

    builder.position_at_end(done_block);
    checked(builder.build_return(None), "return void");
}

/// `@format_args_count(#format_args) -> iNative`: return `count`.
fn format_args_count(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let table_ty = lower.format_args_type();
    let params = [BasicMetadataTypeEnum::from(table_ty)];
    let function = lower.runtime_fn("format_args_count", native.fn_type(&params, false));
    let builder = entry(lower, function);
    let table = param(function, 0, "format_args_count");
    table.set_name("table");
    let count = checked(
        builder.build_extract_value(table.into_struct_value(), 0, ""),
        "count extract",
    );
    checked(builder.build_return(Some(&count)), "return count");
}

/// `@format_args_piece(#format_args, iNative) -> #byte_buffer`: return `pieces[i]`.
fn format_args_piece(lower: &Lower<'_, '_>) {
    let native = lower.native_int();
    let table_ty = lower.format_args_type();
    let buffer_ty = lower.byte_buffer_type();
    let params = [
        BasicMetadataTypeEnum::from(table_ty),
        BasicMetadataTypeEnum::from(native),
    ];
    let function = lower.runtime_fn("format_args_piece", buffer_ty.fn_type(&params, false));
    let builder = entry(lower, function);
    let table = param(function, 0, "format_args_piece");
    let index = param(function, 1, "format_args_piece").into_int_value();
    table.set_name("table");
    index.set_name("index");
    let slot = checked(builder.build_alloca(table_ty, ""), "format_args spill");
    checked(builder.build_store(slot, table), "format_args spill store");
    let pieces = checked(
        builder.build_struct_gep(table_ty, slot, 1, ""),
        "pieces gep",
    );
    let element = piece_ptr(lower, &builder, pieces, index);
    let value = checked(builder.build_load(buffer_ty, element, ""), "piece load");
    checked(builder.build_return(Some(&value)), "return piece");
}

/// `@lilac_abort()`: `llvm.trap` then `unreachable`.
fn abort(lower: &Lower<'_, '_>) {
    let void = lower.context().void_type();
    let trap = lower.runtime_fn("llvm.trap", void.fn_type(&[], false));
    let function = lower.runtime_fn("lilac_abort", void.fn_type(&[], false));
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

/// The address of `pieces[index]`, whose stride is `2 * sizeof(iNative)`.
fn piece_ptr<'ctx>(
    lower: &Lower<'ctx, '_>,
    builder: &Builder<'ctx>,
    pieces: PointerValue<'ctx>,
    index: IntValue<'ctx>,
) -> PointerValue<'ctx> {
    let native = lower.native_int();
    let stride = native.const_int(u64::from(native.get_bit_width()) / 8 * 2, false);
    let offset = checked(builder.build_int_mul(index, stride, ""), "piece offset");
    byte_ptr(lower, builder, pieces, offset)
}
