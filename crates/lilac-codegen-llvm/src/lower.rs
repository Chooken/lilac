//! LIR → LLVM lowering: types, layouts, data globals, and places.
//!
//! Task 2 of M5b covers the type map, named layouts, data globals, the pointer
//! calling convention, and zero-initialized `alloca` places. Instruction bodies
//! (beyond `alloca`) and non-`return` terminators land in later M5b tasks; the
//! placeholders here keep every emitted module verifiable meanwhile.

use std::collections::HashMap;

use inkwell::basic_block::BasicBlock;
use inkwell::builder::{Builder, BuilderError};
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::{BasicTypeEnum, IntType, PointerType, StructType};
use inkwell::values::PointerValue;
use inkwell::AddressSpace;

use lilac_error::bug;
use lilac_ir::{LirDataKind, LirFunc, LirInst, LirModule, LirTerm};
use lilac_span::Span;

/// Turns a fallible inkwell builder call into an ICE.
fn checked<T>(result: Result<T, BuilderError>, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(err) => bug!(Span::dummy(), "failed to build {what}: {err}"),
    }
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
        let mut places: HashMap<String, PointerValue<'ctx>> = HashMap::new();
        for (index, (place, _)) in func.ins.iter().enumerate() {
            if let Some(value) = params.get(index).copied() {
                value.set_name(&format!("in{index}"));
                places.insert(place.clone(), value.into_pointer_value());
            }
        }
        for (index, (place, _)) in func.outs.iter().enumerate() {
            let slot = func.ins.len() + index;
            if let Some(value) = params.get(slot).copied() {
                value.set_name(&format!("out{index}"));
                places.insert(place.clone(), value.into_pointer_value());
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
                self.lower_inst(inst, &builder, &mut places);
            }
            lower_term(&block.term, &builder, &blocks);
        }
    }

    /// Lowers the instructions M5b Task 2 owns; later tasks extend this match.
    fn lower_inst(
        &self,
        inst: &LirInst,
        builder: &Builder<'ctx>,
        places: &mut HashMap<String, PointerValue<'ctx>>,
    ) {
        if let LirInst::Alloca { ty, place, .. } = inst {
            let value_ty = self.lir_type(ty);
            let ptr = checked(builder.build_alloca(value_ty, place), "alloca");
            let zero = value_ty.const_zero();
            checked(builder.build_store(ptr, zero), "zero-init store");
            places.insert(place.clone(), ptr);
        }
    }
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
