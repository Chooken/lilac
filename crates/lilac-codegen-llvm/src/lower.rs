//! LIR → LLVM lowering: types, layouts, data globals, places, instructions, and
//! terminators.
//!
//! Covers the type map, named layouts, data globals, the pointer calling
//! convention, zero-initialized `alloca` places, every value / memory /
//! conversion / constant / arithmetic instruction, direct/indirect/builtin
//! calls, `drop`, and control flow (`branch`, `branch.if`, `return`, `panic`,
//! `unreachable`). Builtin calls use the value ABI and reuse the runtime
//! definitions the `runtime` module emits before any user function.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::{Builder, BuilderError};
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::{
    BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, IntType, PointerType,
};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FloatValue, FunctionValue,
    InstructionValue, IntValue, PointerValue, ValueKind,
};
use inkwell::{AddressSpace, AtomicRMWBinOp, FloatPredicate, IntPredicate};

use lilac_codegen::Entry;
use lilac_error::{bug, codes, Diagnostic};
use lilac_ir::{
    builtin_layout, builtin_sig, format_template_layout, AddressTarget, ArithOp, AtomicOrdering,
    ConvertOp, FloatWidth, LirCallee, LirConst, LirExport, LirFunc, LirGlobal, LirInst, LirLayout,
    LirModule, LirOperand, LirTerm, LirTy, PlaceName, Reg, RmwOp, Symbol,
};
use lilac_span::Span;
use lilac_target::{is_64_bit_triple, Arch, Os, Scalar, Target};

/// Turns a fallible inkwell builder call in generated glue (which has no
/// span) into an ICE. Lowering LIR uses [`Lower::checked`].
pub(crate) fn checked<T>(result: Result<T, BuilderError>, what: &str) -> T {
    checked_at(result, Span::dummy(), what)
}

/// Turns a fallible inkwell builder call into an ICE at `span`.
fn checked_at<T>(result: Result<T, BuilderError>, span: Span, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(err) => bug!(span, "failed to build {what}: {err}"),
    }
}

/// The low 64 bits of `value` (an integer constant's words are 64-bit).
#[allow(
    clippy::cast_possible_truncation,
    reason = "dropping the high half is the point"
)]
fn low_word(value: u128) -> u64 {
    value as u64
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
    os: Option<Os>,
    /// `.lilac.offset`, once a field or element address has needed it
    /// ([`Self::offset_ptr`]).
    offset: Cell<Option<FunctionValue<'ctx>>>,
    /// The span of the LIR instruction or terminator being lowered, which
    /// an ICE points at; `Span::dummy()` outside one (declarations, and
    /// glue the backend writes itself).
    at: Cell<Span>,
    /// Each `export`'s C entry symbol, by its LIR name: its address is the
    /// entry's ([`Self::function_address`]).
    exports: HashMap<&'a str, &'a str>,
}

impl<'ctx, 'a> Lower<'ctx, 'a> {
    /// Creates a lowering context over `module` for `triple`, which picks
    /// `iNative` and the `@linux_syscall` instruction.
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
            os: Target::parse(triple).ok().map(|target| target.os),
            offset: Cell::new(None),
            at: Cell::new(Span::dummy()),
            exports: lir
                .funcs
                .iter()
                .filter_map(|func| {
                    Some((func.name.as_str(), func.export.as_ref()?.symbol.as_str()))
                })
                .collect(),
        }
    }

    /// Where an ICE points: the LIR instruction or terminator being lowered
    /// ([`Self::at`]).
    fn at(&self) -> Span {
        self.at.get()
    }

    /// [`checked`] while lowering LIR: the ICE points at [`Self::at`].
    fn checked<T>(&self, result: Result<T, BuilderError>, what: &str) -> T {
        checked_at(result, self.at(), what)
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

    /// The target's operating system, when the triple parses.
    pub(crate) fn os(&self) -> Option<Os> {
        self.os
    }

    /// The target's `iNative` integer width.
    pub(crate) fn native_int(&self) -> IntType<'ctx> {
        self.native
    }

    /// Lowers data globals, global variables, the in-module runtime, and
    /// functions into the module, in that fixed order. Every function is
    /// declared before any body is lowered so calls and `addr.func` may
    /// reference functions defined later, and the runtime is defined first so
    /// builtin calls reuse it. Templates and the addresses in globals'
    /// initial values are filled in once every function and global is
    /// declared. Aggregates are byte arrays to LLVM
    /// ([`Self::lir_type`]), so no layout is defined.
    ///
    /// An `extern` signature outside the supported C scalar/pointer set (or
    /// with more than one out) is `E028`.
    pub(crate) fn run(&mut self, entry: Entry) -> Result<(), Diagnostic> {
        self.define_data();
        self.define_globals();
        self.emit_runtime();
        for func in &self.lir.funcs {
            self.declare_func(func)?;
        }
        self.define_formats();
        self.fill_global_addresses();
        for func in &self.lir.funcs {
            self.lower_func(func);
            if let Some(export) = &func.export {
                self.emit_export(func, export);
            }
        }
        self.emit_entry_wrapper(entry);
        Ok(())
    }

    /// Appends the process entry when the LIR has the entry stub
    /// `.lilac.start` (an executable), passing it the argument count, the
    /// argument vector and the environment vector:
    ///
    /// - a C entry: `i32 main(i32 argc, ptr argv, ptr envp)`;
    /// - native macOS: `.lilac.native_entry`, the same C `main` shape
    ///   (`LC_MAIN`);
    /// - native Windows: `void .lilac.native_entry()`, passing zero and
    ///   nulls (the command line is read through `kernel32`);
    /// - native Linux: `void .lilac.native_main(iN count, ptr, ptr)`, which
    ///   the assembly entry `.lilac.native_entry` calls with what the kernel
    ///   left on the stack ([`crate::native`]).
    ///
    /// Native Linux first gives the main thread its thread-local block when
    /// the module has thread-locals ([`crate::threads::main_tls`]).
    ///
    /// The stub ends the process through `@exit` and never returns, so the
    /// wrapper ends `unreachable`. A module without the stub gets none.
    fn emit_entry_wrapper(&self, entry: Entry) {
        let Some(start) = self.module.get_function(".lilac.start") else {
            return;
        };
        let i32_ty = self.context.i32_type();
        let ptr = self.ptr_type();
        let c_main = i32_ty.fn_type(&[i32_ty.into(), ptr.into(), ptr.into()], false);
        let (name, ty) = match entry {
            Entry::CMain => ("main", c_main),
            Entry::Native if self.os == Some(Os::Macos) => (".lilac.native_entry", c_main),
            Entry::Native if self.os == Some(Os::Windows) => (
                ".lilac.native_entry",
                self.context.void_type().fn_type(&[], false),
            ),
            Entry::Native => (
                ".lilac.native_main",
                self.context
                    .void_type()
                    .fn_type(&[self.native.into(), ptr.into(), ptr.into()], false),
            ),
        };
        let wrapper = self.module.add_function(name, ty, None);
        let builder = self.context.create_builder();
        let block = self.context.append_basic_block(wrapper, "entry");
        builder.position_at_end(block);
        if entry == Entry::Native
            && self.os == Some(Os::Linux)
            && crate::threads::has_thread_locals(&self.module)
        {
            let main_tls = crate::threads::main_tls(&self.module, self.arch);
            self.checked(builder.build_call(main_tls, &[], ""), "main thread TLS");
        }
        let (count, arguments, environment) = match wrapper.get_params().as_slice() {
            [count, arguments, environment] => (
                count.into_int_value(),
                arguments.into_pointer_value(),
                environment.into_pointer_value(),
            ),
            _ => (self.native.const_zero(), ptr.const_null(), ptr.const_null()),
        };
        let count = if count.get_type() == self.native {
            count
        } else {
            self.checked(
                builder.build_int_z_extend(count, self.native, "count"),
                "entry count",
            )
        };
        let mut slots = Vec::new();
        for value in [count.into(), arguments.into(), environment.into()] {
            let value: BasicValueEnum = value;
            let slot = self.checked(builder.build_alloca(value.get_type(), ""), "entry slot");
            self.store(&builder, slot, value, "entry store");
            slots.push(slot.into());
        }
        self.checked(builder.build_call(start, &slots, ""), "entry call");
        self.checked(builder.build_unreachable(), "entry end");
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

    /// Maps a LIR type to its LLVM type. An aggregate (a named layout or an
    /// array) is its bytes, `[size x i8]`: the compiler owns every layout,
    /// so LLVM never computes one, and a field is its offset from the base
    /// ([`Self::field_ptr`]).
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
            LirTy::Named(_) | LirTy::Array(..) => {
                match lilac_ir::size_align(&self.lir.layouts, ty, &self.lir.target) {
                    Some(sized) => self.bytes_type(sized.size),
                    None => bug!(self.at(), "unknown LIR type `{ty}`"),
                }
            }
            other => bug!(self.at(), "unknown LIR type `{other}`"),
        }
    }

    /// `[size x i8]`. LLVM counts array elements in 32 bits, so a larger
    /// object is whole chunks of 2^31 bytes followed by the rest, packed.
    pub(crate) fn bytes_type(&self, size: u64) -> BasicTypeEnum<'ctx> {
        const CHUNK: u32 = 1 << 31;
        let byte = self.context.i8_type();
        if let Ok(size) = u32::try_from(size) {
            return byte.array_type(size).into();
        }
        let chunks = u32::try_from(size / u64::from(CHUNK))
            .unwrap_or_else(|_| bug!(self.at(), "an object of {size} bytes is past LLVM's limit"));
        let rest = u32::try_from(size % u64::from(CHUNK)).unwrap_or(0);
        let chunk = byte.array_type(CHUNK);
        self.context
            .struct_type(
                &[
                    chunk.array_type(chunks).into(),
                    byte.array_type(rest).into(),
                ],
                true,
            )
            .into()
    }

    /// The size and alignment of a LIR type, read from the module's layouts
    /// (never computed here); the verifier checked every type is sized.
    fn size_align(&self, ty: &LirTy) -> lilac_layout::SizeAlign {
        lilac_ir::size_align(&self.lir.layouts, ty, &self.lir.target)
            .unwrap_or_else(|| bug!(self.at(), "`{ty}` has no size on `{}`", self.lir.target))
    }

    /// The alignment of a LIR type, as LLVM takes it.
    fn align_of(&self, ty: &LirTy) -> u32 {
        let align = self.size_align(ty).align;
        u32::try_from(align)
            .unwrap_or_else(|_| bug!(self.at(), "`{ty}` is aligned to {align} bytes"))
    }

    /// The alignment of a scalar LLVM type: its C scalar's on the target
    /// (`lilac_target::Target::scalar_layout`), the table every layout is
    /// built from.
    fn scalar_align(&self, ty: BasicTypeEnum<'ctx>) -> u32 {
        let scalar = match ty {
            BasicTypeEnum::IntType(int) => Scalar::bit(int.get_bit_width()),
            BasicTypeEnum::FloatType(float) => [
                (16, self.context.f16_type()),
                (32, self.context.f32_type()),
                (64, self.context.f64_type()),
                (128, self.context.f128_type()),
            ]
            .into_iter()
            .find(|(_, known)| *known == float)
            .and_then(|(bits, _)| Scalar::float(bits)),
            BasicTypeEnum::PointerType(_) => Some(Scalar::Pointer),
            _ => None,
        };
        let Some(scalar) = scalar else {
            bug!(self.at(), "`{}` is not a scalar", ty.print_to_string());
        };
        let align = self.lir.target.scalar_layout(scalar).1;
        u32::try_from(align).unwrap_or(1)
    }

    /// Loads a scalar of `ty` from `ptr`, aligned to it.
    fn load(
        &self,
        builder: &Builder<'ctx>,
        ty: BasicTypeEnum<'ctx>,
        ptr: PointerValue<'ctx>,
        what: &str,
    ) -> BasicValueEnum<'ctx> {
        let value = self.checked(builder.build_load(ty, ptr, ""), what);
        self.aligned(value.as_instruction_value(), self.scalar_align(ty), what);
        value
    }

    /// Stores the scalar `value` to `ptr`, aligned to its type.
    fn store(
        &self,
        builder: &Builder<'ctx>,
        ptr: PointerValue<'ctx>,
        value: BasicValueEnum<'ctx>,
        what: &str,
    ) {
        let store = self.checked(builder.build_store(ptr, value), what);
        self.aligned(Some(store), self.scalar_align(value.get_type()), what);
    }

    /// Copies a `ty` from `src` to `dst`: an `llvm.memcpy` of its size, at
    /// its alignment, so every byte (padding too) arrives.
    fn copy_bytes(
        &self,
        builder: &Builder<'ctx>,
        dst: PointerValue<'ctx>,
        src: PointerValue<'ctx>,
        ty: &LirTy,
    ) {
        let size = self.native.const_int(self.size_align(ty).size, false);
        let align = self.align_of(ty);
        self.checked(builder.build_memcpy(dst, align, src, align, size), "copy");
    }

    /// Zeroes the `ty` at `ptr`: a scalar with one store, an aggregate with
    /// an `llvm.memset` of its size, at its alignment.
    fn zero_bytes(&self, builder: &Builder<'ctx>, ptr: PointerValue<'ctx>, ty: &LirTy) {
        if ty.is_scalar() {
            self.store(builder, ptr, self.lir_type(ty).const_zero(), "zero");
            return;
        }
        let size = self.native.const_int(self.size_align(ty).size, false);
        let zero = self.context.i8_type().const_zero();
        self.checked(
            builder.build_memset(ptr, self.align_of(ty), zero, size),
            "zero",
        );
    }

    /// `base + offset`, keeping `base`'s provenance: a call of the
    /// always-inline `.lilac.offset`, a `getelementptr inbounds i8`
    /// (inkwell's own GEP builders are `unsafe`). The helper is defined the
    /// first time an address needs it, so a module with no field or element
    /// access has none.
    fn offset_ptr(
        &self,
        builder: &Builder<'ctx>,
        base: PointerValue<'ctx>,
        offset: IntValue<'ctx>,
    ) -> PointerValue<'ctx> {
        let helper = self.offset.get().unwrap_or_else(|| {
            let helper = crate::runtime::offset_helper(self);
            self.offset.set(Some(helper));
            helper
        });
        let call = self.checked(
            builder.build_call(helper, &[base.into(), offset.into()], ""),
            "offset",
        );
        match call.try_as_basic_value() {
            ValueKind::Basic(BasicValueEnum::PointerValue(address)) => address,
            _ => bug!(self.at(), "`.lilac.offset` returned no pointer"),
        }
    }

    /// The opaque `ptr` type used for every place under the pointer ABI.
    pub(crate) fn ptr_type(&self) -> PointerType<'ctx> {
        self.context.ptr_type(AddressSpace::default())
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

    /// Defines each global variable: internal, aligned to its type, and
    /// zeroed or holding its initializer ([`Self::global_bytes`]), with a
    /// null in each address slot until [`Self::fill_global_addresses`].
    fn define_globals(&self) {
        for global in &self.lir.globals {
            let initializer = if global.init.is_empty() && global.addresses.is_empty() {
                self.lir_type(&global.ty).const_zero()
            } else {
                self.global_bytes(global, |_| self.ptr_type().const_null())
            };
            let value = self
                .module
                .add_global(initializer.get_type(), None, global.name.as_str());
            value.set_initializer(&initializer);
            value.set_linkage(Linkage::Internal);
            value.set_alignment(self.align_of(&global.ty));
        }
    }

    /// Sets the initializer of each global holding addresses, now that
    /// every function and global they name is declared. The constant has
    /// the type [`Self::define_globals`] gave it: only the pointers change.
    fn fill_global_addresses(&self) {
        for global in &self.lir.globals {
            if global.addresses.is_empty() {
                continue;
            }
            let initializer = self.global_bytes(global, |target| match target {
                AddressTarget::Func(name) => self.function_address(name, global.span),
                AddressTarget::Global(name) => match self.module.get_global(name.as_str()) {
                    Some(value) => value.as_pointer_value(),
                    None => bug!(global.span, "address of undeclared global `!{name}`"),
                },
            });
            let Some(value) = self.module.get_global(global.name.as_str()) else {
                bug!(global.span, "global `!{}` was not defined", global.name);
            };
            value.set_initializer(&initializer);
        }
    }

    /// A global's initial value, its type's size in all: its runs and its
    /// addresses (each pointer from `address`) with the zero gaps between
    /// and after them, as one packed constant (or the run alone when it
    /// covers the object). A gap is a `zeroinitializer`, so a large zero
    /// array costs nothing.
    fn global_bytes(
        &self,
        global: &LirGlobal,
        address: impl Fn(&AddressTarget) -> PointerValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let size = self.size_align(&global.ty).size;
        let pointer = self.lir.target.scalar_layout(Scalar::Pointer).0;
        let runs = global.init.iter().map(|(offset, bytes)| {
            let value: BasicValueEnum<'ctx> = self.context.const_string(bytes, false).into();
            (*offset, bytes.len() as u64, value)
        });
        let addresses = global.addresses.iter().map(|slot| {
            let value: BasicValueEnum<'ctx> = address(&slot.target).into();
            (slot.offset, pointer, value)
        });
        let mut values: Vec<(u64, u64, BasicValueEnum<'ctx>)> = runs.chain(addresses).collect();
        values.sort_by_key(|(offset, _, _)| *offset);
        let mut parts: Vec<BasicValueEnum<'ctx>> = Vec::new();
        let mut at = 0;
        for (offset, len, value) in values {
            if offset > at {
                parts.push(self.bytes_type(offset - at).const_zero());
            }
            parts.push(value);
            at = offset + len;
        }
        if at < size {
            parts.push(self.bytes_type(size - at).const_zero());
        }
        match parts.as_slice() {
            [only] => *only,
            _ => self.context.const_struct(&parts, true).into(),
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
            // The runtime declares the C functions the backend calls itself
            // (`memcmp`); an `extern` of one shares that declaration, so it
            // must agree with it.
            match self.module.get_function(func.name.as_str()) {
                Some(declared) if declared.get_type() == fn_type => {}
                Some(declared) => {
                    return Err(Diagnostic::error(
                        codes::E028,
                        format!(
                            "`extern` function `{}` does not match the C signature `{}`",
                            func.name,
                            declared.get_type().print_to_string()
                        ),
                    )
                    .help("declare it with the C library's own signature"));
                }
                None => {
                    self.module.add_function(func.name.as_str(), fn_type, None);
                }
            }
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
        if let Some(export) = &func.export {
            let fn_type = self.export_fn_type(export);
            self.module
                .add_function(export.symbol.as_str(), fn_type, None);
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

    /// The C signature of an `export`'s entry.
    fn export_fn_type(&self, export: &LirExport) -> FunctionType<'ctx> {
        let c_type = |ty: &LirTy| {
            self.extern_c_type(ty).unwrap_or_else(|| {
                bug!(
                    self.at(),
                    "export `{}` has the non-C type `{ty}`",
                    export.symbol
                )
            })
        };
        let params: Vec<BasicMetadataTypeEnum<'ctx>> =
            export.ins.iter().map(|ty| c_type(ty).into()).collect();
        match export.outs.first() {
            Some(out) => c_type(out).fn_type(&params, false),
            None => self.context.void_type().fn_type(&params, false),
        }
    }

    /// The C entry of an `export`: `symbol` with its C signature stores
    /// each argument in a stack slot, calls the function with the place
    /// ABI (each slot's address, then a slot per out), and returns the out.
    /// A `ref` slot holds the pointer, as the place ABI passes a `ref`.
    fn emit_export(&self, func: &LirFunc, export: &LirExport) {
        let c_type = |ty: &LirTy| {
            self.extern_c_type(ty).unwrap_or_else(|| {
                bug!(
                    self.at(),
                    "export `{}` has the non-C type `{ty}`",
                    export.symbol
                )
            })
        };
        let symbol = export.symbol.as_str();
        let Some(entry) = self.module.get_function(symbol) else {
            bug!(self.at(), "export `{symbol}` was not declared");
        };
        let Some(body) = self.module.get_function(self::symbol(func.name.as_str())) else {
            bug!(self.at(), "export of undeclared `{}`", func.name);
        };
        let builder = self.context.create_builder();
        builder.position_at_end(self.context.append_basic_block(entry, "entry"));
        let mut slots: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for (index, ty) in export.ins.iter().enumerate() {
            let slot = self.checked(builder.build_alloca(c_type(ty), "arg"), "export argument");
            let Some(value) = entry.get_nth_param(u32::try_from(index).unwrap_or(u32::MAX)) else {
                bug!(self.at(), "export `{symbol}` lost a parameter");
            };
            self.store(&builder, slot, value, "export argument store");
            slots.push(slot.into());
        }
        let outs: Vec<_> = export
            .outs
            .iter()
            .map(|ty| {
                (
                    c_type(ty),
                    self.checked(builder.build_alloca(c_type(ty), "out"), "export out"),
                )
            })
            .collect();
        slots.extend(
            outs.iter()
                .map(|(_, slot)| BasicMetadataValueEnum::from(*slot)),
        );
        self.checked(builder.build_call(body, &slots, ""), "export call");
        match outs.first() {
            Some((ty, slot)) => {
                let value = self.load(&builder, *ty, *slot, "export result");
                self.checked(builder.build_return(Some(&value)), "export return");
            }
            None => {
                self.checked(builder.build_return(None), "export return");
            }
        }
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
            bug!(self.at(), "function `{}` was not declared", func.name);
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
            self.at.set(block.term.span());
            let Some(basic_block) = blocks.get(block.label.as_str()).copied() else {
                bug!(self.at(), "missing block `{}`", block.label);
            };
            builder.position_at_end(basic_block);
            for inst in &block.insts {
                self.at.set(inst.span());
                self.lower_inst(inst, &builder, &mut state);
            }
            self.at.set(block.term.span());
            let next = func
                .blocks
                .get(index + 1)
                .and_then(|next| blocks.get(next.label.as_str()).copied());
            self.lower_term(&block.term, &builder, &blocks, &state, next);
        }
        self.at.set(Span::dummy());
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
                self.write(src, self.place_ptr(dst, state), builder, state);
            }
            LirInst::CopyDeref { src, dst, .. } => {
                let from = self.pointer_in(src, builder, state);
                let Some(ty) = state.place_types.get(dst) else {
                    bug!(self.at(), "copy to undeclared place `{dst}`");
                };
                let to = self.place_ptr(dst, state);
                if ty.is_scalar() {
                    let value = self.load(builder, self.lir_type(ty), from, "copy load");
                    self.store(builder, to, value, "copy store");
                } else {
                    self.copy_bytes(builder, to, from, ty);
                }
            }
            LirInst::Const { dst, value, ty, .. } => {
                let value = self.const_value(value, ty, builder);
                state.regs.insert(dst.clone(), value);
            }
            LirInst::Load { src, dst, ty, .. } => {
                // `load %src` always follows the pointer in `src`, even when
                // the pointee is itself a `ref` (an `Optional[ref T]` payload).
                let ptr = self.pointer_in(src, builder, state);
                let value = self.load(builder, self.lir_type(ty), ptr, "load");
                state.regs.insert(dst.clone(), value);
            }
            LirInst::Addr { place, dst, .. } => {
                let ptr = self.place_ptr(place, state);
                state.regs.insert(dst.clone(), ptr.into());
            }
            LirInst::AddrFunc { func, dst, ty, .. } => {
                let value = self.addr_func_value(func, ty, builder);
                state.regs.insert(dst.clone(), value);
            }
            LirInst::AddrGlobal { global, dst, .. } => {
                state.regs.insert(dst.clone(), self.global_addr(global));
            }
            LirInst::Field {
                obj, idx, dst, ty, ..
            } => {
                let field = self.field_ptr(obj, *idx, builder, state);
                let value = self.load(builder, self.lir_type(ty), field, "field load");
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
                self.write(value, field, builder, state);
            }
            LirInst::Store { src, dst, .. } => {
                let reference = self.load_place(dst, builder, state).into_pointer_value();
                self.write(src, reference, builder, state);
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
                self.at(),
                "instruction `{inst:?}` is a compile-time marker and must not reach codegen"
            ),
            LirInst::AtomicLoad { .. }
            | LirInst::AtomicStore { .. }
            | LirInst::AtomicRmw { .. }
            | LirInst::CmpXchg { .. }
            | LirInst::Fence { .. } => self.lower_atomic(inst, builder, state),
            LirInst::Format { .. } => self.lower_format(inst, builder, state),
        }
    }

    /// Defines each template as a private constant in its layout
    /// ([`format_template_layout`]): the hole count, then for each piece a
    /// `#byte_buffer` over its own private bytes (an empty piece too, so
    /// no piece is null) and the address of the formatter of the hole after
    /// it (null after the last piece). Run after every function is
    /// declared, since a template points at its formatters.
    fn define_formats(&self) {
        let target = &self.lir.target;
        let (Some(part), Some(text)) = (
            builtin_layout(&LirTy::format_part(), target),
            builtin_layout(&LirTy::byte_buffer(), target),
        ) else {
            bug!(self.at(), "the template parts have no layout");
        };
        let ptr = self.ptr_type();
        for format in &self.lir.formats {
            let Some(layout) = format_template_layout(format.formats.len(), target) else {
                bug!(
                    format.span,
                    "template `!{}` has no layout",
                    format.name.as_str()
                );
            };
            let (Some(count), Some(parts)) = (layout.fields.first(), layout.fields.get(1)) else {
                bug!(
                    format.span,
                    "template `!{}` has no parts",
                    format.name.as_str()
                );
            };
            let word = |value: usize| -> BasicValueEnum<'ctx> {
                self.native.const_int(value as u64, false).into()
            };
            let mut values = vec![(count.offset, word(format.formats.len()))];
            for (index, piece) in format.pieces.iter().enumerate() {
                let at = parts.offset + index as u64 * part.size;
                let text_at = at + part.fields[0].offset;
                let bytes = self.context.const_string(piece, false);
                let global = self.module.add_global(bytes.get_type(), None, "");
                global.set_linkage(Linkage::Private);
                global.set_constant(true);
                global.set_unnamed_addr(true);
                global.set_initializer(&bytes);
                values.push((text_at + text.fields[0].offset, word(piece.len())));
                values.push((
                    text_at + text.fields[1].offset,
                    global.as_pointer_value().into(),
                ));
                let formatter = match format.formats.get(index) {
                    Some(name) => match self.module.get_function(symbol(name.as_str())) {
                        Some(function) => function.as_global_value().as_pointer_value(),
                        None => bug!(format.span, "formatter `{name}` was not declared"),
                    },
                    None => ptr.const_null(),
                };
                values.push((at + part.fields[1].offset, formatter.into()));
            }
            let init = self.packed_at(&values, layout.size);
            let global = self
                .module
                .add_global(init.get_type(), None, format.name.as_str());
            global.set_linkage(Linkage::Private);
            global.set_constant(true);
            global.set_unnamed_addr(true);
            global.set_initializer(&init);
            global.set_alignment(u32::try_from(layout.align).unwrap_or(1));
        }
    }

    /// A packed constant of `size` bytes holding each scalar `value` at its
    /// offset (in increasing order) and zeros between them.
    fn packed_at(&self, values: &[(u64, BasicValueEnum<'ctx>)], size: u64) -> BasicValueEnum<'ctx> {
        let pointer = self.lir.target.scalar_layout(Scalar::Pointer).0;
        let mut parts: Vec<BasicValueEnum<'ctx>> = Vec::new();
        let mut at = 0;
        for (offset, value) in values {
            if *offset > at {
                parts.push(self.bytes_type(offset - at).const_zero());
            }
            parts.push(*value);
            at = offset
                + match value.get_type() {
                    BasicTypeEnum::IntType(int) => u64::from(int.get_bit_width() / 8),
                    _ => pointer,
                };
        }
        if at < size {
            parts.push(self.bytes_type(size - at).const_zero());
        }
        self.context.const_struct(&parts, true).into()
    }

    /// Lowers `format !t (holes) -> %dst`: the hole addresses go in an
    /// array allocated in the entry block (so a `format` in a loop reuses
    /// one), and `%dst` gets the template's address and the array's (null
    /// when there are no holes).
    fn lower_format(&self, inst: &LirInst, builder: &Builder<'ctx>, state: &FnState<'ctx>) {
        let LirInst::Format {
            template,
            holes,
            dst,
            ..
        } = inst
        else {
            bug!(self.at(), "non-`format` instruction `{inst:?}`");
        };
        let Some(global) = self.module.get_global(template.as_str()) else {
            bug!(
                self.at(),
                "format of undeclared template `!{}`",
                template.as_str()
            );
        };
        let (pointer, align) = self.lir.target.scalar_layout(Scalar::Pointer);
        let array = if holes.is_empty() {
            self.ptr_type().const_null()
        } else {
            let array = self.entry_alloca(builder, self.bytes_type(pointer * holes.len() as u64));
            self.aligned(
                array.as_instruction(),
                u32::try_from(align).unwrap_or(1),
                "hole array",
            );
            for (index, reg) in holes.iter().enumerate() {
                let slot = if index == 0 {
                    array
                } else {
                    let offset = self.native.const_int(pointer * index as u64, false);
                    self.offset_ptr(builder, array, offset)
                };
                self.store(
                    builder,
                    slot,
                    self.reg_ptr(reg, state).into(),
                    "format hole",
                );
            }
            array
        };
        let view = self.place_ptr(dst, state);
        let fields = [global.as_pointer_value(), array];
        for (index, value) in (0u32..).zip(fields) {
            let offset = self.field_offset(&LirTy::format_args(), index);
            let field = if offset == 0 {
                view
            } else {
                self.offset_ptr(builder, view, self.native.const_int(offset, false))
            };
            self.store(builder, field, value.into(), "format field");
        }
    }

    /// An `alloca` of `ty` at the start of the current function's entry
    /// block, so it is made once per call however often its use runs.
    fn entry_alloca(&self, builder: &Builder<'ctx>, ty: BasicTypeEnum<'ctx>) -> PointerValue<'ctx> {
        let Some(entry) = builder
            .get_insert_block()
            .and_then(BasicBlock::get_parent)
            .and_then(FunctionValue::get_first_basic_block)
        else {
            bug!(self.at(), "an `alloca` outside a function");
        };
        let at_entry = self.context.create_builder();
        match entry.get_first_instruction() {
            Some(first) => at_entry.position_before(&first),
            None => at_entry.position_at_end(entry),
        }
        self.checked(at_entry.build_alloca(ty, ""), "entry alloca")
    }

    /// The pointer held in the `ref` place `place` (what `load` and
    /// `copy ref` follow).
    fn pointer_in(
        &self,
        place: &PlaceName,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> PointerValue<'ctx> {
        self.load(
            builder,
            self.ptr_type().into(),
            self.place_ptr(place, state),
            "load ref",
        )
        .into_pointer_value()
    }

    /// The address of global `global` (`addr.global`).
    fn global_addr(&self, global: &Symbol) -> BasicValueEnum<'ctx> {
        let Some(value) = self.module.get_global(global.as_str()) else {
            bug!(self.at(), "address of undeclared global `!{global}`");
        };
        value.as_pointer_value().into()
    }

    /// Writes the operand `src` to the memory at `dst`: an aggregate place
    /// as an `llvm.memcpy` of its bytes, a scalar as an aligned store.
    fn write(
        &self,
        src: &LirOperand,
        dst: PointerValue<'ctx>,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) {
        if let LirOperand::Place(name) = src {
            match state.place_types.get(name) {
                Some(ty) if !ty.is_scalar() => {
                    self.copy_bytes(builder, dst, self.place_ptr(name, state), ty);
                    return;
                }
                Some(_) => {}
                None => bug!(self.at(), "copy of undeclared place `{name}`"),
            }
        }
        let value = self.operand(src, None, builder, state);
        self.store(builder, dst, value, "copy store");
    }

    /// Lowers `zero %place`: an `llvm.memset` of the place's size, at its
    /// alignment.
    fn lower_zero(&self, place: &PlaceName, builder: &Builder<'ctx>, state: &FnState<'ctx>) {
        let ptr = self.place_ptr(place, state);
        let Some(ty) = state.place_types.get(place) else {
            bug!(self.at(), "zero of undeclared place `{place}`");
        };
        let size = self.native.const_int(self.size_align(ty).size, false);
        let zero = self.context.i8_type().const_zero();
        self.checked(
            builder.build_memset(ptr, self.align_of(ty), zero, size),
            "zero place",
        );
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
                let value = self.checked(
                    builder.build_load(self.lir_type(ty), self.reg_ptr(ptr, state), ""),
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
                let store = self.checked(
                    builder.build_store(self.reg_ptr(ptr, state), self.reg_int(value, state)),
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
                let old = self.checked(
                    builder.build_atomicrmw(
                        rmw_bin_op(*op),
                        self.reg_ptr(ptr, state),
                        self.reg_int(value, state),
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
                let pair = self.checked(
                    builder.build_cmpxchg(
                        self.reg_ptr(ptr, state),
                        self.reg_int(expected, state),
                        self.reg_int(desired, state),
                        llvm_ordering(*order),
                        llvm_ordering(order.failure()),
                    ),
                    "cmpxchg",
                );
                let old = self.checked(builder.build_extract_value(pair, 0, ""), "cmpxchg old");
                state.regs.insert(dst.clone(), old);
            }
            LirInst::Fence { order, .. } => {
                self.checked(
                    builder.build_fence(llvm_ordering(*order), false, ""),
                    "fence",
                );
            }
            _ => bug!(self.at(), "non-atomic instruction `{inst:?}`"),
        }
    }

    /// Makes a load or store atomic, aligned to its width.
    fn make_atomic(&self, inst: Option<InstructionValue<'ctx>>, ty: &LirTy, order: AtomicOrdering) {
        let Some(inst) = inst else {
            bug!(self.at(), "atomic access is not an instruction");
        };
        let BasicTypeEnum::IntType(int_type) = self.lir_type(ty) else {
            bug!(self.at(), "atomic access of non-integer `{ty}`");
        };
        let ordered = inst.set_atomic_ordering(llvm_ordering(order));
        let aligned = inst.set_alignment(int_type.get_bit_width() / 8);
        if let Err(err) = ordered.and(aligned) {
            bug!(self.at(), "invalid atomic access: {err}");
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
            ConvertOp::Trunc | ConvertOp::ExtendZero | ConvertOp::ExtendSign => {
                let source = self.reg_int(src, state);
                let BasicTypeEnum::IntType(result_ty) = self.lir_type(ty) else {
                    bug!(self.at(), "`convert` to non-integer type `{ty}`");
                };
                match op {
                    ConvertOp::Trunc => self
                        .checked(builder.build_int_truncate(source, result_ty, ""), "trunc")
                        .into(),
                    ConvertOp::ExtendSign => self
                        .checked(builder.build_int_s_extend(source, result_ty, ""), "sext")
                        .into(),
                    _ => self
                        .checked(builder.build_int_z_extend(source, result_ty, ""), "zext")
                        .into(),
                }
            }
            ConvertOp::Bitcast => {
                let source = self.operand(&LirOperand::Reg(src.to_owned()), None, builder, state);
                match (source, self.lir_type(ty)) {
                    (BasicValueEnum::PointerValue(pointer), BasicTypeEnum::IntType(int)) => self
                        .checked(builder.build_ptr_to_int(pointer, int, ""), "ptrtoint")
                        .into(),
                    (BasicValueEnum::IntValue(int), BasicTypeEnum::PointerType(pointer)) => self
                        .checked(builder.build_int_to_ptr(int, pointer, ""), "inttoptr")
                        .into(),
                    (source, result_ty) => {
                        self.checked(builder.build_bit_cast(source, result_ty, ""), "bitcast")
                    }
                }
            }
        };
        state.regs.insert(dst.to_owned(), value);
    }

    /// Allocates a zeroed place of `ty`, aligned to its layout, and binds it
    /// under `place`.
    fn lower_alloca(
        &self,
        ty: &LirTy,
        place: &PlaceName,
        builder: &Builder<'ctx>,
        state: &mut FnState<'ctx>,
    ) {
        let ptr = self.checked(
            builder.build_alloca(self.lir_type(ty), place.as_str()),
            "alloca",
        );
        self.aligned(ptr.as_instruction(), self.align_of(ty), "alloca");
        self.zero_bytes(builder, ptr, ty);
        state.places.insert(place.to_owned(), ptr);
        state.place_types.insert(place.to_owned(), ty.clone());
    }

    /// The address of function `name`: an `export`'s is its C entry's.
    /// An ICE at `span` when it was not declared.
    fn function_address(&self, name: &Symbol, span: Span) -> PointerValue<'ctx> {
        let target = self
            .exports
            .get(name.as_str())
            .copied()
            .unwrap_or_else(|| symbol(name.as_str()));
        match self.module.get_function(target) {
            Some(function) => function.as_global_value().as_pointer_value(),
            None => bug!(span, "address of unknown function `{name}`"),
        }
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
        let ptr = self.function_address(name, self.at());
        match self.lir_type(ty) {
            BasicTypeEnum::IntType(int_ty) => self
                .checked(builder.build_ptr_to_int(ptr, int_ty, ""), "ptrtoint")
                .into(),
            BasicTypeEnum::PointerType(_) => ptr.into(),
            other => bug!(
                self.at(),
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
            LirCallee::CReg(reg) => self.lower_c_indirect_call(reg, ins, outs, builder, state),
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
            bug!(self.at(), "call to unknown function `{name}`");
        };
        let llvm_name = if callee.is_extern {
            name.as_str()
        } else {
            symbol(name.as_str())
        };
        let Some(function) = self.module.get_function(llvm_name) else {
            bug!(self.at(), "call to unknown function `{name}`");
        };
        if callee.uses_c_abi() {
            let args = self.extern_call_args(callee, ins, builder, state);
            let result = self.checked(builder.build_call(function, &args, ""), "extern call");
            if let Some(out) = outs.first() {
                let value = match result.try_as_basic_value() {
                    ValueKind::Basic(value) => value,
                    ValueKind::Instruction(_) => {
                        bug!(self.at(), "extern `{name}` unexpectedly returned void")
                    }
                };
                self.store(
                    builder,
                    self.place_ptr(out, state),
                    value,
                    "extern result store",
                );
            }
        } else {
            let args = self.direct_call_args(callee, ins, outs, builder, state);
            self.checked(builder.build_call(function, &args, ""), "direct call");
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
                        BasicMetadataValueEnum::from(self.place_ptr(place, state))
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
                let ptr = self.place_ptr(place, state);
                let arg_is_ref = state.place_types.get(place).is_some_and(LirTy::is_ref);
                if param_ty.is_ref() && !arg_is_ref {
                    let slot = self.checked(
                        builder.build_alloca(self.ptr_type(), "ref.arg"),
                        "ref arg alloca",
                    );
                    self.store(builder, slot, ptr.into(), "ref arg store");
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
            bug!(self.at(), "call through undefined register `{reg}`");
        };
        let ptr = match value {
            BasicValueEnum::PointerValue(ptr) => ptr,
            BasicValueEnum::IntValue(int) => self.checked(
                builder.build_int_to_ptr(int, self.ptr_type(), ""),
                "inttoptr",
            ),
            other => bug!(
                self.at(),
                "indirect callee `{reg}` is a `{}`, not a pointer or integer",
                other.get_type().print_to_string()
            ),
        };
        let arity = ins.len() + outs.len();
        let param_types = vec![self.ptr_type().into(); arity];
        let fn_type = self.context.void_type().fn_type(&param_types, false);
        let args = self.place_args(ins, outs, state);
        self.checked(
            builder.build_indirect_call(fn_type, ptr, &args, ""),
            "indirect call",
        );
    }

    /// An indirect C call: each `in` place is loaded and passed by value
    /// at its own (C) type, and the result is stored into the `out` place.
    fn lower_c_indirect_call(
        &self,
        reg: &Reg,
        ins: &[PlaceName],
        outs: &[PlaceName],
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) {
        let Some(value) = state.regs.get(reg).copied() else {
            bug!(self.at(), "call through undefined register `{reg}`");
        };
        let ptr = match value {
            BasicValueEnum::PointerValue(ptr) => ptr,
            BasicValueEnum::IntValue(int) => self.checked(
                builder.build_int_to_ptr(int, self.ptr_type(), ""),
                "inttoptr",
            ),
            other => bug!(
                self.at(),
                "indirect callee `{reg}` is a `{}`, not a pointer or integer",
                other.get_type().print_to_string()
            ),
        };
        let place_type = |place: &PlaceName| {
            let Some(ty) = state.place_types.get(place) else {
                bug!(self.at(), "C call with the undeclared place `{place}`");
            };
            self.lir_type(ty)
        };
        let params: Vec<BasicMetadataTypeEnum<'ctx>> =
            ins.iter().map(|place| place_type(place).into()).collect();
        let fn_type = match outs.first() {
            Some(out) => place_type(out).fn_type(&params, false),
            None => self.context.void_type().fn_type(&params, false),
        };
        let args: Vec<BasicMetadataValueEnum<'ctx>> = ins
            .iter()
            .map(|place| self.load_place(place, builder, state).into())
            .collect();
        let result = self.checked(
            builder.build_indirect_call(fn_type, ptr, &args, ""),
            "C indirect call",
        );
        if let Some(out) = outs.first() {
            let ValueKind::Basic(value) = result.try_as_basic_value() else {
                bug!(self.at(), "C call through `{reg}` returned void");
            };
            self.store(
                builder,
                self.place_ptr(out, state),
                value,
                "C call result store",
            );
        }
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
            bug!(self.at(), "call to unknown builtin `{name}`");
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
                self.at(),
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
                self.at(),
                "builtin `{name}` symbol `{symbol}` was not emitted by the use-scan; expected `{}`",
                fn_type.print_to_string()
            );
        };
        let args: Vec<BasicMetadataValueEnum<'ctx>> = ins
            .iter()
            .map(|place| self.load_place(place, builder, state).into())
            .collect();
        let call = self.checked(builder.build_call(function, &args, ""), "builtin call");
        if let Some(out) = outs.first() {
            let value = match call.try_as_basic_value() {
                ValueKind::Basic(value) => value,
                ValueKind::Instruction(_) => {
                    bug!(self.at(), "builtin `{name}` unexpectedly returned void")
                }
            };
            self.store(
                builder,
                self.place_ptr(out, state),
                value,
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
                None => bug!(self.at(), "use of undefined register `{name}`"),
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
            bug!(self.at(), "load of undeclared place `{name}`");
        };
        self.load_as(name, &ty, builder, state)
    }

    /// Loads place `name` as the given (scalar) LIR type.
    fn load_as(
        &self,
        name: &PlaceName,
        ty: &LirTy,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        // A `ref` place holds a pointer; reading a non-`ref` value out of it
        // follows that pointer to the pointee first.
        let ptr = self.place_ptr(name, state);
        let ptr = match state.place_types.get(name) {
            Some(place_ty) if place_ty.is_ref() && !ty.is_ref() => self
                .load(builder, self.ptr_type().into(), ptr, "load ref")
                .into_pointer_value(),
            _ => ptr,
        };
        self.load(builder, self.lir_type(ty), ptr, "load")
    }

    /// The address of field `idx` of place `obj` (loading the object's
    /// pointer first when the place holds a `ref`): the base plus the
    /// field's offset in the layout.
    fn field_ptr(
        &self,
        obj: &PlaceName,
        idx: u32,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> PointerValue<'ctx> {
        let Some(place_ty) = state.place_types.get(obj) else {
            bug!(self.at(), "field of undeclared place `{obj}`");
        };
        let offset = self.field_offset(place_ty.strip_ref(), idx);
        let base = if place_ty.is_ref() {
            self.load_place(obj, builder, state).into_pointer_value()
        } else {
            self.place_ptr(obj, state)
        };
        if offset == 0 {
            return base;
        }
        self.offset_ptr(builder, base, self.native.const_int(offset, false))
    }

    /// The offset of field `idx` of the aggregate `owner`, from its layout
    /// (a module layout, else a builtin one).
    fn field_offset(&self, owner: &LirTy, idx: u32) -> u64 {
        let field = |layout: &LirLayout| layout.fields.get(idx as usize).map(|field| field.offset);
        let offset = match owner.name().and_then(|name| self.lir.layouts.get(name)) {
            Some(layout) => field(layout),
            None => builtin_layout(owner, &self.lir.target).and_then(|layout| field(&layout)),
        };
        offset.unwrap_or_else(|| bug!(self.at(), "`{owner}` has no field {idx}"))
    }

    /// `elem.addr`: the array's base plus `index` times the element size.
    /// Lowering checked the bounds (or the index is a constant in them), so
    /// the address is in bounds.
    fn lower_elem_addr(&self, inst: &LirInst, builder: &Builder<'ctx>, state: &mut FnState<'ctx>) {
        let LirInst::ElemAddr {
            array,
            index,
            dst,
            ty,
            ..
        } = inst
        else {
            bug!(self.at(), "non-`elem.addr` instruction `{inst:?}`");
        };
        let size = self.size_align(ty.strip_ref()).size;
        let offset = self.checked(
            builder.build_int_mul(
                self.reg_int(index, state),
                self.native.const_int(size, false),
                "",
            ),
            "element offset",
        );
        let elem = self.offset_ptr(builder, self.reg_ptr(array, state), offset);
        state.regs.insert(dst.clone(), elem.into());
    }

    /// Materializes a `const`: a data-symbol address, a float immediate, or
    /// an integer/pointer immediate. The verifier has checked the value
    /// against its type. A float is its exact bit pattern as an integer of
    /// its width, bitcast (the builder folds the cast into a constant), so
    /// no value passes through an `f64`.
    fn const_value(
        &self,
        value: &LirConst,
        ty: &LirTy,
        builder: &Builder<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match (value, self.lir_type(ty)) {
            (LirConst::Data(name), BasicTypeEnum::PointerType(_)) => {
                let Some(global) = self.module.get_global(name.as_str()) else {
                    bug!(self.at(), "const of undeclared data `!{}`", name.as_str());
                };
                global.as_pointer_value().into()
            }
            (LirConst::Float(bits), BasicTypeEnum::FloatType(float_ty)) => {
                let pattern = bits.to_bits();
                let int_ty = match bits.width() {
                    FloatWidth::W16 => self.context.i16_type(),
                    FloatWidth::W32 => self.context.i32_type(),
                    FloatWidth::W64 => self.context.i64_type(),
                    FloatWidth::W128 => self.context.i128_type(),
                };
                let words = [low_word(pattern), low_word(pattern >> 64)];
                let int = int_ty.const_int_arbitrary_precision(&words);
                self.checked(builder.build_bit_cast(int, float_ty, ""), "float constant")
            }
            (LirConst::Int(value), BasicTypeEnum::IntType(int_ty)) => {
                int_ty.const_int(*value, false).into()
            }
            (LirConst::Int(0), BasicTypeEnum::PointerType(ptr_ty)) => ptr_ty.const_null().into(),
            (value, other) => bug!(
                self.at(),
                "`const {value}` cannot be a `{}` value",
                other.print_to_string()
            ),
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
                let left = self.reg_int(lhs, state);
                let right = self.reg_int(rhs, state);
                if !matches!(self.lir_type(ty), BasicTypeEnum::IntType(_)) {
                    bug!(
                        self.at(),
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
                self.checked(value, "integer arith").into()
            }
            ArithOp::And
            | ArithOp::Or
            | ArithOp::Xor
            | ArithOp::Shl
            | ArithOp::Shr
            | ArithOp::ShrU => self.bitwise(op, lhs, rhs, builder, state),
            ArithOp::Eq => {
                // `==` is strict bit-equality (SPEC §8) even for floats, so a
                // float operand is reinterpreted to the same-width integer and
                // compared with `icmp eq`; `fcmp` would compare values.
                let left = self.eq_operand(lhs, builder, state);
                let right = self.eq_operand(rhs, builder, state);
                let cmp = self.checked(
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
                let left = self.reg_int(lhs, state);
                let right = self.reg_int(rhs, state);
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
                let cmp = self.checked(
                    builder.build_int_compare(predicate, left, right, ""),
                    "icmp",
                );
                self.widen_bool(cmp, ty, builder)
            }
            ArithOp::FAdd | ArithOp::FSub | ArithOp::FMul | ArithOp::FDiv | ArithOp::FMod => {
                let left = self.reg_float(lhs, state);
                let right = self.reg_float(rhs, state);
                if !matches!(self.lir_type(ty), BasicTypeEnum::FloatType(_)) {
                    bug!(self.at(), "`{op:?}` needs a float type, found `{ty}`");
                }
                let value = match op {
                    ArithOp::FAdd => builder.build_float_add(left, right, ""),
                    ArithOp::FSub => builder.build_float_sub(left, right, ""),
                    ArithOp::FMul => builder.build_float_mul(left, right, ""),
                    ArithOp::FDiv => builder.build_float_div(left, right, ""),
                    _ => builder.build_float_rem(left, right, ""),
                };
                self.checked(value, "float arith").into()
            }
            ArithOp::FLt | ArithOp::FGt | ArithOp::FLe | ArithOp::FGe => {
                let left = self.reg_float(lhs, state);
                let right = self.reg_float(rhs, state);
                let predicate = match op {
                    ArithOp::FLt => FloatPredicate::OLT,
                    ArithOp::FLe => FloatPredicate::OLE,
                    ArithOp::FGe => FloatPredicate::OGE,
                    _ => FloatPredicate::OGT,
                };
                let cmp = self.checked(
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
                self.at(),
                "comparison result needs a bit-container type, found `{ty}`"
            );
        };
        if result_ty.get_bit_width() == 1 {
            cmp.into()
        } else {
            self.checked(builder.build_int_z_extend(cmp, result_ty, ""), "widen bool")
                .into()
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
                self.checked(builder.build_ptr_to_int(value, self.native, ""), "ptrtoint")
            }
            Some(BasicValueEnum::FloatValue(value)) => {
                let int_ty = self.int_type_of_width(value.get_type().get_bit_width());
                match self.checked(builder.build_bit_cast(value, int_ty, ""), "float bitcast") {
                    BasicValueEnum::IntValue(bits) => bits,
                    other => bug!(
                        self.at(),
                        "float bitcast produced a `{}`",
                        other.get_type().print_to_string()
                    ),
                }
            }
            Some(other) => bug!(
                self.at(),
                "register `{name}` is a `{}`, not comparable",
                other.get_type().print_to_string()
            ),
            None => bug!(self.at(), "use of undefined register `{name}`"),
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
            _ => bug!(self.at(), "no integer type for a {width}-bit float operand"),
        }
    }

    /// Lowers a block terminator. `return` → `ret void`; `branch` → `br`;
    /// `branch.if` → `br i1` (the false edge is the next block in layout order
    /// when `els` is absent); `unreachable` → `unreachable`.
    fn lower_term(
        &self,
        term: &LirTerm,
        builder: &Builder<'ctx>,
        blocks: &HashMap<&str, BasicBlock<'ctx>>,
        state: &FnState<'ctx>,
        next: Option<BasicBlock<'ctx>>,
    ) {
        match term {
            LirTerm::Return { .. } => {
                self.checked(builder.build_return(None), "return");
            }
            LirTerm::Unreachable { .. } => {
                self.checked(builder.build_unreachable(), "unreachable");
            }
            LirTerm::Branch { target, .. } => {
                let block = self.block_at(blocks, target.as_str());
                self.checked(builder.build_unconditional_branch(block), "branch");
            }
            LirTerm::BranchIf {
                cond, then, els, ..
            } => {
                let then_block = self.block_at(blocks, then.as_str());
                let else_block = match els {
                    Some(els) => self.block_at(blocks, els.as_str()),
                    None => match next {
                        Some(next) => next,
                        None => bug!(
                            self.at(),
                            "`branch.if` without `else` has no fall-through block"
                        ),
                    },
                };
                let condition = self.cond_value(cond, builder, state);
                self.checked(
                    builder.build_conditional_branch(condition, then_block, else_block),
                    "branch.if",
                );
            }
        }
    }
}

/// Helpers that read the lowering state, with the span of what is lowered
/// for their ICEs.
impl<'ctx> Lower<'ctx, '_> {
    /// Returns a place's pointer, raising an ICE if it was never declared.
    fn place_ptr(&self, name: &PlaceName, state: &FnState<'ctx>) -> PointerValue<'ctx> {
        match state.places.get(name).copied() {
            Some(ptr) => ptr,
            None => bug!(self.at(), "use of undeclared place `{name}`"),
        }
    }

    /// The address of every listed place, inputs then outputs (pointer ABI).
    fn place_args(
        &self,
        ins: &[PlaceName],
        outs: &[PlaceName],
        state: &FnState<'ctx>,
    ) -> Vec<BasicMetadataValueEnum<'ctx>> {
        ins.iter()
            .chain(outs.iter())
            .map(|place| BasicMetadataValueEnum::from(self.place_ptr(place, state)))
            .collect()
    }

    /// Reads a `branch.if` condition register as an `i1`: an `i1` is used
    /// directly, any wider bit container is tested against zero.
    fn cond_value(
        &self,
        reg: &Reg,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> IntValue<'ctx> {
        let Some(value) = state.regs.get(reg).copied() else {
            bug!(self.at(), "branch on undefined register `{reg}`");
        };
        match value {
            BasicValueEnum::IntValue(int) if int.get_type().get_bit_width() == 1 => int,
            BasicValueEnum::IntValue(int) => {
                let zero = int.get_type().const_zero();
                self.checked(
                    builder.build_int_compare(IntPredicate::NE, int, zero, ""),
                    "branch condition",
                )
            }
            other => bug!(
                self.at(),
                "branch condition `{reg}` is a `{}`, not an integer",
                other.get_type().print_to_string()
            ),
        }
    }

    /// Looks up a branch target, raising an ICE when it is unknown.
    fn block_at(&self, blocks: &HashMap<&str, BasicBlock<'ctx>>, label: &str) -> BasicBlock<'ctx> {
        match blocks.get(label) {
            Some(block) => *block,
            None => bug!(self.at(), "branch to unknown block `${label}`"),
        }
    }

    /// Reads a register as a pointer value.
    fn reg_ptr(&self, name: &Reg, state: &FnState<'ctx>) -> PointerValue<'ctx> {
        match state.regs.get(name).copied() {
            Some(BasicValueEnum::PointerValue(value)) => value,
            Some(other) => bug!(
                self.at(),
                "register `{name}` is a `{}`, not a pointer",
                other.get_type().print_to_string()
            ),
            None => bug!(self.at(), "use of undefined register `{name}`"),
        }
    }

    /// Reads a register as an integer value.
    fn reg_int(&self, name: &Reg, state: &FnState<'ctx>) -> IntValue<'ctx> {
        match state.regs.get(name).copied() {
            Some(BasicValueEnum::IntValue(value)) => value,
            Some(other) => bug!(
                self.at(),
                "register `{name}` is a `{}`, not an integer",
                other.get_type().print_to_string()
            ),
            None => bug!(self.at(), "use of undefined register `{name}`"),
        }
    }

    /// A bitwise op or shift. LIR shifts by the amount modulo the width;
    /// LLVM's shift is poison past it, so the amount is masked first.
    fn bitwise(
        &self,
        op: &ArithOp,
        lhs: &Reg,
        rhs: &Reg,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let left = self.reg_int(lhs, state);
        let right = self.reg_int(rhs, state);
        let value = match op {
            ArithOp::And => builder.build_and(left, right, ""),
            ArithOp::Or => builder.build_or(left, right, ""),
            ArithOp::Xor => builder.build_xor(left, right, ""),
            _ => {
                let width = u64::from(left.get_type().get_bit_width());
                let mask = left.get_type().const_int(width - 1, false);
                let amount = self.checked(builder.build_and(right, mask, ""), "shift mask");
                match op {
                    ArithOp::Shl => builder.build_left_shift(left, amount, ""),
                    ArithOp::Shr => builder.build_right_shift(left, amount, true, ""),
                    _ => builder.build_right_shift(left, amount, false, ""),
                }
            }
        };
        self.checked(value, "bitwise").into()
    }

    /// Reads a register as a float value.
    fn reg_float(&self, name: &Reg, state: &FnState<'ctx>) -> FloatValue<'ctx> {
        match state.regs.get(name).copied() {
            Some(BasicValueEnum::FloatValue(value)) => value,
            Some(other) => bug!(
                self.at(),
                "register `{name}` is a `{}`, not a float",
                other.get_type().print_to_string()
            ),
            None => bug!(self.at(), "use of undefined register `{name}`"),
        }
    }

    /// Sets a load's or store's alignment (`align` bytes).
    fn aligned(&self, inst: Option<InstructionValue<'_>>, align: u32, what: &str) {
        let Some(inst) = inst else {
            bug!(self.at(), "{what} is not an instruction");
        };
        if let Err(err) = inst.set_alignment(align) {
            bug!(self.at(), "cannot align {what} to {align}: {err}");
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
/// host C library's `memcpy`; `@ptr_add`/`@lilac_abort`/`@linux_syscall` map to
/// their `.lilac.` leaves (`syscall` is also a C library function). `@memcmp`
/// is C's own `memcmp`, which the optimizer expands inline for a small
/// constant size and the C library (or, natively, [`crate::native`])
/// defines.
fn builtin_symbol(name: &str) -> &str {
    match name {
        "@memcpy" => ".lilac.memcpy",
        "@memmove" => ".lilac.memmove",
        "@memset" => ".lilac.memset",
        "@memcmp" => "memcmp",
        "@ptr_add" => ".lilac.ptr_add",
        "@lilac_abort" => ".lilac.abort",
        "@linux_syscall" => ".lilac.linux_syscall",
        "@trace_head" => ".lilac.trace_head",
        "@panic_depth" => ".lilac.panic_depth",
        "@linux_thread_clone" => ".lilac.linux_thread_clone",
        other => other.strip_prefix('@').unwrap_or(other),
    }
}

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
