//! LIR → LLVM lowering: types, layouts, data globals, places, instructions, and
//! terminators.
//!
//! Covers the type map, named layouts, data globals, the pointer calling
//! convention, zero-initialized `alloca` places, every value / memory /
//! conversion / constant / arithmetic instruction, direct/indirect/builtin
//! calls, `drop`, and control flow (`branch`, `branch.if`, `return`, `panic`,
//! `unreachable`). Builtin calls use the value ABI and reuse the runtime
//! definitions the `runtime` module emits before any user function.

use std::collections::{HashMap, HashSet};

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::{Builder, BuilderError};
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::targets::TargetData;
use inkwell::types::{
    BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, IntType, PointerType, StructType,
};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FloatValue, FunctionValue,
    InstructionValue, IntValue, PointerValue, ValueKind,
};
use inkwell::{AddressSpace, AtomicRMWBinOp, FloatPredicate, IntPredicate};

use lilac_codegen::Entry;
use lilac_error::{bug, codes, Diagnostic};
use lilac_ir::{
    builtin_layout, builtin_sig, ArithOp, AtomicOrdering, ConvertOp, LirCallee, LirConst, LirFunc,
    LirInst, LirModule, LirOperand, LirTerm, LirTy, PlaceName, Reg, RmwOp, Symbol,
};
use lilac_span::Span;
use lilac_target::{is_64_bit_triple, Arch, Target};

/// Turns a fallible inkwell builder call into an ICE.
pub(crate) fn checked<T>(result: Result<T, BuilderError>, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(err) => bug!(Span::dummy(), "failed to build {what}: {err}"),
    }
}

/// Per-function lowering state: every place's pointer and declared LIR type,
/// plus the SSA value bound to each `&` register.
struct FnState<'ctx> {
    places: HashMap<PlaceName, PointerValue<'ctx>>,
    place_types: HashMap<PlaceName, LirTy>,
    regs: HashMap<Reg, BasicValueEnum<'ctx>>,
}

/// One lowering context: an LLVM module plus the LIR being lowered into it.
pub(crate) struct Lower<'ctx, 'a> {
    context: &'ctx Context,
    module: Module<'ctx>,
    lir: &'a LirModule,
    native: IntType<'ctx>,
    arch: Option<Arch>,
    named: HashMap<String, StructType<'ctx>>,
}

impl<'ctx, 'a> Lower<'ctx, 'a> {
    /// Creates a lowering context over `module` for `triple`, which picks
    /// `iNative` and the `@syscall` instruction.
    pub(crate) fn new(
        context: &'ctx Context,
        module: Module<'ctx>,
        lir: &'a LirModule,
        triple: &str,
    ) -> Self {
        let native = if is_64_bit_triple(triple) {
            context.i64_type()
        } else {
            context.i32_type()
        };
        Self {
            context,
            module,
            lir,
            native,
            arch: Target::parse(triple).ok().map(|target| target.arch),
            named: HashMap::new(),
        }
    }

    /// Consumes the context, returning the populated LLVM module.
    pub(crate) fn into_module(self) -> Module<'ctx> {
        self.module
    }

    /// The LLVM context this lowering is built in.
    pub(crate) fn context(&self) -> &'ctx Context {
        self.context
    }

    /// The LLVM module being populated.
    pub(crate) fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// The target's architecture, when the triple parses.
    pub(crate) fn arch(&self) -> Option<Arch> {
        self.arch
    }

    /// The target's `iNative` integer width.
    pub(crate) fn native_int(&self) -> IntType<'ctx> {
        self.native
    }

    /// Lowers layouts, data globals, the in-module runtime, and functions into
    /// the module, in that fixed order. Every function is declared before any
    /// body is lowered so calls and `addr.func` may reference functions defined
    /// later, and the runtime is defined first so builtin calls reuse it.
    ///
    /// An `extern` signature outside the supported C scalar/pointer set (or
    /// with more than one out) is `E028`.
    pub(crate) fn run(&mut self, entry: Entry) -> Result<(), Diagnostic> {
        self.define_layouts();
        self.define_data();
        self.emit_runtime();
        for func in &self.lir.funcs {
            self.declare_func(func)?;
        }
        for func in &self.lir.funcs {
            self.lower_func(func);
        }
        if entry == Entry::CMain {
            self.emit_entry_wrapper();
        }
        Ok(())
    }

    /// Appends the C entry point when the LIR has the entry stub
    /// `.lilac.start` (an executable): `i32 @main()` calls it. The stub ends
    /// the process through `@exit` and never returns, so the wrapper ends
    /// `unreachable`. A module without the stub gets no wrapper.
    fn emit_entry_wrapper(&self) {
        let Some(start) = self.module.get_function(".lilac.start") else {
            return;
        };
        let i32_ty = self.context.i32_type();
        let main = self
            .module
            .add_function("main", i32_ty.fn_type(&[], false), None);
        let builder = self.context.create_builder();
        let entry = self.context.append_basic_block(main, "entry");
        builder.position_at_end(entry);
        checked(builder.build_call(start, &[], ""), "main wrapper call");
        checked(builder.build_unreachable(), "main wrapper end");
    }

    /// The leaf builtins this module references, so the runtime emits only the
    /// ones a surviving function calls: every `@`-builtin `call`. Names keep
    /// their `@` sigil.
    pub(crate) fn used_builtins(&self) -> HashSet<&str> {
        let mut used = HashSet::new();
        for func in &self.lir.funcs {
            for block in &func.blocks {
                for inst in &block.insts {
                    if let LirInst::Call {
                        func: LirCallee::Builtin(name),
                        ..
                    } = inst
                    {
                        used.insert(name.as_str());
                    }
                }
            }
        }
        used
    }

    /// Emits the fixed in-module builtin runtime.
    fn emit_runtime(&self) {
        super::runtime::emit_runtime(self);
    }

    /// Maps a LIR type to its LLVM type.
    pub(crate) fn lir_type(&self, ty: &LirTy) -> BasicTypeEnum<'ctx> {
        match ty {
            LirTy::Ref(_) => self.ptr_type().into(),
            LirTy::Bit(8) => self.context.i8_type().into(),
            LirTy::Bit(16) => self.context.i16_type().into(),
            LirTy::Bit(32) => self.context.i32_type().into(),
            LirTy::Bit(64) => self.context.i64_type().into(),
            LirTy::BitNative => self.native.into(),
            LirTy::FBit(16) => self.context.f16_type().into(),
            LirTy::FBit(32) => self.context.f32_type().into(),
            LirTy::FBit(64) => self.context.f64_type().into(),
            LirTy::FBit(128) => self.context.f128_type().into(),
            LirTy::Named(name) if self.named.contains_key(name) => self.named[name].into(),
            LirTy::Array(elem, len) => {
                let Ok(len) = u32::try_from(*len) else {
                    bug!(Span::dummy(), "array length `{len}` exceeds LLVM's limit");
                };
                self.lir_type(elem).array_type(len).into()
            }
            other => bug!(Span::dummy(), "unknown LIR type `{other}`"),
        }
    }

    /// The opaque `ptr` type used for every place under the pointer ABI.
    pub(crate) fn ptr_type(&self) -> PointerType<'ctx> {
        self.context.ptr_type(AddressSpace::default())
    }

    /// Defines one named LLVM struct per LIR layout, in sorted order so printed
    /// IR is deterministic. Opaque shells are created first so fields may refer
    /// to any layout regardless of name order.
    ///
    /// The builtin aggregates `#byte_buffer`/`#format_args`/`#.lilac.frame`
    /// never appear in `module.layouts`, so they are built here from
    /// `builtin_layout`, resolving `#byte_buffer` first (the other two embed
    /// it).
    fn define_layouts(&mut self) {
        for ty in [
            LirTy::byte_buffer(),
            LirTy::format_args(),
            LirTy::trace_frame(),
        ] {
            let Some(layout) = builtin_layout(&ty) else {
                continue;
            };
            let fields: Vec<BasicTypeEnum<'ctx>> =
                layout.iter().map(|(_, ty)| self.lir_type(ty)).collect();
            let struct_ty = self.context.struct_type(&fields, false);
            if let Some(name) = ty.name() {
                self.named.insert(name.to_owned(), struct_ty);
            }
        }
        let mut names: Vec<&String> = self.lir.layouts.keys().collect();
        names.sort_unstable();
        for name in &names {
            let struct_ty = self.context.opaque_struct_type(name);
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
            let bytes = datum.bytes.as_slice();
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
            let global = self.module.add_global(array_ty, None, datum.name.as_str());
            global.set_linkage(Linkage::Private);
            global.set_constant(true);
            global.set_unnamed_addr(true);
            global.set_initializer(&init);
        }
    }

    /// Declares one function without lowering its body. Internal functions get
    /// the pointer-place ABI `void @name(ptr %in0, …, ptr %out0, …)`; an
    /// `extern` function gets its platform C signature under its **unmangled**
    /// declared name (a `.lilac.` hook declaration keeps the pointer-place
    /// ABI, [`LirFunc::uses_c_abi`]). Declaring every function up front lets
    /// calls and `addr.func` reference functions defined later.
    fn declare_func(&self, func: &LirFunc) -> Result<(), Diagnostic> {
        if func.uses_c_abi() {
            let fn_type = self.extern_fn_type(func)?;
            self.module.add_function(func.name.as_str(), fn_type, None);
            return Ok(());
        }
        let arity = func.ins.len() + func.outs.len();
        let param_types = vec![self.ptr_type().into(); arity];
        let fn_type = self.context.void_type().fn_type(&param_types, false);
        let function = self
            .module
            .add_function(symbol(func.name.as_str()), fn_type, None);
        if func.inline {
            let kind = Attribute::get_named_enum_kind_id("alwaysinline");
            let attribute = self.context.create_enum_attribute(kind, 0);
            function.add_attribute(AttributeLoc::Function, attribute);
        }
        Ok(())
    }

    /// The platform C signature of an `extern` function: every `in` maps to its
    /// C type and the single `out` (if any) becomes the return value. A slot
    /// outside [`Self::extern_c_type`]'s set, or more than one out, is `E028`.
    fn extern_fn_type(&self, func: &LirFunc) -> Result<FunctionType<'ctx>, Diagnostic> {
        let mut params = Vec::with_capacity(func.ins.len());
        for (place, ty) in &func.ins {
            let c_type = self.extern_c_type(ty).ok_or_else(|| {
                Diagnostic::error(
                    codes::E028,
                    format!("`extern` parameter `{place}` has unsupported C type `{ty}`"),
                )
                .help("`extern` parameters must be a bit integer, a float, or `ref T`")
            })?;
            params.push(BasicMetadataTypeEnum::from(c_type));
        }
        let returns = match func.outs.as_slice() {
            [] => None,
            [(place, ty)] => Some(self.extern_c_type(ty).ok_or_else(|| {
                Diagnostic::error(
                    codes::E028,
                    format!("`extern` return `{place}` has unsupported C type `{ty}`"),
                )
                .help("`extern` returns must be a bit integer, a float, or `ref T`")
            })?),
            _ => {
                return Err(Diagnostic::error(
                    codes::E028,
                    format!(
                        "`extern` function `{}` declares {} out places; at most one is allowed",
                        func.name,
                        func.outs.len()
                    ),
                ))
            }
        };
        // A variadic prototype lets LLVM apply each target's varargs rules
        // (stack slots on Apple arm64, `%al` on x86-64 System V).
        Ok(match returns {
            Some(return_ty) => return_ty.fn_type(&params, func.variadic),
            None => self.context.void_type().fn_type(&params, func.variadic),
        })
    }

    /// Maps a LIR type to its platform C ABI type, or `None` when it is not one
    /// of the supported by-value scalars or a reference (pointer).
    ///
    /// The accept-set comes from [`lilac_ir::is_c_abi_type`] — the same
    /// predicate the frontend `E028` check uses — so a type the driver accepts
    /// always has a mapping here.
    fn extern_c_type(&self, ty: &LirTy) -> Option<BasicTypeEnum<'ctx>> {
        lilac_ir::is_c_abi_type(ty).then(|| self.lir_type(ty))
    }

    /// Lowers one function body: its signature places are bound to the incoming
    /// pointer arguments, then its blocks and terminators are emitted.
    fn lower_func(&self, func: &LirFunc) {
        let name = if func.is_extern {
            func.name.as_str()
        } else {
            symbol(func.name.as_str())
        };
        let Some(function) = self.module.get_function(name) else {
            bug!(Span::dummy(), "function `{}` was not declared", func.name);
        };

        if func.is_extern {
            return;
        }

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

        let mut blocks: HashMap<&str, BasicBlock<'ctx>> = HashMap::new();
        for block in &func.blocks {
            let basic_block = self
                .context
                .append_basic_block(function, block.label.as_str());
            blocks.insert(block.label.as_str(), basic_block);
        }

        let builder = self.context.create_builder();
        for (index, block) in func.blocks.iter().enumerate() {
            let Some(basic_block) = blocks.get(block.label.as_str()).copied() else {
                bug!(Span::dummy(), "missing block `{}`", block.label);
            };
            builder.position_at_end(basic_block);
            for inst in &block.insts {
                self.lower_inst(inst, &builder, &mut state);
            }
            let next = func
                .blocks
                .get(index + 1)
                .and_then(|next| blocks.get(next.label.as_str()).copied());
            Self::lower_term(&block.term, &builder, &blocks, &state, next);
        }
    }

    /// Lowers one instruction. A `CompileError` marker is reported as `E031`
    /// before verification; a survivor is an ICE.
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
            LirInst::Const { dst, value, ty, .. } => {
                let value = self.const_value(value, ty);
                state.regs.insert(dst.clone(), value);
            }
            LirInst::Load { src, dst, ty, .. } => {
                // `load %src` always follows the pointer in `src`, even when
                // the pointee is itself a `ref` (an `Optional[ref T]` payload).
                let ptr = checked(
                    builder.build_load(self.ptr_type(), place_ptr(src, state), ""),
                    "load ref",
                )
                .into_pointer_value();
                let value = checked(builder.build_load(self.lir_type(ty), ptr, ""), "load");
                state.regs.insert(dst.clone(), value);
            }
            LirInst::Addr { place, dst, .. } => {
                let ptr = place_ptr(place, state);
                state.regs.insert(dst.clone(), ptr.into());
            }
            LirInst::AddrFunc { func, dst, ty, .. } => {
                let value = self.addr_func_value(func, ty, builder);
                state.regs.insert(dst.clone(), value);
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
            LirInst::ElemAddr { .. } => self.lower_elem_addr(inst, builder, state),
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
                self.lower_convert(op, src, dst, ty, builder, state);
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
            LirInst::Call {
                func, ins, outs, ..
            } => {
                self.lower_call(func, ins, outs, builder, state);
            }
            LirInst::Drop { .. } => {}
            LirInst::Zero { place, .. } => self.lower_zero(place, builder, state),
            LirInst::CompileError { .. } => bug!(
                Span::dummy(),
                "instruction `{inst:?}` is a compile-time marker and must not reach codegen"
            ),
            LirInst::AtomicLoad { .. }
            | LirInst::AtomicStore { .. }
            | LirInst::AtomicRmw { .. }
            | LirInst::CmpXchg { .. }
            | LirInst::Fence { .. } => self.lower_atomic(inst, builder, state),
        }
    }

    /// Lowers `zero %place`: a `memset` of the place's size.
    fn lower_zero(&self, place: &PlaceName, builder: &Builder<'ctx>, state: &FnState<'ctx>) {
        let ptr = place_ptr(place, state);
        let Some(ty) = state.place_types.get(place) else {
            bug!(Span::dummy(), "zero of undeclared place `{place}`");
        };
        let Some(size) = self.lir_type(ty).size_of() else {
            bug!(Span::dummy(), "zero of unsized place `{place}`");
        };
        let zero = self.context.i8_type().const_zero();
        checked(builder.build_memset(ptr, 1, zero, size), "zero place");
    }

    /// Lowers an atomic instruction. `atomicrmw` and `cmpxchg` take their
    /// natural alignment from the builder; a load or store is given it.
    fn lower_atomic(&self, inst: &LirInst, builder: &Builder<'ctx>, state: &mut FnState<'ctx>) {
        match inst {
            LirInst::AtomicLoad {
                ptr,
                dst,
                ty,
                order,
                ..
            } => {
                let value = checked(
                    builder.build_load(self.lir_type(ty), reg_ptr(ptr, state), ""),
                    "atomic load",
                );
                self.make_atomic(value.as_instruction_value(), ty, *order);
                state.regs.insert(dst.clone(), value);
            }
            LirInst::AtomicStore {
                value,
                ptr,
                ty,
                order,
                ..
            } => {
                let store = checked(
                    builder.build_store(reg_ptr(ptr, state), reg_int(value, state)),
                    "atomic store",
                );
                self.make_atomic(Some(store), ty, *order);
            }
            LirInst::AtomicRmw {
                op,
                ptr,
                value,
                dst,
                order,
                ..
            } => {
                let old = checked(
                    builder.build_atomicrmw(
                        rmw_bin_op(*op),
                        reg_ptr(ptr, state),
                        reg_int(value, state),
                        llvm_ordering(*order),
                    ),
                    "atomicrmw",
                );
                state.regs.insert(dst.clone(), old.into());
            }
            LirInst::CmpXchg {
                ptr,
                expected,
                desired,
                dst,
                order,
                ..
            } => {
                let pair = checked(
                    builder.build_cmpxchg(
                        reg_ptr(ptr, state),
                        reg_int(expected, state),
                        reg_int(desired, state),
                        llvm_ordering(*order),
                        llvm_ordering(order.failure()),
                    ),
                    "cmpxchg",
                );
                let old = checked(builder.build_extract_value(pair, 0, ""), "cmpxchg old");
                state.regs.insert(dst.clone(), old);
            }
            LirInst::Fence { order, .. } => {
                checked(
                    builder.build_fence(llvm_ordering(*order), false, ""),
                    "fence",
                );
            }
            _ => bug!(Span::dummy(), "non-atomic instruction `{inst:?}`"),
        }
    }

    /// Makes a load or store atomic, aligned to its width.
    fn make_atomic(&self, inst: Option<InstructionValue<'ctx>>, ty: &LirTy, order: AtomicOrdering) {
        let Some(inst) = inst else {
            bug!(Span::dummy(), "atomic access is not an instruction");
        };
        let BasicTypeEnum::IntType(int_type) = self.lir_type(ty) else {
            bug!(Span::dummy(), "atomic access of non-integer `{ty}`");
        };
        let ordered = inst.set_atomic_ordering(llvm_ordering(order));
        let aligned = inst.set_alignment(int_type.get_bit_width() / 8);
        if let Err(err) = ordered.and(aligned) {
            bug!(Span::dummy(), "invalid atomic access: {err}");
        }
    }

    /// Lowers one `convert`. `trunc`/`extend.zero` change an integer's width;
    /// `bitcast` reinterprets between an integer and a float of the same width
    /// (e.g. `#bit32` <-> `#fbit32`), or between a reference and its address
    /// (`ptrtoint`/`inttoptr`).
    fn lower_convert(
        &self,
        op: &ConvertOp,
        src: &Reg,
        dst: &Reg,
        ty: &LirTy,
        builder: &Builder<'ctx>,
        state: &mut FnState<'ctx>,
    ) {
        let value = match op {
            ConvertOp::Trunc | ConvertOp::ExtendZero => {
                let source = reg_int(src, state);
                let BasicTypeEnum::IntType(result_ty) = self.lir_type(ty) else {
                    bug!(Span::dummy(), "`convert` to non-integer type `{ty}`");
                };
                if matches!(op, ConvertOp::Trunc) {
                    checked(builder.build_int_truncate(source, result_ty, ""), "trunc").into()
                } else {
                    checked(builder.build_int_z_extend(source, result_ty, ""), "zext").into()
                }
            }
            ConvertOp::Bitcast => {
                let source = self.operand(&LirOperand::Reg(src.to_owned()), None, builder, state);
                match (source, self.lir_type(ty)) {
                    (BasicValueEnum::PointerValue(pointer), BasicTypeEnum::IntType(int)) => {
                        checked(builder.build_ptr_to_int(pointer, int, ""), "ptrtoint").into()
                    }
                    (BasicValueEnum::IntValue(int), BasicTypeEnum::PointerType(pointer)) => {
                        checked(builder.build_int_to_ptr(int, pointer, ""), "inttoptr").into()
                    }
                    (source, result_ty) => {
                        checked(builder.build_bit_cast(source, result_ty, ""), "bitcast")
                    }
                }
            }
        };
        state.regs.insert(dst.to_owned(), value);
    }

    /// Allocates a zeroed place of `ty` and binds it under `place`.
    fn lower_alloca(
        &self,
        ty: &LirTy,
        place: &PlaceName,
        builder: &Builder<'ctx>,
        state: &mut FnState<'ctx>,
    ) {
        let value_ty = self.lir_type(ty);
        let ptr = checked(builder.build_alloca(value_ty, place.as_str()), "alloca");
        checked(
            builder.build_store(ptr, value_ty.const_zero()),
            "zero-init store",
        );
        state.places.insert(place.to_owned(), ptr);
        state.place_types.insert(place.to_owned(), ty.clone());
    }

    /// An `alloca` in the current function's entry block (so a loop does not
    /// grow the stack).
    fn entry_alloca(
        &self,
        ty: impl BasicType<'ctx>,
        builder: &Builder<'ctx>,
    ) -> PointerValue<'ctx> {
        let Some(entry) = builder
            .get_insert_block()
            .and_then(inkwell::basic_block::BasicBlock::get_parent)
            .and_then(inkwell::values::FunctionValue::get_first_basic_block)
        else {
            bug!(Span::dummy(), "alloca outside a function");
        };
        let at_entry = self.context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => at_entry.position_before(&first),
            None => at_entry.position_at_end(entry),
        }
        checked(at_entry.build_alloca(ty, ""), "entry alloca")
    }

    /// Materializes the address of `@name`, normalized to the instruction's
    /// declared type: an integer bit type gets `ptrtoint` (the indirect-call
    /// path `inttoptr`s it back); a pointer type keeps the function pointer.
    fn addr_func_value(
        &self,
        name: &Symbol,
        ty: &LirTy,
        builder: &Builder<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let Some(function) = self.module.get_function(symbol(name.as_str())) else {
            bug!(Span::dummy(), "address of unknown function `{name}`");
        };
        let ptr = function.as_global_value().as_pointer_value();
        match self.lir_type(ty) {
            BasicTypeEnum::IntType(int_ty) => {
                checked(builder.build_ptr_to_int(ptr, int_ty, ""), "ptrtoint").into()
            }
            BasicTypeEnum::PointerType(_) => ptr.into(),
            other => bug!(
                Span::dummy(),
                "`addr.func` type `{ty}` is `{}`, not an integer or pointer",
                other.print_to_string()
            ),
        }
    }

    /// Lowers a `call` according to its target kind.
    fn lower_call(
        &self,
        callee: &LirCallee,
        ins: &[PlaceName],
        outs: &[PlaceName],
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) {
        match callee {
            LirCallee::Func(name) => self.lower_direct_call(name, ins, outs, builder, state),
            LirCallee::Reg(reg) => self.lower_indirect_call(reg, ins, outs, builder, state),
            LirCallee::Builtin(name) => self.lower_builtin_call(name, ins, outs, builder, state),
        }
    }

    /// A direct user call. An internal callee uses the pointer-place ABI
    /// `call void @f(ptr %in…, ptr %out…)`; an `extern` callee uses the
    /// platform C ABI, with scalars by value, `ref T` as a pointer, and the
    /// single return stored into the out place.
    fn lower_direct_call(
        &self,
        name: &Symbol,
        ins: &[PlaceName],
        outs: &[PlaceName],
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) {
        let Some(callee) = self.lir.funcs.iter().find(|func| func.name == *name) else {
            bug!(Span::dummy(), "call to unknown function `{name}`");
        };
        let llvm_name = if callee.is_extern {
            name.as_str()
        } else {
            symbol(name.as_str())
        };
        let Some(function) = self.module.get_function(llvm_name) else {
            bug!(Span::dummy(), "call to unknown function `{name}`");
        };
        if callee.uses_c_abi() {
            let args = self.extern_call_args(callee, ins, builder, state);
            let result = checked(builder.build_call(function, &args, ""), "extern call");
            if let Some(out) = outs.first() {
                let value = match result.try_as_basic_value() {
                    ValueKind::Basic(value) => value,
                    ValueKind::Instruction(_) => {
                        bug!(Span::dummy(), "extern `{name}` unexpectedly returned void")
                    }
                };
                checked(
                    builder.build_store(place_ptr(out, state), value),
                    "extern result store",
                );
            }
        } else {
            let args = self.direct_call_args(callee, ins, outs, builder, state);
            checked(builder.build_call(function, &args, ""), "direct call");
        }
    }

    /// C-ABI arguments for an `extern` call: scalar `in` places are loaded and
    /// passed by value; a `ref T` parameter gets a pointer (the load of a `ref`
    /// place, or the address of a non-`ref` one). A variadic extern's extra
    /// places are loaded at their own type (a `ref` place yields its pointer).
    fn extern_call_args(
        &self,
        callee: &LirFunc,
        ins: &[PlaceName],
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> Vec<BasicMetadataValueEnum<'ctx>> {
        ins.iter()
            .zip(&callee.ins)
            .map(|(place, (_, param_ty))| {
                if param_ty.is_ref() {
                    let arg_is_ref = state.place_types.get(place).is_some_and(LirTy::is_ref);
                    if arg_is_ref {
                        BasicMetadataValueEnum::from(self.load_place(place, builder, state))
                    } else {
                        BasicMetadataValueEnum::from(place_ptr(place, state))
                    }
                } else {
                    BasicMetadataValueEnum::from(self.load_as(place, param_ty, builder, state))
                }
            })
            .chain(
                ins.iter().skip(callee.ins.len()).map(|place| {
                    BasicMetadataValueEnum::from(self.load_place(place, builder, state))
                }),
            )
            .collect()
    }

    /// The address of every listed place (pointer ABI), inputs then outputs,
    /// coercing a value place passed to a `ref` parameter.
    ///
    /// A `ref #T` parameter's incoming pointer is the address of a reference
    /// *slot* (the callee loads the `#T` pointer out of it). When the caller
    /// lists a plain `#T` place — as the RAII `@on_drop` call does — the
    /// argument's own address is the reference, so it is spilled into a fresh
    /// pointer slot before being passed.
    fn direct_call_args(
        &self,
        callee: &LirFunc,
        ins: &[PlaceName],
        outs: &[PlaceName],
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> Vec<BasicMetadataValueEnum<'ctx>> {
        let params = callee.ins.iter().chain(&callee.outs).map(|(_, ty)| ty);
        ins.iter()
            .chain(outs.iter())
            .zip(params)
            .map(|(place, param_ty)| {
                let ptr = place_ptr(place, state);
                let arg_is_ref = state.place_types.get(place).is_some_and(LirTy::is_ref);
                if param_ty.is_ref() && !arg_is_ref {
                    let slot = checked(
                        builder.build_alloca(self.ptr_type(), "ref.arg"),
                        "ref arg alloca",
                    );
                    checked(builder.build_store(slot, ptr), "ref arg store");
                    BasicMetadataValueEnum::from(slot)
                } else {
                    BasicMetadataValueEnum::from(ptr)
                }
            })
            .collect()
    }

    /// An indirect call: `%reg` holds the callee (a function pointer, or its
    /// integer normalization which is `inttoptr`ed back). The `void(ptr…)`
    /// type comes from the call's place count.
    fn lower_indirect_call(
        &self,
        reg: &Reg,
        ins: &[PlaceName],
        outs: &[PlaceName],
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) {
        let Some(value) = state.regs.get(reg).copied() else {
            bug!(Span::dummy(), "call through undefined register `{reg}`");
        };
        let ptr = match value {
            BasicValueEnum::PointerValue(ptr) => ptr,
            BasicValueEnum::IntValue(int) => checked(
                builder.build_int_to_ptr(int, self.ptr_type(), ""),
                "inttoptr",
            ),
            other => bug!(
                Span::dummy(),
                "indirect callee `{reg}` is a `{}`, not a pointer or integer",
                other.get_type().print_to_string()
            ),
        };
        let arity = ins.len() + outs.len();
        let param_types = vec![self.ptr_type().into(); arity];
        let fn_type = self.context.void_type().fn_type(&param_types, false);
        let args = place_args(ins, outs, state);
        checked(
            builder.build_indirect_call(fn_type, ptr, &args, ""),
            "indirect call",
        );
    }

    /// A builtin call uses the value ABI: each input place is loaded, the
    /// declared runtime symbol (from `builtin_sig`) is called, and the single
    /// result is stored back into the out place. The use-scan in
    /// `used_builtins` guarantees the runtime module has already defined the
    /// leaf this call names, so the existing function is reused rather than
    /// re-declared.
    fn lower_builtin_call(
        &self,
        name: &str,
        ins: &[PlaceName],
        outs: &[PlaceName],
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) {
        let Some(sig) = builtin_sig(name) else {
            bug!(Span::dummy(), "call to unknown builtin `{name}`");
        };
        let params: Vec<BasicMetadataTypeEnum<'ctx>> = sig
            .ins
            .iter()
            .map(|slot| BasicMetadataTypeEnum::from(self.lir_type(slot)))
            .collect();
        let fn_type = match (outs.len(), sig.outs.first()) {
            (0, _) => self.context.void_type().fn_type(&params, false),
            (1, Some(result)) => self.lir_type(result).fn_type(&params, false),
            _ => bug!(
                Span::dummy(),
                "builtin `{name}` has {} out places, at most one is supported",
                outs.len()
            ),
        };
        let symbol = builtin_symbol(name);
        // The use-scan in `used_builtins` has the runtime emit every leaf this
        // module calls, so the symbol must already exist. A miss is scan/emit
        // drift — an ICE, never a quiet external declaration.
        let Some(function) = self.module.get_function(symbol) else {
            bug!(
                Span::dummy(),
                "builtin `{name}` symbol `{symbol}` was not emitted by the use-scan; expected `{}`",
                fn_type.print_to_string()
            );
        };
        let args: Vec<BasicMetadataValueEnum<'ctx>> = ins
            .iter()
            .map(|place| self.load_place(place, builder, state).into())
            .collect();
        let call = checked(builder.build_call(function, &args, ""), "builtin call");
        if let Some(out) = outs.first() {
            let value = match call.try_as_basic_value() {
                ValueKind::Basic(value) => value,
                ValueKind::Instruction(_) => {
                    bug!(Span::dummy(), "builtin `{name}` unexpectedly returned void")
                }
            };
            checked(
                builder.build_store(place_ptr(out, state), value),
                "builtin result store",
            );
        }
    }

    /// Declares (once) `void @name(...)` with `fn_type`, reusing any existing
    /// definition or declaration.
    pub(crate) fn runtime_fn(
        &self,
        name: &str,
        fn_type: FunctionType<'ctx>,
    ) -> FunctionValue<'ctx> {
        match self.module.get_function(name) {
            Some(function) => function,
            None => self.module.add_function(name, fn_type, None),
        }
    }

    /// Resolves an operand to an SSA value: a register's value, or a load of a
    /// place (using `ty` when the instruction names it, else the place's own
    /// declared type).
    fn operand(
        &self,
        operand: &LirOperand,
        ty: Option<&LirTy>,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match operand {
            LirOperand::Reg(name) => match state.regs.get(name).copied() {
                Some(value) => value,
                None => bug!(Span::dummy(), "use of undefined register `{name}`"),
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
        name: &PlaceName,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let Some(ty) = state.place_types.get(name).cloned() else {
            bug!(Span::dummy(), "load of undeclared place `{name}`");
        };
        self.load_as(name, &ty, builder, state)
    }

    /// Loads place `name` as the given LIR type.
    fn load_as(
        &self,
        name: &PlaceName,
        ty: &LirTy,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        // A `ref` place holds a pointer; reading a non-`ref` value out of it
        // follows that pointer to the pointee first.
        let ptr = place_ptr(name, state);
        let ptr = match state.place_types.get(name) {
            Some(place_ty) if place_ty.is_ref() && !ty.is_ref() => {
                checked(builder.build_load(self.ptr_type(), ptr, ""), "load ref")
                    .into_pointer_value()
            }
            _ => ptr,
        };
        checked(builder.build_load(self.lir_type(ty), ptr, ""), "load")
    }

    /// GEPs to field `idx` of place `obj`, loading the object pointer first
    /// when the place holds a `ref`.
    fn field_ptr(
        &self,
        obj: &PlaceName,
        idx: u32,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> PointerValue<'ctx> {
        let Some(place_ty) = state.place_types.get(obj).cloned() else {
            bug!(Span::dummy(), "field of undeclared place `{obj}`");
        };
        let owner = place_ty.strip_ref();
        let BasicTypeEnum::StructType(struct_ty) = self.lir_type(owner) else {
            bug!(Span::dummy(), "field of non-struct place `{obj}`");
        };
        let base = if place_ty.is_ref() {
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

    /// `elem.addr`: the array's base plus `index` times the element size,
    /// in integer arithmetic (no dynamic GEP).
    fn lower_elem_addr(&self, inst: &LirInst, builder: &Builder<'ctx>, state: &mut FnState<'ctx>) {
        let LirInst::ElemAddr {
            array,
            index,
            dst,
            ty,
            ..
        } = inst
        else {
            bug!(Span::dummy(), "non-`elem.addr` instruction `{inst:?}`");
        };
        let ptr_bytes = u64::from(self.native.get_bit_width() / 8);
        let (size, _) = lilac_ir::size_align(&self.lir.layouts, ty.strip_ref(), ptr_bytes);
        let offset = checked(
            builder.build_int_mul(
                reg_int(index, state),
                self.native.const_int(size, false),
                "",
            ),
            "element offset",
        );
        let elem = crate::runtime::byte_ptr(self, builder, reg_ptr(array, state), offset);
        state.regs.insert(dst.clone(), elem.into());
    }

    /// Materializes a `const`: a data-symbol address (or `#byte_buffer`
    /// wrapper), a float immediate, or an integer/pointer immediate. The
    /// verifier has checked the value against its type.
    fn const_value(&self, value: &LirConst, ty: &LirTy) -> BasicValueEnum<'ctx> {
        match (value, self.lir_type(ty)) {
            (LirConst::Data(name), _) => self.data_const(name, ty),
            (LirConst::Float(value), BasicTypeEnum::FloatType(float_ty)) => {
                float_ty.const_float(*value).into()
            }
            (LirConst::Int(value), BasicTypeEnum::IntType(int_ty)) => {
                int_ty.const_int(*value, false).into()
            }
            (LirConst::Int(0), BasicTypeEnum::PointerType(ptr_ty)) => ptr_ty.const_null().into(),
            (value, other) => bug!(
                Span::dummy(),
                "`const {value}` cannot be a `{}` value",
                other.print_to_string()
            ),
        }
    }

    /// The value of a data-symbol `const`: `#byte_buffer` wraps `{len, ptr}`,
    /// a reference type is the bare global address.
    fn data_const(&self, name: &Symbol, ty: &LirTy) -> BasicValueEnum<'ctx> {
        let Some(global) = self.module.get_global(name.as_str()) else {
            bug!(
                Span::dummy(),
                "const of undeclared data `!{}`",
                name.as_str()
            );
        };
        let ptr = global.as_pointer_value();
        if *ty == LirTy::byte_buffer() {
            let len = self.data_len(name.as_str());
            let len_value = self.native.const_int(len, false);
            self.context
                .const_struct(&[len_value.into(), ptr.into()], false)
                .into()
        } else if ty.is_ref() {
            ptr.into()
        } else {
            // The LIR verifier's `check_const_lit` accepts `#byte_buffer`,
            // `#format_args`, and `ref #bit8` data-symbol consts. A `[N x i8]`
            // global can only become a `#byte_buffer` wrapper or a bare
            // reference; `#format_args` (a seventeen-field piece table) has no
            // meaningful lowering and no `lilac-lower` path emits one.
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
            Some(datum) => u64::try_from(datum.bytes.len()).unwrap_or(u64::MAX),
            None => bug!(Span::dummy(), "const of undeclared data `!{name}`"),
        }
    }

    /// Lowers one arithmetic/comparison op. Integer ops work on bit containers
    /// directly; float ops work on real `half`/`float`/`double`/`fp128` values.
    /// Float comparisons widen their `i1` result to `#bit8`.
    fn arith(
        &self,
        op: &ArithOp,
        lhs: &Reg,
        rhs: &Reg,
        ty: &LirTy,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match op {
            ArithOp::IAdd
            | ArithOp::ISub
            | ArithOp::IMul
            | ArithOp::IDiv
            | ArithOp::IDivU
            | ArithOp::IMod
            | ArithOp::IModU => {
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
                    ArithOp::IDivU => builder.build_int_unsigned_div(left, right, ""),
                    ArithOp::IMod => builder.build_int_signed_rem(left, right, ""),
                    _ => builder.build_int_unsigned_rem(left, right, ""),
                };
                checked(value, "integer arith").into()
            }
            ArithOp::Eq => {
                // `==` is strict bit-equality (SPEC §8) even for floats, so a
                // float operand is reinterpreted to the same-width integer and
                // compared with `icmp eq`; `fcmp` would compare values.
                let left = self.eq_operand(lhs, builder, state);
                let right = self.eq_operand(rhs, builder, state);
                let cmp = checked(
                    builder.build_int_compare(IntPredicate::EQ, left, right, ""),
                    "icmp",
                );
                self.widen_bool(cmp, ty, builder)
            }
            ArithOp::ILt
            | ArithOp::ILtU
            | ArithOp::IGt
            | ArithOp::IGtU
            | ArithOp::ILe
            | ArithOp::ILeU
            | ArithOp::IGe
            | ArithOp::IGeU => {
                let left = reg_int(lhs, state);
                let right = reg_int(rhs, state);
                let predicate = match op {
                    ArithOp::ILt => IntPredicate::SLT,
                    ArithOp::ILtU => IntPredicate::ULT,
                    ArithOp::IGtU => IntPredicate::UGT,
                    ArithOp::ILe => IntPredicate::SLE,
                    ArithOp::ILeU => IntPredicate::ULE,
                    ArithOp::IGe => IntPredicate::SGE,
                    ArithOp::IGeU => IntPredicate::UGE,
                    _ => IntPredicate::SGT,
                };
                let cmp = checked(
                    builder.build_int_compare(predicate, left, right, ""),
                    "icmp",
                );
                self.widen_bool(cmp, ty, builder)
            }
            ArithOp::FAdd | ArithOp::FSub | ArithOp::FMul | ArithOp::FDiv | ArithOp::FMod => {
                let left = reg_float(lhs, state);
                let right = reg_float(rhs, state);
                if !matches!(self.lir_type(ty), BasicTypeEnum::FloatType(_)) {
                    bug!(Span::dummy(), "`{op:?}` needs a float type, found `{ty}`");
                }
                let value = match op {
                    ArithOp::FAdd => builder.build_float_add(left, right, ""),
                    ArithOp::FSub => builder.build_float_sub(left, right, ""),
                    ArithOp::FMul => builder.build_float_mul(left, right, ""),
                    ArithOp::FDiv => builder.build_float_div(left, right, ""),
                    _ => builder.build_float_rem(left, right, ""),
                };
                checked(value, "float arith").into()
            }
            ArithOp::FLt | ArithOp::FGt | ArithOp::FLe | ArithOp::FGe => {
                let left = reg_float(lhs, state);
                let right = reg_float(rhs, state);
                let predicate = match op {
                    ArithOp::FLt => FloatPredicate::OLT,
                    ArithOp::FLe => FloatPredicate::OLE,
                    ArithOp::FGe => FloatPredicate::OGE,
                    _ => FloatPredicate::OGT,
                };
                let cmp = checked(
                    builder.build_float_compare(predicate, left, right, ""),
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
        ty: &LirTy,
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

    /// Reads an `==` operand as an integer for strict bit-equality. A float
    /// register is reinterpreted to the same-width integer first, so `==`
    /// compares bits uniformly (SPEC §8) rather than using `fcmp`.
    fn eq_operand(
        &self,
        name: &Reg,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> IntValue<'ctx> {
        match state.regs.get(name).copied() {
            Some(BasicValueEnum::IntValue(value)) => value,
            Some(BasicValueEnum::PointerValue(value)) => {
                checked(builder.build_ptr_to_int(value, self.native, ""), "ptrtoint")
            }
            Some(BasicValueEnum::StructValue(value)) => {
                // Aggregates compare as one integer over their bytes; the slot
                // is zeroed first so padding is deterministic.
                let ty = value.get_type();
                let layout = self.module.get_data_layout();
                let data = TargetData::create(layout.as_str().to_str().unwrap_or_default());
                let bits = u32::try_from(data.get_abi_size(&ty) * 8).unwrap_or(u32::MAX);
                let slot = self.entry_alloca(ty, builder);
                let Some(size) = ty.size_of() else {
                    bug!(Span::dummy(), "comparison of an unsized aggregate");
                };
                let zero = self.context.i8_type().const_zero();
                checked(builder.build_memset(slot, 1, zero, size), "eq slot zero");
                checked(builder.build_store(slot, value), "eq slot store");
                let Some(int_ty) = std::num::NonZeroU32::new(bits)
                    .and_then(|bits| self.context.custom_width_int_type(bits).ok())
                else {
                    bug!(Span::dummy(), "comparison of a zero-sized aggregate");
                };
                checked(builder.build_load(int_ty, slot, ""), "eq slot load").into_int_value()
            }
            Some(BasicValueEnum::FloatValue(value)) => {
                let int_ty = self.int_type_of_width(value.get_type().get_bit_width());
                match checked(builder.build_bit_cast(value, int_ty, ""), "float bitcast") {
                    BasicValueEnum::IntValue(bits) => bits,
                    other => bug!(
                        Span::dummy(),
                        "float bitcast produced a `{}`",
                        other.get_type().print_to_string()
                    ),
                }
            }
            Some(other) => bug!(
                Span::dummy(),
                "register `{name}` is a `{}`, not comparable",
                other.get_type().print_to_string()
            ),
            None => bug!(Span::dummy(), "use of undefined register `{name}`"),
        }
    }

    /// The integer type of `width` bits (16/32/64/128), used to reinterpret a
    /// float operand for bit-equality.
    fn int_type_of_width(&self, width: u32) -> IntType<'ctx> {
        match width {
            16 => self.context.i16_type(),
            32 => self.context.i32_type(),
            64 => self.context.i64_type(),
            128 => self.context.i128_type(),
            _ => bug!(
                Span::dummy(),
                "no integer type for a {width}-bit float operand"
            ),
        }
    }

    /// Lowers a block terminator. `return` → `ret void`; `branch` → `br`;
    /// `branch.if` → `br i1` (the false edge is the next block in layout order
    /// when `els` is absent); `unreachable` → `unreachable`.
    fn lower_term(
        term: &LirTerm,
        builder: &Builder<'ctx>,
        blocks: &HashMap<&str, BasicBlock<'ctx>>,
        state: &FnState<'ctx>,
        next: Option<BasicBlock<'ctx>>,
    ) {
        match term {
            LirTerm::Return { .. } => {
                checked(builder.build_return(None), "return");
            }
            LirTerm::Unreachable { .. } => {
                checked(builder.build_unreachable(), "unreachable");
            }
            LirTerm::Branch { target, .. } => {
                let block = block_at(blocks, target.as_str());
                checked(builder.build_unconditional_branch(block), "branch");
            }
            LirTerm::BranchIf {
                cond, then, els, ..
            } => {
                let then_block = block_at(blocks, then.as_str());
                let else_block = match els {
                    Some(els) => block_at(blocks, els.as_str()),
                    None => match next {
                        Some(next) => next,
                        None => bug!(
                            Span::dummy(),
                            "`branch.if` without `else` has no fall-through block"
                        ),
                    },
                };
                let condition = cond_value(cond, builder, state);
                checked(
                    builder.build_conditional_branch(condition, then_block, else_block),
                    "branch.if",
                );
            }
        }
    }
}

/// The LLVM symbol a LIR function emits under. LIR `main` is renamed
/// `.lilac.main` so the generated C `main` wrapper can take the real entry
/// symbol; every other function keeps its name. Calls and `addr.func` are
/// mapped through this too, so references to `main` stay consistent.
pub(crate) fn symbol(name: &str) -> &str {
    if name == "main" {
        ".lilac.main"
    } else {
        name
    }
}

/// The LLVM symbol a `@`-prefixed leaf LIR builtin emits under. `@memcpy`
/// becomes `.lilac.memcpy` so its in-module definition cannot collide with the
/// host C library's `memcpy`; `@ptr_add`/`@lilac_abort`/`@syscall` map to
/// their `.lilac.` leaves (`syscall` is also a C library function).
fn builtin_symbol(name: &str) -> &str {
    match name {
        "@memcpy" => ".lilac.memcpy",
        "@memmove" => ".lilac.memmove",
        "@memset" => ".lilac.memset",
        "@ptr_add" => ".lilac.ptr_add",
        "@lilac_abort" => ".lilac.abort",
        "@syscall" => ".lilac.syscall",
        "@trace_head" => ".lilac.trace_head",
        other => other.strip_prefix('@').unwrap_or(other),
    }
}

/// Returns a place's pointer, raising an ICE if it was never declared.
fn place_ptr<'ctx>(name: &PlaceName, state: &FnState<'ctx>) -> PointerValue<'ctx> {
    match state.places.get(name).copied() {
        Some(ptr) => ptr,
        None => bug!(Span::dummy(), "use of undeclared place `{name}`"),
    }
}

/// The address of every listed place, inputs then outputs (pointer ABI).
fn place_args<'ctx>(
    ins: &[PlaceName],
    outs: &[PlaceName],
    state: &FnState<'ctx>,
) -> Vec<BasicMetadataValueEnum<'ctx>> {
    ins.iter()
        .chain(outs.iter())
        .map(|place| BasicMetadataValueEnum::from(place_ptr(place, state)))
        .collect()
}

/// Reads a `branch.if` condition register as an `i1`: an `i1` is used
/// directly, any wider bit container is tested against zero.
fn cond_value<'ctx>(reg: &Reg, builder: &Builder<'ctx>, state: &FnState<'ctx>) -> IntValue<'ctx> {
    let Some(value) = state.regs.get(reg).copied() else {
        bug!(Span::dummy(), "branch on undefined register `{reg}`");
    };
    match value {
        BasicValueEnum::IntValue(int) if int.get_type().get_bit_width() == 1 => int,
        BasicValueEnum::IntValue(int) => {
            let zero = int.get_type().const_zero();
            checked(
                builder.build_int_compare(IntPredicate::NE, int, zero, ""),
                "branch condition",
            )
        }
        other => bug!(
            Span::dummy(),
            "branch condition `{reg}` is a `{}`, not an integer",
            other.get_type().print_to_string()
        ),
    }
}

/// Looks up a branch target, raising an ICE when it is unknown.
fn block_at<'ctx>(blocks: &HashMap<&str, BasicBlock<'ctx>>, label: &str) -> BasicBlock<'ctx> {
    match blocks.get(label) {
        Some(block) => *block,
        None => bug!(Span::dummy(), "branch to unknown block `${label}`"),
    }
}

/// Reads a register as an integer value.
/// The LLVM ordering of a LIR atomic ordering.
fn llvm_ordering(order: AtomicOrdering) -> inkwell::AtomicOrdering {
    match order {
        AtomicOrdering::Relaxed => inkwell::AtomicOrdering::Monotonic,
        AtomicOrdering::Acquire => inkwell::AtomicOrdering::Acquire,
        AtomicOrdering::Release => inkwell::AtomicOrdering::Release,
        AtomicOrdering::AcqRel => inkwell::AtomicOrdering::AcquireRelease,
        AtomicOrdering::SeqCst => inkwell::AtomicOrdering::SequentiallyConsistent,
    }
}

/// The LLVM `atomicrmw` operation of a LIR one.
fn rmw_bin_op(op: RmwOp) -> AtomicRMWBinOp {
    match op {
        RmwOp::Swap => AtomicRMWBinOp::Xchg,
        RmwOp::Add => AtomicRMWBinOp::Add,
        RmwOp::Sub => AtomicRMWBinOp::Sub,
        RmwOp::And => AtomicRMWBinOp::And,
        RmwOp::Or => AtomicRMWBinOp::Or,
        RmwOp::Xor => AtomicRMWBinOp::Xor,
        RmwOp::Max => AtomicRMWBinOp::Max,
        RmwOp::Min => AtomicRMWBinOp::Min,
        RmwOp::MaxU => AtomicRMWBinOp::UMax,
        RmwOp::MinU => AtomicRMWBinOp::UMin,
    }
}

fn reg_ptr<'ctx>(name: &Reg, state: &FnState<'ctx>) -> PointerValue<'ctx> {
    match state.regs.get(name).copied() {
        Some(BasicValueEnum::PointerValue(value)) => value,
        Some(other) => bug!(
            Span::dummy(),
            "register `{name}` is a `{}`, not a pointer",
            other.get_type().print_to_string()
        ),
        None => bug!(Span::dummy(), "use of undefined register `{name}`"),
    }
}

fn reg_int<'ctx>(name: &Reg, state: &FnState<'ctx>) -> IntValue<'ctx> {
    match state.regs.get(name).copied() {
        Some(BasicValueEnum::IntValue(value)) => value,
        Some(other) => bug!(
            Span::dummy(),
            "register `{name}` is a `{}`, not an integer",
            other.get_type().print_to_string()
        ),
        None => bug!(Span::dummy(), "use of undefined register `{name}`"),
    }
}

/// Reads a register as a float value.
fn reg_float<'ctx>(name: &Reg, state: &FnState<'ctx>) -> FloatValue<'ctx> {
    match state.regs.get(name).copied() {
        Some(BasicValueEnum::FloatValue(value)) => value,
        Some(other) => bug!(
            Span::dummy(),
            "register `{name}` is a `{}`, not a float",
            other.get_type().print_to_string()
        ),
        None => bug!(Span::dummy(), "use of undefined register `{name}`"),
    }
}
