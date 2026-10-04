//! LIR → LLVM lowering: types, layouts, data globals, places, and the value,
//! memory, conversion, constant, and arithmetic instruction bodies.
//!
//! M5b Tasks 1–3 cover the type map, named layouts, data globals, the pointer
//! calling convention, zero-initialized `alloca` places, and every value /
//! memory / conversion / constant / arithmetic instruction. Calls, indirect
//! calls, `drop`, control flow, and the non-`return` terminators land in
//! Task 4; the in-module builtin runtime lands in Task 5.

use std::collections::HashMap;

use inkwell::basic_block::BasicBlock;
use inkwell::builder::{Builder, BuilderError};
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::{BasicTypeEnum, FloatType, IntType, PointerType, StructType};
use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue};
use inkwell::{AddressSpace, FloatPredicate, IntPredicate};

use lilac_error::bug;
use lilac_ir::{ArithOp, ConvertOp, LirDataKind, LirFunc, LirInst, LirModule, LirOperand, LirTerm};
use lilac_span::Span;

/// Turns a fallible inkwell builder call into an ICE.
fn checked<T>(result: Result<T, BuilderError>, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(err) => bug!(Span::dummy(), "failed to build {what}: {err}"),
    }
}

/// Per-function lowering state: every place's pointer and declared LIR type,
/// plus the SSA value bound to each `&` register.
struct FnState<'ctx> {
    places: HashMap<String, PointerValue<'ctx>>,
    place_types: HashMap<String, String>,
    regs: HashMap<String, BasicValueEnum<'ctx>>,
}

/// One lowering context: an LLVM module plus the LIR being lowered into it.
pub(crate) struct Lower<'ctx, 'a> {
    context: &'ctx Context,
    module: Module<'ctx>,
    lir: &'a LirModule,
    native: IntType<'ctx>,
    named: HashMap<String, StructType<'ctx>>,
}

impl<'ctx, 'a> Lower<'ctx, 'a> {
    /// Creates a lowering context over `module`, picking `iNative` from
    /// `is_64_bit` (the backend's triple drives this).
    pub(crate) fn new(
        context: &'ctx Context,
        module: Module<'ctx>,
        lir: &'a LirModule,
        is_64_bit: bool,
    ) -> Self {
        let native = if is_64_bit {
            context.i64_type()
        } else {
            context.i32_type()
        };
        Self {
            context,
            module,
            lir,
            native,
            named: HashMap::new(),
        }
    }

    /// Consumes the context, returning the populated LLVM module.
    pub(crate) fn into_module(self) -> Module<'ctx> {
        self.module
    }

    /// Lowers layouts, data globals, and functions into the module, in that
    /// fixed order.
    pub(crate) fn run(&mut self) {
        self.define_layouts();
        self.define_data();
        for func in &self.lir.funcs {
            self.lower_func(func);
        }
    }

    /// Maps a LIR type spelling to its LLVM type.
    pub(crate) fn lir_type(&self, ty: &str) -> BasicTypeEnum<'ctx> {
        if ty.starts_with("ref ") {
            return self.ptr_type().into();
        }
        match ty {
            "#bit8" => self.context.i8_type().into(),
            "#bit16" => self.context.i16_type().into(),
            "#bit32" => self.context.i32_type().into(),
            "#bit64" => self.context.i64_type().into(),
            "#bitNative" => self.native.into(),
            "#ptr" => self.ptr_type().into(),
            "#byte_buffer" => self.byte_buffer_type().into(),
            "#format_args" => self.format_args_type().into(),
            "#str" | "#numlit" => bug!(
                Span::dummy(),
                "data-only type `{ty}` has no LLVM value type"
            ),
            other => match self.named.get(other) {
                Some(struct_ty) => (*struct_ty).into(),
                None => bug!(Span::dummy(), "unknown LIR type `{other}`"),
            },
        }
    }

    /// `{len: iNative, ptr}`.
    fn byte_buffer_type(&self) -> StructType<'ctx> {
        self.context
            .struct_type(&[self.native.into(), self.ptr_type().into()], false)
    }

    /// `{count: iNative, pieces: [16 x {len: iNative, ptr}]}`.
    fn format_args_type(&self) -> StructType<'ctx> {
        let piece = self
            .context
            .struct_type(&[self.native.into(), self.ptr_type().into()], false);
        let table = piece.array_type(16);
        self.context
            .struct_type(&[self.native.into(), table.into()], false)
    }

    /// The opaque `ptr` type used for every place under the pointer ABI.
    fn ptr_type(&self) -> PointerType<'ctx> {
        self.context.ptr_type(AddressSpace::default())
    }

    /// Defines one named LLVM struct per LIR layout, in sorted order so printed
    /// IR is deterministic. Opaque shells are created first so fields may refer
    /// to any layout regardless of name order.
    fn define_layouts(&mut self) {
        let mut names: Vec<&String> = self.lir.layouts.keys().collect();
        names.sort_unstable();
        for name in &names {
            let llvm_name = name.trim_start_matches('#');
            let struct_ty = self.context.opaque_struct_type(llvm_name);
            self.named.insert((*name).clone(), struct_ty);
        }
        for name in &names {
            let fields: Vec<BasicTypeEnum<'ctx>> = self
                .lir
                .layouts
                .get(name.as_str())
                .map(|members| members.iter().map(|(_, ty)| self.lir_type(ty)).collect())
                .unwrap_or_default();
            if let Some(struct_ty) = self.named.get(name.as_str()).copied() {
                if !struct_ty.set_body(&fields, false) {
                    bug!(Span::dummy(), "failed to define layout `{name}`");
                }
            }
        }
    }

    /// Emits each `!c` data constant as a private `[N x i8]` global, in
    /// declaration order.
    fn define_data(&self) {
        for datum in &self.lir.data {
            let bytes = match &datum.kind {
                LirDataKind::Str(text) | LirDataKind::NumLit(text) => text.as_bytes(),
            };
            let i8_ty = self.context.i8_type();
            let array_ty = i8_ty.array_type(u32::try_from(bytes.len()).unwrap_or(u32::MAX));
            let init = if bytes.is_empty() {
                array_ty.const_zero()
            } else {
                let values: Vec<_> = bytes
                    .iter()
                    .map(|byte| i8_ty.const_int(u64::from(*byte), false))
                    .collect();
                i8_ty.const_array(&values)
            };
            let global = self.module.add_global(array_ty, None, &datum.name);
            global.set_linkage(Linkage::Private);
            global.set_constant(true);
            global.set_unnamed_addr(true);
            global.set_initializer(&init);
        }
    }

    /// Lowers one function: `define void @name(ptr %in0, …, ptr %out0, …)`, its
    /// signature places bound to the incoming pointer arguments, then its
    /// blocks.
    fn lower_func(&self, func: &LirFunc) {
        let param = self.ptr_type();
        let arity = func.ins.len() + func.outs.len();
        let param_types = vec![param.into(); arity];
        let fn_type = self.context.void_type().fn_type(&param_types, false);
        let function = self.module.add_function(&func.name, fn_type, None);

        let params = function.get_params();
        let mut state = FnState {
            places: HashMap::new(),
            place_types: HashMap::new(),
            regs: HashMap::new(),
        };
        for (index, (place, ty)) in func.ins.iter().enumerate() {
            if let Some(value) = params.get(index).copied() {
                value.set_name(&format!("in{index}"));
                state
                    .places
                    .insert(place.clone(), value.into_pointer_value());
                state.place_types.insert(place.clone(), ty.clone());
            }
        }
        for (index, (place, ty)) in func.outs.iter().enumerate() {
            let slot = func.ins.len() + index;
            if let Some(value) = params.get(slot).copied() {
                value.set_name(&format!("out{index}"));
                state
                    .places
                    .insert(place.clone(), value.into_pointer_value());
                state.place_types.insert(place.clone(), ty.clone());
            }
        }

        if func.is_extern {
            return;
        }

        let mut blocks: HashMap<&str, BasicBlock<'ctx>> = HashMap::new();
        for block in &func.blocks {
            let basic_block = self.context.append_basic_block(function, &block.label);
            blocks.insert(block.label.as_str(), basic_block);
        }

        let builder = self.context.create_builder();
        for block in &func.blocks {
            let Some(basic_block) = blocks.get(block.label.as_str()).copied() else {
                bug!(Span::dummy(), "missing block `${}`", block.label);
            };
            builder.position_at_end(basic_block);
            for inst in &block.insts {
                self.lower_inst(inst, &builder, &mut state);
            }
            lower_term(&block.term, &builder, &blocks);
        }
    }

    /// Lowers the value/memory/conversion instructions M5b Task 3 owns. Calls,
    /// drops, and terminators belong to Task 4; reaching one here is an ICE.
    fn lower_inst(&self, inst: &LirInst, builder: &Builder<'ctx>, state: &mut FnState<'ctx>) {
        match inst {
            LirInst::Alloca { ty, place, .. } => {
                self.lower_alloca(ty, place, builder, state);
            }
            LirInst::CopyReg { src, dst, ty, .. } => {
                let value = self.operand(src, Some(ty), builder, state);
                state.regs.insert(dst.clone(), value);
            }
            LirInst::CopyPlace { src, dst, .. } => {
                let value = self.operand(src, None, builder, state);
                let ptr = place_ptr(dst, state);
                checked(builder.build_store(ptr, value), "copy store");
            }
            LirInst::Const { dst, lit, ty, .. } => {
                let value = self.const_value(lit, ty);
                state.regs.insert(dst.clone(), value);
            }
            LirInst::Load { src, dst, ty, .. } => {
                let value = self.load_as(src, ty, builder, state);
                state.regs.insert(dst.clone(), value);
            }
            LirInst::Addr { place, dst, .. } => {
                let ptr = place_ptr(place, state);
                state.regs.insert(dst.clone(), ptr.into());
            }
            LirInst::AddrFunc { func, dst, .. } => {
                let Some(function) = self.module.get_function(func) else {
                    bug!(Span::dummy(), "address of unknown function `@{func}`");
                };
                state.regs.insert(
                    dst.clone(),
                    function.as_global_value().as_pointer_value().into(),
                );
            }
            LirInst::Field {
                obj, idx, dst, ty, ..
            } => {
                let field = self.field_ptr(obj, *idx, builder, state);
                let value = checked(
                    builder.build_load(self.lir_type(ty), field, ""),
                    "field load",
                );
                state.regs.insert(dst.clone(), value);
            }
            LirInst::FieldAddr { obj, idx, dst, .. } => {
                let field = self.field_ptr(obj, *idx, builder, state);
                state.regs.insert(dst.clone(), field.into());
            }
            LirInst::StoreField {
                value, obj, idx, ..
            } => {
                let field = self.field_ptr(obj, *idx, builder, state);
                let stored = self.operand(value, None, builder, state);
                checked(builder.build_store(field, stored), "field store");
            }
            LirInst::Store { src, dst, .. } => {
                let reference = self.load_place(dst, builder, state).into_pointer_value();
                let value = self.operand(src, None, builder, state);
                checked(builder.build_store(reference, value), "store through ref");
            }
            LirInst::Convert {
                op, src, dst, ty, ..
            } => {
                let source = reg_int(src, state);
                let BasicTypeEnum::IntType(result_ty) = self.lir_type(ty) else {
                    bug!(Span::dummy(), "`convert` to non-integer type `{ty}`");
                };
                let value = match op {
                    ConvertOp::Trunc => {
                        checked(builder.build_int_truncate(source, result_ty, ""), "trunc")
                    }
                    ConvertOp::ExtendZero => {
                        checked(builder.build_int_z_extend(source, result_ty, ""), "zext")
                    }
                };
                state.regs.insert(dst.clone(), value.into());
            }
            LirInst::Arith {
                op,
                lhs,
                rhs,
                dst,
                ty,
                ..
            } => {
                let value = self.arith(op, lhs, rhs, ty, builder, state);
                state.regs.insert(dst.clone(), value);
            }
            LirInst::Call { .. }
            | LirInst::Drop { .. }
            | LirInst::CompileError { .. }
            | LirInst::FoldIntrinsic { .. } => bug!(
                Span::dummy(),
                "instruction `{inst:?}` is not lowered until M5b Task 4"
            ),
        }
    }

    /// Allocates a zeroed place of `ty` and binds it under `place`.
    fn lower_alloca(
        &self,
        ty: &str,
        place: &str,
        builder: &Builder<'ctx>,
        state: &mut FnState<'ctx>,
    ) {
        let value_ty = self.lir_type(ty);
        let ptr = checked(builder.build_alloca(value_ty, place), "alloca");
        checked(
            builder.build_store(ptr, value_ty.const_zero()),
            "zero-init store",
        );
        state.places.insert(place.to_owned(), ptr);
        state.place_types.insert(place.to_owned(), ty.to_owned());
    }

    /// Resolves an operand to an SSA value: a register's value, or a load of a
    /// place (using `ty` when the instruction names it, else the place's own
    /// declared type).
    fn operand(
        &self,
        operand: &LirOperand,
        ty: Option<&str>,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match operand {
            LirOperand::Reg(name) => match state.regs.get(name).copied() {
                Some(value) => value,
                None => bug!(Span::dummy(), "use of undefined register `&{name}`"),
            },
            LirOperand::Place(name) => match ty {
                Some(ty) => self.load_as(name, ty, builder, state),
                None => self.load_place(name, builder, state),
            },
        }
    }

    /// Loads the value stored in place `name`.
    fn load_place(
        &self,
        name: &str,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let Some(ty) = state.place_types.get(name).cloned() else {
            bug!(Span::dummy(), "load of undeclared place `%{name}`");
        };
        self.load_as(name, &ty, builder, state)
    }

    /// Loads place `name` as the given LIR type.
    fn load_as(
        &self,
        name: &str,
        ty: &str,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let ptr = place_ptr(name, state);
        checked(builder.build_load(self.lir_type(ty), ptr, ""), "load")
    }

    /// GEPs to field `idx` of place `obj`, loading the object pointer first
    /// when the place holds a `ref`.
    fn field_ptr(
        &self,
        obj: &str,
        idx: u32,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> PointerValue<'ctx> {
        let Some(place_ty) = state.place_types.get(obj).cloned() else {
            bug!(Span::dummy(), "field of undeclared place `%{obj}`");
        };
        let owner = place_ty.strip_prefix("ref ").unwrap_or(&place_ty);
        let BasicTypeEnum::StructType(struct_ty) = self.lir_type(owner) else {
            bug!(Span::dummy(), "field of non-struct place `%{obj}`");
        };
        let base = if place_ty.starts_with("ref ") {
            let raw = place_ptr(obj, state);
            checked(builder.build_load(self.ptr_type(), raw, ""), "load ref").into_pointer_value()
        } else {
            place_ptr(obj, state)
        };
        checked(
            builder.build_struct_gep(struct_ty, base, idx, ""),
            "field gep",
        )
    }

    /// Materializes a `const` literal: a data-symbol address (or `#byte_buffer`
    /// wrapper), or an integer/pointer immediate.
    fn const_value(&self, lit: &str, ty: &str) -> BasicValueEnum<'ctx> {
        if let Some(name) = lit.strip_prefix('!') {
            return self.data_const(name, ty);
        }
        let value = parse_int(lit);
        match self.lir_type(ty) {
            BasicTypeEnum::IntType(int_ty) => int_ty.const_int(value, false).into(),
            BasicTypeEnum::PointerType(ptr_ty) if value == 0 => ptr_ty.const_null().into(),
            other => bug!(
                Span::dummy(),
                "const literal `{lit}` cannot be a `{}` value",
                other.print_to_string()
            ),
        }
    }

    /// The value of a data-symbol `const`: `#byte_buffer` wraps `{len, ptr}`,
    /// a reference type is the bare global address.
    fn data_const(&self, name: &str, ty: &str) -> BasicValueEnum<'ctx> {
        let Some(global) = self.module.get_global(name) else {
            bug!(Span::dummy(), "const of undeclared data `!{name}`");
        };
        let ptr = global.as_pointer_value();
        if ty == "#byte_buffer" {
            let len = self.data_len(name);
            let len_value = self.native.const_int(len, false);
            self.context
                .const_struct(&[len_value.into(), ptr.into()], false)
                .into()
        } else if ty.starts_with("ref ") || ty == "#ptr" {
            ptr.into()
        } else {
            // The LIR verifier's `check_const_lit` accepts `#byte_buffer`,
            // `#format_args`, and `ref #bit8` data-symbol consts. A `[N x i8]`
            // global can only become a `#byte_buffer` wrapper or a bare
            // reference; `#format_args` (a `{ iNative, [16 x …] }` piece table)
            // has no meaningful lowering and no `lilac-lower` path emits one.
            // This explicit, documented ICE pins the divergence
            // (`format_args_data_const_is_ice`).
            bug!(
                Span::dummy(),
                "data-symbol const type `{ty}` is accepted by the LIR verifier but has no codegen lowering"
            );
        }
    }

    /// Byte length of a data global (no NUL terminator).
    fn data_len(&self, name: &str) -> u64 {
        match self.lir.data.iter().find(|datum| datum.name == name) {
            Some(datum) => match &datum.kind {
                LirDataKind::Str(text) | LirDataKind::NumLit(text) => {
                    u64::try_from(text.len()).unwrap_or(u64::MAX)
                }
            },
            None => bug!(Span::dummy(), "const of undeclared data `!{name}`"),
        }
    }

    /// Lowers one arithmetic/comparison op. Integer ops work on bit containers
    /// directly; float ops reinterpret the bit container as `float`/`double`,
    /// apply the op, then reinterpret the result back.
    fn arith(
        &self,
        op: &ArithOp,
        lhs: &str,
        rhs: &str,
        ty: &str,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match op {
            ArithOp::IAdd | ArithOp::ISub | ArithOp::IMul | ArithOp::IDiv | ArithOp::IMod => {
                let left = reg_int(lhs, state);
                let right = reg_int(rhs, state);
                if !matches!(self.lir_type(ty), BasicTypeEnum::IntType(_)) {
                    bug!(
                        Span::dummy(),
                        "`{op:?}` needs a bit-container type, found `{ty}`"
                    );
                }
                let value = match op {
                    ArithOp::IAdd => builder.build_int_add(left, right, ""),
                    ArithOp::ISub => builder.build_int_sub(left, right, ""),
                    ArithOp::IMul => builder.build_int_mul(left, right, ""),
                    ArithOp::IDiv => builder.build_int_signed_div(left, right, ""),
                    _ => builder.build_int_signed_rem(left, right, ""),
                };
                checked(value, "integer arith").into()
            }
            ArithOp::Eq | ArithOp::ILt | ArithOp::IGt => {
                let left = reg_int(lhs, state);
                let right = reg_int(rhs, state);
                let predicate = match op {
                    ArithOp::Eq => IntPredicate::EQ,
                    ArithOp::ILt => IntPredicate::SLT,
                    _ => IntPredicate::SGT,
                };
                let cmp = checked(
                    builder.build_int_compare(predicate, left, right, ""),
                    "icmp",
                );
                self.widen_bool(cmp, ty, builder)
            }
            ArithOp::FAdd | ArithOp::FSub | ArithOp::FMul | ArithOp::FDiv | ArithOp::FMod => {
                let left = reg_int(lhs, state);
                let right = reg_int(rhs, state);
                let float_ty = self.float_type(left.get_type().get_bit_width());
                let left_f = bit_as_float(left, float_ty, builder);
                let right_f = bit_as_float(right, float_ty, builder);
                let BasicTypeEnum::IntType(result_ty) = self.lir_type(ty) else {
                    bug!(
                        Span::dummy(),
                        "`{op:?}` needs a bit-container type, found `{ty}`"
                    );
                };
                let value = match op {
                    ArithOp::FAdd => builder.build_float_add(left_f, right_f, ""),
                    ArithOp::FSub => builder.build_float_sub(left_f, right_f, ""),
                    ArithOp::FMul => builder.build_float_mul(left_f, right_f, ""),
                    ArithOp::FDiv => builder.build_float_div(left_f, right_f, ""),
                    _ => builder.build_float_rem(left_f, right_f, ""),
                };
                let bits = checked(value, "float arith");
                checked(
                    builder.build_bit_cast(bits, result_ty, ""),
                    "float result bitcast",
                )
            }
            ArithOp::FLt | ArithOp::FGt => {
                let left = reg_int(lhs, state);
                let right = reg_int(rhs, state);
                let float_ty = self.float_type(left.get_type().get_bit_width());
                let left_f = bit_as_float(left, float_ty, builder);
                let right_f = bit_as_float(right, float_ty, builder);
                let predicate = if matches!(op, ArithOp::FLt) {
                    FloatPredicate::OLT
                } else {
                    FloatPredicate::OGT
                };
                let cmp = checked(
                    builder.build_float_compare(predicate, left_f, right_f, ""),
                    "fcmp",
                );
                self.widen_bool(cmp, ty, builder)
            }
        }
    }

    /// Widens an `i1` comparison result to the declared bit-container type.
    fn widen_bool(
        &self,
        cmp: IntValue<'ctx>,
        ty: &str,
        builder: &Builder<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let BasicTypeEnum::IntType(result_ty) = self.lir_type(ty) else {
            bug!(
                Span::dummy(),
                "comparison result needs a bit-container type, found `{ty}`"
            );
        };
        if result_ty.get_bit_width() == 1 {
            cmp.into()
        } else {
            checked(builder.build_int_z_extend(cmp, result_ty, ""), "widen bool").into()
        }
    }

    /// The float type for a bit width.
    fn float_type(&self, bits: u32) -> FloatType<'ctx> {
        match bits {
            32 => self.context.f32_type(),
            64 => self.context.f64_type(),
            other => bug!(Span::dummy(), "no float type for `#bit{other}`"),
        }
    }
}

/// Returns a place's pointer, raising an ICE if it was never declared.
fn place_ptr<'ctx>(name: &str, state: &FnState<'ctx>) -> PointerValue<'ctx> {
    match state.places.get(name).copied() {
        Some(ptr) => ptr,
        None => bug!(Span::dummy(), "use of undeclared place `%{name}`"),
    }
}

/// Reads a register as an integer value.
fn reg_int<'ctx>(name: &str, state: &FnState<'ctx>) -> IntValue<'ctx> {
    match state.regs.get(name).copied() {
        Some(BasicValueEnum::IntValue(value)) => value,
        Some(other) => bug!(
            Span::dummy(),
            "register `&{name}` is a `{}`, not an integer",
            other.get_type().print_to_string()
        ),
        None => bug!(Span::dummy(), "use of undefined register `&{name}`"),
    }
}

/// Parses an integer literal (`0x`/`0b`/decimal, `_` separators).
fn parse_int(text: &str) -> u64 {
    let digits = text.trim().replace('_', "");
    let parsed = if let Some(hex) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = digits
        .strip_prefix("0b")
        .or_else(|| digits.strip_prefix("0B"))
    {
        u64::from_str_radix(bin, 2).ok()
    } else if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
        digits.parse::<u64>().ok()
    } else {
        None
    };
    match parsed {
        Some(value) => value,
        None => bug!(Span::dummy(), "invalid const literal `{text}`"),
    }
}

/// Reinterprets a bit container as a float of `float_ty`.
fn bit_as_float<'ctx>(
    value: IntValue<'ctx>,
    float_ty: FloatType<'ctx>,
    builder: &Builder<'ctx>,
) -> FloatValue<'ctx> {
    checked(builder.build_bit_cast(value, float_ty, ""), "float bitcast").into_float_value()
}

/// Lowers a block terminator. `return` is final; the remaining shapes are
/// verifiable placeholders until M5b Tasks 3–4 lower control flow and panics.
fn lower_term(term: &LirTerm, builder: &Builder<'_>, blocks: &HashMap<&str, BasicBlock<'_>>) {
    match term {
        LirTerm::Return { .. } => {
            checked(builder.build_return(None), "return");
        }
        LirTerm::Unreachable { .. } | LirTerm::Panic { .. } => {
            checked(builder.build_unreachable(), "unreachable");
        }
        LirTerm::Branch { target, .. } => {
            if let Some(block) = blocks.get(target.as_str()) {
                checked(builder.build_unconditional_branch(*block), "branch");
            } else {
                bug!(Span::dummy(), "branch to unknown block `${target}`");
            }
        }
        LirTerm::BranchIf { then, .. } => {
            if let Some(block) = blocks.get(then.as_str()) {
                checked(
                    builder.build_unconditional_branch(*block),
                    "branch.if placeholder",
                );
            } else {
                bug!(Span::dummy(), "branch.if to unknown block `${then}`");
            }
        }
    }
}
