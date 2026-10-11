//! LLVM backend behind [`Backend`].
//!
//! Type/layout/data/place lowering, the value, memory, conversion, constant,
//! and arithmetic instruction bodies, calls (direct/indirect/builtin), and
//! control flow and terminators live in the `lower` module. The fixed
//! in-module builtin runtime is emitted by the `runtime` module. This crate is
//! the ONLY one allowed to link LLVM (enforced by CI grep gate).

use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use inkwell::OptimizationLevel;

use lilac_codegen::{Backend, CodegenStep, Entry, OptLevel};
use lilac_error::Emitter;
use lilac_ir::LirModule;

mod lower;
mod native;
mod runtime;
mod threads;

/// LLVM backend for a fixed target triple and optimization level.
#[derive(Debug)]
pub struct LlvmBackend {
    triple: String,
    optimize: OptLevel,
}

impl LlvmBackend {
    /// Creates the backend for `triple` at `optimize`.
    #[must_use]
    pub fn new(triple: &str, optimize: OptLevel) -> Self {
        Self {
            triple: triple.to_owned(),
            optimize,
        }
    }

    /// The codegen [`OptLevel`]'s optimization level and new-pass-manager
    /// pipeline, together. `O0` runs no passes; every other level builds LLVM's
    /// `default<O*>` pipeline.
    fn optimize_settings(&self) -> (OptimizationLevel, Option<&'static str>) {
        match self.optimize {
            OptLevel::O0 => (OptimizationLevel::None, None),
            OptLevel::O1 => (OptimizationLevel::Less, Some("default<O1>")),
            OptLevel::O2 => (OptimizationLevel::Default, Some("default<O2>")),
            OptLevel::O3 => (OptimizationLevel::Aggressive, Some("default<O3>")),
            OptLevel::Os => (OptimizationLevel::Default, Some("default<Os>")),
        }
    }

    /// Builds an empty module carrying the target triple and data layout, and
    /// returns it with the target machine the pass pipeline needs. Lowering
    /// populates the module; [`Self::build_module`] verifies and optimizes it.
    fn prepare_module<'ctx>(
        &self,
        context: &'ctx Context,
        reloc: RelocMode,
    ) -> Result<(Module<'ctx>, TargetMachine), String> {
        let module = context.create_module("lilac");
        let triple = TargetTriple::create(&self.triple);
        module.set_triple(&triple);

        Target::initialize_all(&InitializationConfig::default());
        let target = Target::from_triple(&triple)
            .map_err(|e| format!("unknown target triple `{}`: {e}", self.triple))?;
        let (optimize, _) = self.optimize_settings();
        let machine: TargetMachine = target
            .create_target_machine(&triple, "generic", "", optimize, reloc, CodeModel::Default)
            .ok_or_else(|| format!("failed to create target machine for `{}`", self.triple))?;
        module.set_data_layout(&machine.get_target_data().get_data_layout());

        Ok((module, machine))
    }

    /// Lowers, verifies and optimizes `lir` into a fresh LLVM module, calling
    /// `on_step` before each step. Shared by [`Backend::emit_llvm`] and
    /// [`Backend::emit_object`].
    fn build_module<'ctx>(
        &self,
        context: &'ctx Context,
        lir: &LirModule,
        reloc: RelocMode,
        entry: Entry,
        on_step: &mut dyn FnMut(CodegenStep),
    ) -> Result<(Module<'ctx>, TargetMachine), String> {
        let (module, machine) = self.prepare_module(context, reloc)?;
        on_step(CodegenStep::Lower);
        let mut lower = lower::Lower::new(context, module, lir, &self.triple);
        if let Err(diagnostic) = lower.run(entry) {
            return Err(lilac_error::EmitterHuman::new()
                .render(&diagnostic, &lilac_span::SourceMap::default()));
        }
        let native = lower.native_int();
        let module = lower.into_module();
        if entry == Entry::Native {
            native::define(context, &module, &self.triple, native);
        }
        on_step(CodegenStep::Verify);
        module
            .verify()
            .map_err(|e| format!("module verification failed: {e}"))?;
        if let Some(pipeline) = self.optimize_settings().1 {
            on_step(CodegenStep::Optimize);
            module
                .run_passes(pipeline, &machine, PassBuilderOptions::create())
                .map_err(|e| format!("optimization pipeline `{pipeline}` failed: {e}"))?;
        }
        Ok((module, machine))
    }

    /// The relocation model for object files: position-independent on ELF
    /// targets (PIE is the default executable kind there), else the default.
    /// Textual IR does not record it.
    fn object_reloc(&self) -> RelocMode {
        if self.triple.contains("linux") {
            RelocMode::PIC
        } else {
            RelocMode::Default
        }
    }
}

impl Backend for LlvmBackend {
    fn name(&self) -> &'static str {
        "llvm"
    }

    fn emit_object(
        &self,
        lir: &LirModule,
        entry: Entry,
        on_step: &mut dyn FnMut(CodegenStep),
    ) -> Result<Vec<u8>, String> {
        let context = Context::create();
        let (module, machine) =
            self.build_module(&context, lir, self.object_reloc(), entry, on_step)?;
        on_step(CodegenStep::Emit);
        let buffer = machine
            .write_to_memory_buffer(&module, FileType::Object)
            .map_err(|e| format!("object emission failed: {e}"))?;
        Ok(buffer.as_slice().to_vec())
    }

    fn emit_llvm(&self, lir: &LirModule) -> Result<String, String> {
        let context = Context::create();
        let (module, _) =
            self.build_module(&context, lir, RelocMode::Default, Entry::CMain, &mut |_| {})?;
        Ok(module.print_to_string().to_string())
    }
}

#[cfg(test)]
mod tests {
    fn t(text: &str) -> lilac_ir::LirTy {
        lilac_ir::LirTy::parse(text).expect("type spelling")
    }

    use crate as lilac_codegen_llvm;
    use lilac_codegen::{Backend, CodegenStep, Entry, OptLevel};
    use lilac_ir::{
        ArithOp, ConvertOp, LirBlock, LirCallee, LirConst, LirData, LirFunc, LirInst, LirModule,
        LirOperand, LirTerm,
    };
    use lilac_span::Span;

    fn span() -> Span {
        Span::dummy()
    }

    /// A C struct of `(name, type)` fields on the tests' x86-64 targets.
    fn struct_layout(fields: &[(&str, &str)]) -> lilac_ir::LirLayout {
        lilac_ir::LirLayout::of_struct(
            fields
                .iter()
                .map(|(name, ty)| ((*name).to_owned(), t(ty)))
                .collect(),
            &std::collections::HashMap::<String, lilac_ir::LirLayout>::new(),
            &lilac_target::Target::default(),
        )
        .expect("a layout")
    }

    fn backend() -> lilac_codegen_llvm::LlvmBackend {
        lilac_codegen_llvm::LlvmBackend::new("x86_64-pc-windows-msvc", OptLevel::O0)
    }

    fn emit(module: &LirModule) -> String {
        backend().emit_llvm(module).expect("emits")
    }

    /// One function whose signature covers every scalar type plus the two
    /// builtin aggregates, and whose body allocas every type (including the
    /// `#Pair` layout) so the stores are observable.
    fn type_module() -> LirModule {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.layouts.insert(
            "Pair".to_owned(),
            struct_layout(&[("a", "#bit32"), ("b", "#bit8")]),
        );
        let allocas = [
            ("p8", "#bit8"),
            ("p16", "#bit16"),
            ("p32", "#bit32"),
            ("p64", "#bit64"),
            ("pn", "#bit_native"),
            ("pr", "ref #bit32"),
            ("pbb", "#byte_buffer"),
            ("pfa", "#format_args"),
            ("ppair", "#Pair"),
        ];
        let insts = allocas
            .iter()
            .map(|(place, ty)| LirInst::Alloca {
                ty: t(ty),
                place: (*place).into(),
                span: span(),
            })
            .collect();
        module.funcs.push(LirFunc {
            name: "f".into(),
            ins: vec![
                ("a".into(), t("#bit8")),
                ("b".into(), t("#bit16")),
                ("c".into(), t("#bit32")),
                ("d".into(), t("#bit64")),
                ("e".into(), t("#bit_native")),
                ("r".into(), t("ref #bit32")),
                ("bb".into(), t("#byte_buffer")),
                ("fa".into(), t("#format_args")),
            ],
            outs: vec![("out".into(), t("#bit32"))],
            is_extern: false,
            export: None,
            variadic: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".into(),
                insts,
                term: LirTerm::Return { span: span() },
                span: span(),
            }],
        });
        module
    }

    /// Aggregates are their bytes to LLVM, `[size x i8]` at the layout's
    /// alignment, and are zeroed with `llvm.memset`; scalars keep their LLVM
    /// types and are zeroed with an aligned store.
    #[test]
    fn lowers_types_layouts_and_zero_init() {
        let text = emit(&type_module());
        assert!(
            !text.contains("type {"),
            "no struct type is defined:\n{text}"
        );
        for needle in [
            "%ppair = alloca [8 x i8], align 4",
            "%pbb = alloca [16 x i8], align 8",
            "%pfa = alloca [264 x i8], align 8",
            "store i8 0, ptr %p8, align 1",
            "store i16 0, ptr %p16, align 2",
            "store i32 0, ptr %p32, align 4",
            "store i64 0, ptr %p64, align 8",
            "store ptr null, ptr %pr, align 8",
            "call void @llvm.memset.p0.i64(ptr align 4 %ppair, i8 0, i64 8, i1 false)",
            "call void @llvm.memset.p0.i64(ptr align 8 %pbb, i8 0, i64 16, i1 false)",
            "call void @llvm.memset.p0.i64(ptr align 8 %pfa, i8 0, i64 264, i1 false)",
        ] {
            assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
        }
    }

    #[test]
    fn lowers_pointer_calling_convention() {
        let text = emit(&type_module());
        assert!(
            text.contains(
                "define void @f(ptr %in0, ptr %in1, ptr %in2, ptr %in3, ptr %in4, \
                 ptr %in5, ptr %in6, ptr %in7, ptr %out0)"
            ),
            "{text}"
        );
    }

    /// An `extern` function is declared with the platform C ABI — scalars by
    /// value, `ref T` as `ptr`, the single out as the return type — instead of
    /// the pointer-place ABI internal functions use, and emits no body.
    #[test]
    fn extern_decl_uses_c_signature() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.funcs.push(LirFunc {
            name: "write".into(),
            ins: vec![
                ("fd".into(), t("#bit32")),
                ("buf".into(), t("ref #bit8")),
                ("n".into(), t("#bit_native")),
            ],
            outs: vec![("written".into(), t("#bit_native"))],
            is_extern: true,
            export: None,
            variadic: false,
            inline: false,
            blocks: Vec::new(),
        });
        let text = emit(&module);
        assert!(text.contains("declare i64 @write(i32, ptr, i64)"), "{text}");
        assert!(!text.contains("define i64 @write"), "{text}");
    }

    /// An `extern` with float parameter/return types uses the platform C ABI
    /// float types (`double`/`float`); the float types are in the accept-set.
    /// An aggregate parameter is still `E028`.
    #[test]
    fn extern_float_types_use_c_signature() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.funcs.push(LirFunc {
            name: "scale".into(),
            ins: vec![("x".into(), t("#fbit64"))],
            outs: vec![("out".into(), t("#fbit32"))],
            is_extern: true,
            export: None,
            variadic: false,
            inline: false,
            blocks: Vec::new(),
        });
        let text = emit(&module);
        assert!(text.contains("declare float @scale(double)"), "{text}");

        let mut bad =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        bad.funcs.push(LirFunc {
            name: "puts".into(),
            ins: vec![("s".into(), t("#byte_buffer"))],
            outs: Vec::new(),
            is_extern: true,
            export: None,
            variadic: false,
            inline: false,
            blocks: Vec::new(),
        });
        let err = backend()
            .emit_llvm(&bad)
            .expect_err("aggregate parameter must stay E028");
        assert!(err.contains("error[E028]"), "{err}");
    }

    /// A call to an `extern` uses the platform C ABI: scalar `in` places are
    /// loaded and passed by value, `ref T` places pass their pointer, and the
    /// single return is stored into the out place — not the internal
    /// pointer-place ABI.
    #[test]
    fn extern_call_uses_c_abi() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.data.push(LirData {
            name: "data".into(),
            bytes: "hi".as_bytes().to_vec(),
            span: span(),
        });
        module.funcs.push(LirFunc {
            name: "write".into(),
            ins: vec![
                ("fd".into(), t("#bit32")),
                ("buf".into(), t("ref #bit8")),
                ("n".into(), t("#bit_native")),
            ],
            outs: vec![("written".into(), t("#bit_native"))],
            is_extern: true,
            export: None,
            variadic: false,
            inline: false,
            blocks: Vec::new(),
        });
        let mut insts = Vec::new();
        for (place, ty) in [
            ("fd", "#bit32"),
            ("buf", "ref #bit8"),
            ("n", "#bit_native"),
            ("written", "#bit_native"),
        ] {
            alloca(&mut insts, place, ty);
        }
        insts.push(LirInst::Const {
            dst: "1".into(),
            value: LirConst::Int(1),
            ty: t("#bit32"),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("1".into()), "fd");
        insts.push(LirInst::Const {
            dst: "2".into(),
            value: LirConst::Data("data".into()),
            ty: t("ref #bit8"),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("2".into()), "buf");
        insts.push(LirInst::Const {
            dst: "3".into(),
            value: LirConst::Int(2),
            ty: t("#bit_native"),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("3".into()), "n");
        call(
            &mut insts,
            LirCallee::Func("write".into()),
            &["fd", "buf", "n"],
            &["written"],
        );
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        // The values `(fd=1, buf=@data, n=2)` are materialized into their
        // places; at O0 their loads pass as SSA values, not folded literals.
        assert!(text.contains("store i32 1"), "{text}");
        assert!(text.contains("store ptr @data"), "{text}");
        assert!(text.contains("store i64 2"), "{text}");
        // The C ABI call, exactly: the scalar/pointer places are loaded and
        // passed by value (`i32 %0`, `ptr %1`, `i64 %2`), the single i64 return
        // is `%3`, and it is stored into the `written` out place — never the
        // internal all-`ptr` place ABI.
        assert_eq!(
            text.matches("call i64 @write").count(),
            1,
            "expected exactly one extern call:\n{text}"
        );
        let call = text
            .lines()
            .find(|line| line.contains("call i64 @write"))
            .expect("extern write call");
        assert_eq!(
            call.trim(),
            "%3 = call i64 @write(i32 %0, ptr %1, i64 %2)",
            "{text}"
        );
        assert!(text.contains("store i64 %3, ptr %written"), "{text}");
    }

    /// A variadic `extern` gets a `...` prototype, and a call passes its extra
    /// places by value after the fixed ones.
    #[test]
    fn variadic_extern_call_passes_extras() {
        let mut module = LirModule::new(
            lilac_target::Target::parse("x86_64-unknown-linux-gnu").expect("triple"),
        );
        module.funcs.push(LirFunc {
            name: "open".into(),
            ins: vec![
                ("path".into(), t("ref #bit8")),
                ("flags".into(), t("#bit32")),
            ],
            outs: vec![("fd".into(), t("#bit32"))],
            is_extern: true,
            export: None,
            variadic: true,
            inline: false,
            blocks: Vec::new(),
        });
        let mut insts = Vec::new();
        for (place, ty) in [
            ("path", "ref #bit8"),
            ("flags", "#bit32"),
            ("mode", "#bit32"),
            ("fd", "#bit32"),
        ] {
            alloca(&mut insts, place, ty);
        }
        call(
            &mut insts,
            LirCallee::Func("open".into()),
            &["path", "flags", "mode"],
            &["fd"],
        );
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(text.contains("declare i32 @open(ptr, i32, ...)"), "{text}");
        let call = text
            .lines()
            .find(|line| line.contains("call") && line.contains("@open("))
            .expect("variadic call");
        assert_eq!(
            call.trim(),
            "%3 = call i32 (ptr, i32, ...) @open(ptr %0, i32 %1, i32 %2)",
            "{text}"
        );
    }

    /// An `extern` parameter outside the scalar/pointer set is `E028`, not a
    /// mis-compiled struct pointer.
    #[test]
    fn extern_aggregate_param_is_e028() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.funcs.push(LirFunc {
            name: "puts".into(),
            ins: vec![("s".into(), t("#byte_buffer"))],
            outs: Vec::new(),
            is_extern: true,
            export: None,
            variadic: false,
            inline: false,
            blocks: Vec::new(),
        });
        let err = backend()
            .emit_llvm(&module)
            .expect_err("aggregate parameter must be E028");
        assert!(err.contains("error[E028]"), "{err}");
    }

    /// An `extern` declaring more than one out place is `E028`.
    #[test]
    fn extern_multi_out_is_e028() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.funcs.push(LirFunc {
            name: "pair".into(),
            ins: Vec::new(),
            outs: vec![("a".into(), t("#bit32")), ("b".into(), t("#bit32"))],
            is_extern: true,
            export: None,
            variadic: false,
            inline: false,
            blocks: Vec::new(),
        });
        let err = backend()
            .emit_llvm(&module)
            .expect_err("multiple outs must be E028");
        assert!(err.contains("error[E028]"), "{err}");
    }

    /// An `extern` named `main` keeps the unmangled C symbol `main`; the entry
    /// wrapper must not mistake it for the Lilac entry and look up the absent
    /// `.lilac.main`, which used to ICE on user input. The frontend rejects it
    /// as `E028`; the backend must still emit (declaring `@main` and no wrapper)
    /// rather than panic when driven directly.
    #[test]
    fn extern_main_does_not_ice() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.funcs.push(LirFunc {
            name: "main".into(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: true,
            export: None,
            variadic: false,
            inline: false,
            blocks: Vec::new(),
        });
        let text = emit(&module);
        assert!(text.contains("declare void @main()"), "{text}");
        assert!(!text.contains("define i32 @main()"), "{text}");
    }

    #[test]
    fn lowers_data_globals_in_order() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.data.push(LirData {
            name: "c0".into(),
            bytes: "hi".as_bytes().to_vec(),
            span: span(),
        });
        module.data.push(LirData {
            name: "c1".into(),
            bytes: "10.5".as_bytes().to_vec(),
            span: span(),
        });
        let text = emit(&module);
        let first = text
            .find("@c0 = private unnamed_addr constant [2 x i8]")
            .expect("c0 global");
        let second = text
            .find("@c1 = private unnamed_addr constant [4 x i8]")
            .expect("c1 global");
        assert!(first < second, "{text}");
    }

    fn alloca(insts: &mut Vec<LirInst>, place: &str, ty: &str) {
        insts.push(LirInst::Alloca {
            ty: t(ty),
            place: place.into(),
            span: span(),
        });
    }

    fn copy_place(insts: &mut Vec<LirInst>, src: LirOperand, dst: &str) {
        insts.push(LirInst::CopyPlace {
            src,
            dst: dst.into(),
            span: span(),
        });
    }

    fn arith(insts: &mut Vec<LirInst>, op: ArithOp, lhs: &str, rhs: &str, dst: &str, ty: &str) {
        insts.push(LirInst::Arith {
            op,
            lhs: lhs.into(),
            rhs: rhs.into(),
            dst: dst.into(),
            ty: t(ty),
            span: span(),
        });
    }

    fn func(
        name: &str,
        ins: &[(&str, &str)],
        outs: &[(&str, &str)],
        blocks: Vec<LirBlock>,
    ) -> LirFunc {
        LirFunc {
            name: name.into(),
            ins: ins
                .iter()
                .map(|(place, ty)| ((*place).into(), t(ty)))
                .collect(),
            outs: outs
                .iter()
                .map(|(place, ty)| ((*place).into(), t(ty)))
                .collect(),
            is_extern: false,
            export: None,
            variadic: false,
            inline: false,
            blocks,
        }
    }

    fn ret_block(label: &str, insts: Vec<LirInst>) -> LirBlock {
        LirBlock {
            label: label.into(),
            insts,
            term: LirTerm::Return { span: span() },
            span: span(),
        }
    }

    fn call(insts: &mut Vec<LirInst>, func: LirCallee, ins: &[&str], outs: &[&str]) {
        insts.push(LirInst::Call {
            func,
            ins: ins.iter().map(|place| (*place).into()).collect(),
            outs: outs.iter().map(|place| (*place).into()).collect(),
            span: span(),
        });
    }

    /// A `main` calling all three leaf builtins, so every leaf must be emitted.
    fn all_leaves_module() -> LirModule {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut insts = Vec::new();
        for (place, ty) in [
            ("dest", "ref #bit8"),
            ("src", "ref #bit8"),
            ("len", "#bit_native"),
            ("base", "ref #bit8"),
            ("off", "#bit_native"),
            ("out", "ref #bit8"),
        ] {
            alloca(&mut insts, place, ty);
        }
        call(
            &mut insts,
            LirCallee::Builtin("@memcpy".to_owned()),
            &["dest", "src", "len"],
            &[],
        );
        call(
            &mut insts,
            LirCallee::Builtin("@ptr_add".to_owned()),
            &["base", "off"],
            &["out"],
        );
        call(
            &mut insts,
            LirCallee::Builtin("@lilac_abort".to_owned()),
            &[],
            &[],
        );
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        module
    }

    /// One function exercising every Task-3 instruction family so the printed
    /// IR shows each opcode. Arithmetic operands are loaded (not constants) so
    /// LLVM's builder cannot fold the opcodes away.
    #[allow(clippy::too_many_lines)]
    fn inst_module() -> LirModule {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.layouts.insert(
            "Pair".to_owned(),
            struct_layout(&[("a", "#bit32"), ("b", "#bit8")]),
        );
        module.data.push(LirData {
            name: "c0".into(),
            bytes: "hi".as_bytes().to_vec(),
            span: span(),
        });

        let mut insts = Vec::new();
        for (place, ty) in [
            ("pair", "#Pair"),
            ("pair2", "#Pair"),
            ("rpair", "ref #Pair"),
            ("x", "#bit32"),
            ("x2", "#bit32"),
            ("b", "#bit8"),
            ("r", "ref #bit32"),
            ("bpslot", "ref #bit8"),
            ("adslot", "ref #bit32"),
            ("afslot", "#bit64"),
            ("fslot", "#fbit32"),
        ] {
            alloca(&mut insts, place, ty);
        }

        insts.push(LirInst::Const {
            dst: "c".into(),
            value: LirConst::Int(42),
            ty: t("#bit32"),
            span: span(),
        });
        insts.push(LirInst::Const {
            dst: "dec".into(),
            value: LirConst::Int(7),
            ty: t("#bit8"),
            span: span(),
        });

        insts.push(LirInst::Const {
            dst: "bp".into(),
            value: LirConst::Data("c0".into()),
            ty: t("ref #bit8"),
            span: span(),
        });
        insts.push(LirInst::Addr {
            place: "x".into(),
            dst: "ad".into(),
            ty: t("ref #bit32"),
            span: span(),
        });
        insts.push(LirInst::AddrFunc {
            func: "f".into(),
            dst: "af".into(),
            ty: t("#bit64"),
            span: span(),
        });

        copy_place(&mut insts, LirOperand::Reg("c".into()), "x");
        copy_place(&mut insts, LirOperand::Place("x".into()), "x2");
        copy_place(&mut insts, LirOperand::Reg("dec".into()), "b");
        copy_place(&mut insts, LirOperand::Reg("bp".into()), "bpslot");
        copy_place(&mut insts, LirOperand::Place("pair".into()), "pair2");
        insts.push(LirInst::CopyDeref {
            src: "rpair".into(),
            dst: "pair2".into(),
            span: span(),
        });
        insts.push(LirInst::Field {
            obj: "pair".into(),
            name: "b".to_owned(),
            idx: 1,
            dst: "fb".into(),
            ty: t("#bit8"),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("ad".into()), "adslot");
        copy_place(&mut insts, LirOperand::Reg("af".into()), "afslot");
        insts.push(LirInst::CopyReg {
            src: LirOperand::Place("x".into()),
            dst: "cr".into(),
            ty: t("#bit32"),
            span: span(),
        });
        insts.push(LirInst::Load {
            src: "r".into(),
            dst: "ld".into(),
            ty: t("ref #bit32"),
            span: span(),
        });
        insts.push(LirInst::Load {
            src: "fslot".into(),
            dst: "fcr".into(),
            ty: t("#fbit32"),
            span: span(),
        });
        insts.push(LirInst::Field {
            obj: "pair".into(),
            name: "a".to_owned(),
            idx: 0,
            dst: "fa".into(),
            ty: t("#bit32"),
            span: span(),
        });
        insts.push(LirInst::FieldAddr {
            obj: "pair".into(),
            name: "a".to_owned(),
            idx: 0,
            dst: "fad".into(),
            ty: t("ref #bit32"),
            span: span(),
        });
        insts.push(LirInst::StoreField {
            value: LirOperand::Reg("c".into()),
            obj: "pair".into(),
            name: "a".to_owned(),
            idx: 0,
            span: span(),
        });
        insts.push(LirInst::Store {
            src: LirOperand::Reg("c".into()),
            dst: "r".into(),
            span: span(),
        });
        insts.push(LirInst::Convert {
            op: ConvertOp::Trunc,
            src: "cr".into(),
            dst: "t".into(),
            ty: t("#bit8"),
            span: span(),
        });
        insts.push(LirInst::Convert {
            op: ConvertOp::ExtendZero,
            src: "t".into(),
            dst: "z".into(),
            ty: t("#bit64"),
            span: span(),
        });

        for (op, dst) in [
            (ArithOp::IAdd, "ia"),
            (ArithOp::ISub, "is"),
            (ArithOp::IMul, "im"),
            (ArithOp::IDiv, "id"),
            (ArithOp::IDivU, "idu"),
            (ArithOp::IMod, "ir"),
            (ArithOp::IModU, "iru"),
            (ArithOp::And, "band"),
            (ArithOp::Or, "bor"),
            (ArithOp::Xor, "bxor"),
            (ArithOp::Shl, "shl"),
            (ArithOp::Shr, "shr"),
            (ArithOp::ShrU, "shru"),
        ] {
            arith(&mut insts, op, "cr", "cr", dst, "#bit32");
        }
        for (op, dst) in [
            (ArithOp::Eq, "eq"),
            (ArithOp::ILt, "lt"),
            (ArithOp::ILtU, "ltu"),
            (ArithOp::IGt, "gt"),
            (ArithOp::IGtU, "gtu"),
        ] {
            arith(&mut insts, op, "cr", "cr", dst, "#bit8");
        }
        for (op, dst) in [
            (ArithOp::FAdd, "fa1"),
            (ArithOp::FSub, "fs"),
            (ArithOp::FMul, "fm"),
            (ArithOp::FDiv, "fd"),
            (ArithOp::FMod, "fr"),
        ] {
            arith(&mut insts, op, "fcr", "fcr", dst, "#fbit32");
        }
        for (op, dst) in [(ArithOp::FLt, "fl"), (ArithOp::FGt, "fg")] {
            arith(&mut insts, op, "fcr", "fcr", dst, "#bit8");
        }

        module.funcs.push(LirFunc {
            name: "f".into(),
            ins: vec![("p".into(), t("ref #bit32"))],
            outs: vec![("o".into(), t("#bit32"))],
            is_extern: false,
            export: None,
            variadic: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".into(),
                insts,
                term: LirTerm::Return { span: span() },
                span: span(),
            }],
        });
        module
    }

    #[test]
    fn lowers_memory_arith_convert_and_const() {
        let text = emit(&inst_module());
        for needle in [
            "store i32 42",
            "i8 7",
            "load i32, ptr %x",
            "load ptr, ptr %r",
            "ptr @f",
            "@c0",
            "store ptr @c0, ptr %bpslot, align 8",
            "store ptr %x, ptr %adslot, align 8",
            "%pair = alloca [8 x i8], align 4",
            "call void @llvm.memset.p0.i64(ptr align 4 %pair, i8 0, i64 8, i1 false)",
            "call void @llvm.memcpy.p0.p0.i64(ptr align 4 %pair2, ptr align 4 %pair, i64 8, i1 false)",
            "call ptr @.lilac.offset(ptr %pair, i64 4)",
            "define internal ptr @.lilac.offset(ptr %base, i64 %offset)",
            "getelementptr inbounds i8, ptr %base, i64 %offset",
            "trunc i32",
            "zext i8",
            "add i32",
            "sub i32",
            "mul i32",
            "sdiv i32",
            "udiv i32",
            "srem i32",
            "urem i32",
            "icmp eq i32",
            "icmp slt i32",
            "icmp ult i32",
            "icmp sgt i32",
            "icmp ugt i32",
            "fadd float",
            "fsub float",
            "fmul float",
            "fdiv float",
            "frem float",
            "fcmp olt float",
            "fcmp ogt float",
        ] {
            assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
        }
    }

    /// `field %instance` on an `in ref #Pair` place must load the reference
    /// out of the place before adding the field's offset (the pointer ABI
    /// passes the address of the caller's reference slot).
    #[test]
    fn field_through_ref_loads_reference_first() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.layouts.insert(
            "Pair".to_owned(),
            struct_layout(&[("a", "#bit32"), ("b", "#bit8")]),
        );
        let insts = vec![
            LirInst::Field {
                obj: "rp".into(),
                name: "b".to_owned(),
                idx: 1,
                dst: "v".into(),
                ty: t("#bit8"),
                span: span(),
            },
            LirInst::CopyPlace {
                src: LirOperand::Reg("v".into()),
                dst: "o".into(),
                span: span(),
            },
        ];
        module.funcs.push(LirFunc {
            name: "g".into(),
            ins: vec![("rp".into(), t("ref #Pair"))],
            outs: vec![("o".into(), t("#bit8"))],
            is_extern: false,
            export: None,
            variadic: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".into(),
                insts,
                term: LirTerm::Return { span: span() },
                span: span(),
            }],
        });
        let text = emit(&module);
        assert!(text.contains("load ptr, ptr %in0"), "{text}");
        assert!(
            text.contains("call ptr @.lilac.offset(ptr %0, i64 4)"),
            "{text}"
        );
    }

    /// A direct user call passes the address of every listed place (ins then
    /// outs); the place ABI means the callee signature is all `ptr`.
    #[test]
    fn lowers_direct_call_with_place_pointers() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.funcs.push(func(
            "g",
            &[("a", "#bit32")],
            &[("o", "#bit32")],
            vec![ret_block("entry", Vec::new())],
        ));
        let mut insts = Vec::new();
        alloca(&mut insts, "a", "#bit32");
        alloca(&mut insts, "o", "#bit32");
        call(&mut insts, LirCallee::Func("g".into()), &["a"], &["o"]);
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(text.contains("call void @g(ptr %a, ptr %o)"), "{text}");
    }

    /// A call to a `nothing` function (no places) is `call void @g()` followed
    /// by the block's single `ret void` — no phantom out argument.
    #[test]
    fn lowers_void_call_with_no_places() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module
            .funcs
            .push(func("g", &[], &[], vec![ret_block("entry", Vec::new())]));
        let mut insts = Vec::new();
        call(&mut insts, LirCallee::Func("g".into()), &[], &[]);
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(text.contains("call void @g()\n  ret void"), "{text}");
    }

    /// The caller is listed *before* its callee, so this passes only because
    /// every function is declared up front: both the direct call and
    /// `addr.func` resolve a function whose body is lowered later. `emit`
    /// verifies the module.
    #[test]
    fn lowers_forward_referenced_callee() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut insts = Vec::new();
        alloca(&mut insts, "x", "#bit32");
        alloca(&mut insts, "y", "#bit32");
        alloca(&mut insts, "fptr", "#bit64");
        call(&mut insts, LirCallee::Func("later".into()), &["x"], &["y"]);
        insts.push(LirInst::AddrFunc {
            func: "later".into(),
            dst: "1".into(),
            ty: t("#bit64"),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("1".into()), "fptr");
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        module.funcs.push(func(
            "later",
            &[("a", "#bit32")],
            &[("o", "#bit32")],
            vec![ret_block("entry", Vec::new())],
        ));
        let text = emit(&module);
        assert!(text.contains("call void @later(ptr %x, ptr %y)"), "{text}");
        assert!(text.contains("ptrtoint (ptr @later to i64)"), "{text}");
    }

    /// An indirect call through an `addr.func` register round-trips the
    /// function pointer through its declared integer type (`ptrtoint` at the
    /// address, `inttoptr` at the call) and passes the same place pointers.
    #[test]
    fn lowers_indirect_call_through_addr_func() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.funcs.push(func(
            "addOne",
            &[("v", "#bit32")],
            &[("r", "#bit32")],
            vec![ret_block("entry", Vec::new())],
        ));
        let mut insts = Vec::new();
        alloca(&mut insts, "add", "#bit64");
        alloca(&mut insts, "value", "#bit32");
        alloca(&mut insts, "out", "#bit32");
        insts.push(LirInst::AddrFunc {
            func: "addOne".into(),
            dst: "1".into(),
            ty: t("#bit64"),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("1".into()), "add");
        insts.push(LirInst::CopyReg {
            src: LirOperand::Place("add".into()),
            dst: "3".into(),
            ty: t("#bit64"),
            span: span(),
        });
        call(&mut insts, LirCallee::Reg("3".into()), &["value"], &["out"]);
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(text.contains("ptrtoint (ptr @addOne to i64)"), "{text}");
        assert!(text.contains("inttoptr i64"), "{text}");
        assert!(text.contains("call void %"), "{text}");
        assert!(text.contains("(ptr %value, ptr %out)"), "{text}");
    }

    /// Leaf builtin calls use the value ABI: inputs are loaded, the declared
    /// runtime symbol is called, and the single result is stored into the out
    /// place.
    #[test]
    fn lowers_leaf_builtin_call_with_value_abi() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut insts = Vec::new();
        alloca(&mut insts, "base", "ref #bit8");
        alloca(&mut insts, "off", "#bit_native");
        alloca(&mut insts, "out", "ref #bit8");
        call(
            &mut insts,
            LirCallee::Builtin("@ptr_add".to_owned()),
            &["base", "off"],
            &["out"],
        );
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(
            text.contains("define ptr @.lilac.ptr_add(ptr %base, i64 %offset)"),
            "{text}"
        );
        assert!(
            !text.contains("declare ptr @.lilac.ptr_add"),
            "runtime definition must be reused, not re-declared:\n{text}"
        );
        assert!(text.contains("call ptr @.lilac.ptr_add(ptr %"), "{text}");
        assert!(text.contains("store ptr %"), "{text}");
    }

    /// After the runtime moved into LIR, codegen defines only the three leaf
    /// intrinsics (once each), and only when the module calls them; the moved
    /// functions are ordinary LIR now.
    #[test]
    fn emits_each_runtime_leaf_once_with_value_signature() {
        let text = emit(&all_leaves_module());
        for needle in [
            "define void @.lilac.memcpy(ptr %dest, ptr %src, i64 %len)",
            "define ptr @.lilac.ptr_add(ptr %base, i64 %offset)",
            "define void @.lilac.abort()",
        ] {
            assert_eq!(
                text.matches(needle).count(),
                1,
                "`{needle}` must be defined exactly once in:\n{text}"
            );
        }
        for moved in [
            "byte_buffer_from",
            "byte_buffer_ptr",
            "byte_buffer_len",
            "load_u8",
            "store_u8",
            "format_args_new",
            "format_args_push",
            "format_args_count",
            "format_args_piece",
        ] {
            assert!(
                !text.contains(moved),
                "`{moved}` must no longer be emitted by the backend:\n{text}"
            );
        }
    }

    /// `@ptr_add` is emitted as `.lilac.ptr_add(ptr, iNative) -> ptr`, a plain
    /// `getelementptr i8` that keeps the base pointer's provenance (no
    /// integer arithmetic, and no `inbounds`: the result may leave the object).
    #[test]
    fn emits_ptr_add_runtime_definition() {
        let text = emit(&all_leaves_module());
        assert!(
            text.contains("define ptr @.lilac.ptr_add(ptr %base, i64 %offset)"),
            "{text}"
        );
        let body = text
            .split("\ndefine ")
            .find(|segment| segment.starts_with("ptr @.lilac.ptr_add"))
            .expect("ptr_add definition");
        assert!(
            body.contains("getelementptr i8, ptr %base, i64 %offset"),
            "{body}"
        );
        assert!(
            !body.contains("ptrtoint") && !body.contains("inttoptr"),
            "{body}"
        );
    }

    /// A module that calls no leaf builtin emits none of the
    /// three leaf definitions and never declares `llvm.trap`.
    #[test]
    fn culls_all_unused_leaf_builtins() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", Vec::new())]));
        let text = emit(&module);
        for needle in [
            "define void @.lilac.memcpy",
            "define ptr @.lilac.ptr_add",
            "define void @.lilac.abort",
            "declare void @llvm.trap",
        ] {
            assert!(
                !text.contains(needle),
                "unused leaf `{needle}` was emitted in:\n{text}"
            );
        }
    }

    /// Only the leaves a module calls are emitted: a `@memcpy` call emits
    /// `.lilac.memcpy` alone, with no `.lilac.ptr_add`/`.lilac.abort` and no
    /// `llvm.trap` declaration.
    #[test]
    fn emits_only_the_called_leaf_builtin() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut insts = Vec::new();
        for (place, ty) in [
            ("dest", "ref #bit8"),
            ("src", "ref #bit8"),
            ("len", "#bit_native"),
        ] {
            alloca(&mut insts, place, ty);
        }
        call(
            &mut insts,
            LirCallee::Builtin("@memcpy".to_owned()),
            &["dest", "src", "len"],
            &[],
        );
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(text.contains("define void @.lilac.memcpy"), "{text}");
        assert!(!text.contains("define ptr @.lilac.ptr_add"), "{text}");
        assert!(!text.contains("define void @.lilac.abort"), "{text}");
        assert!(!text.contains("declare void @llvm.trap"), "{text}");
    }

    /// A `@ptr_add` call emits `.lilac.ptr_add` but not `.lilac.memcpy`.
    #[test]
    fn emits_ptr_add_only_when_called() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut insts = Vec::new();
        for (place, ty) in [
            ("base", "ref #bit8"),
            ("off", "#bit_native"),
            ("out", "ref #bit8"),
        ] {
            alloca(&mut insts, place, ty);
        }
        call(
            &mut insts,
            LirCallee::Builtin("@ptr_add".to_owned()),
            &["base", "off"],
            &["out"],
        );
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(text.contains("define ptr @.lilac.ptr_add"), "{text}");
        assert!(!text.contains("define void @.lilac.memcpy"), "{text}");
    }

    /// The `@lilac_abort` builtin resolves to the already-emitted
    /// `.lilac.abort` runtime definition, not an undefined `@lilac_abort`.
    #[test]
    fn lilac_abort_builtin_calls_runtime_definition() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut insts = Vec::new();
        call(
            &mut insts,
            LirCallee::Builtin("@lilac_abort".to_owned()),
            &[],
            &[],
        );
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(text.contains("call void @.lilac.abort()"), "{text}");
        assert!(!text.contains("@lilac_abort"), "{text}");
    }

    /// `@trace_head` resolves to the emitted `.lilac.trace_head`, which
    /// returns a zeroed internal pointer global.
    #[test]
    fn trace_head_builtin_returns_the_slot() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut insts = vec![LirInst::Alloca {
            ty: lilac_ir::LirTy::trace_frame().to_ref().to_ref(),
            place: "head".into(),
            span: span(),
        }];
        call(
            &mut insts,
            LirCallee::Builtin("@trace_head".to_owned()),
            &[],
            &["head"],
        );
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(
            text.contains("@.lilac.trace_head.slot = internal thread_local global ptr null"),
            "{text}"
        );
        assert!(text.contains("define ptr @.lilac.trace_head()"), "{text}");
        assert!(text.contains("call ptr @.lilac.trace_head()"), "{text}");
    }

    /// The panic handler (its shadow-stack walk included) lowers to LLVM.
    #[test]
    fn panic_handler_lowers() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.funcs = lilac_ir::build_runtime(8);
        module.funcs.extend(lilac_ir::hook_externs());
        let (handler, data) = lilac_ir::panic_runtime();
        module.funcs.push(handler);
        module.data = data;
        let text = emit(&module);
        assert!(text.contains("define void @.lilac.panic("), "{text}");
        assert!(text.contains("@.lilac.panic.head = private"), "{text}");
    }

    /// The builtin aggregates are their `builtin_layout` bytes on each
    /// target: `#byte_buffer` is a pointer-sized count and a pointer,
    /// `#format_args` a count plus sixteen of them.
    #[test]
    fn builtin_aggregates_follow_builtin_layout() {
        let text = emit(&type_module());
        assert!(text.contains("%pbb = alloca [16 x i8], align 8"), "{text}");
        assert!(text.contains("%pfa = alloca [264 x i8], align 8"), "{text}");
        let triple = "i686-unknown-linux-gnu";
        let mut module = LirModule::new(lilac_target::Target::parse(triple).expect("triple"));
        let mut insts = Vec::new();
        alloca(&mut insts, "bb", "#byte_buffer");
        alloca(&mut insts, "fa", "#format_args");
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = lilac_codegen_llvm::LlvmBackend::new(triple, OptLevel::O0)
            .emit_llvm(&module)
            .expect("emits");
        assert!(text.contains("%bb = alloca [8 x i8], align 4"), "{text}");
        assert!(text.contains("%fa = alloca [132 x i8], align 4"), "{text}");
    }

    /// The backend still declares and defines the `llvm.trap`-backed abort
    /// leaf when a module calls `@lilac_abort`; the moved
    /// `format_args`/`byte_buffer` runtime is gone.
    #[test]
    fn keeps_only_the_trap_backed_leaf_runtime() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut insts = Vec::new();
        call(
            &mut insts,
            LirCallee::Builtin("@lilac_abort".to_owned()),
            &[],
            &[],
        );
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(text.contains("declare void @llvm.trap()"), "{text}");
        assert!(text.contains("call void @llvm.trap()"), "{text}");
        assert!(text.contains("define void @.lilac.abort()"), "{text}");
    }

    /// The LIR prelude's `format_args` bounds checks are unsigned: an index
    /// with the high bit set must trap, not read out of bounds.
    #[test]
    fn format_args_bounds_checks_are_unsigned() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.funcs = lilac_ir::runtime::build_runtime(8);
        let text = emit(&module);
        for name in [".lilac.format_args_push", ".lilac.format_args_piece"] {
            let body = text
                .split("\ndefine ")
                .find(|segment| segment.starts_with(&format!("void @{name}")))
                .expect("format_args definition");
            assert!(body.contains("icmp ult i64"), "{name} body:\n{body}");
            assert!(!body.contains("icmp slt"), "{name} body:\n{body}");
        }
    }

    /// `branch` is an unconditional `br`; `branch.if` is a conditional `br`
    /// with explicit then/else edges.
    #[test]
    fn lowers_branch_and_branch_if() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut entry = Vec::new();
        alloca(&mut entry, "c", "#bit8");
        entry.push(LirInst::CopyReg {
            src: LirOperand::Place("c".into()),
            dst: "1".into(),
            ty: t("#bit8"),
            span: span(),
        });
        let entry = LirBlock {
            label: "entry".into(),
            insts: entry,
            term: LirTerm::BranchIf {
                cond: "1".into(),
                then: "then".into(),
                els: Some("els".into()),
                span: span(),
            },
            span: span(),
        };
        let then = LirBlock {
            label: "then".into(),
            insts: Vec::new(),
            term: LirTerm::Branch {
                target: "join".into(),
                span: span(),
            },
            span: span(),
        };
        let els = LirBlock {
            label: "els".into(),
            insts: Vec::new(),
            term: LirTerm::Branch {
                target: "join".into(),
                span: span(),
            },
            span: span(),
        };
        module.funcs.push(func(
            "main",
            &[],
            &[],
            vec![entry, then, els, ret_block("join", Vec::new())],
        ));
        let text = emit(&module);
        assert!(text.contains("br i1"), "{text}");
        assert!(text.contains(", label %then, label %els"), "{text}");
        assert!(text.contains("br label %join"), "{text}");
    }

    /// `branch.if` with no `els` branches to the next block in layout order on
    /// the false edge.
    #[test]
    fn lowers_branch_if_fallthrough() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut entry = Vec::new();
        alloca(&mut entry, "c", "#bit8");
        entry.push(LirInst::CopyReg {
            src: LirOperand::Place("c".into()),
            dst: "1".into(),
            ty: t("#bit8"),
            span: span(),
        });
        let entry = LirBlock {
            label: "entry".into(),
            insts: entry,
            term: LirTerm::BranchIf {
                cond: "1".into(),
                then: "then".into(),
                els: None,
                span: span(),
            },
            span: span(),
        };
        let then = LirBlock {
            label: "then".into(),
            insts: Vec::new(),
            term: LirTerm::Branch {
                target: "join".into(),
                span: span(),
            },
            span: span(),
        };
        module.funcs.push(func(
            "main",
            &[],
            &[],
            vec![entry, ret_block("join", Vec::new()), then],
        ));
        let text = emit(&module);
        assert!(text.contains("br i1"), "{text}");
        assert!(text.contains(", label %then, label %join"), "{text}");
    }

    /// `return` → `ret void`, `unreachable` → `unreachable`, and `drop` emits
    /// nothing.
    #[test]
    fn lowers_terminators_and_drop() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module
            .funcs
            .push(func("ret", &[], &[], vec![ret_block("entry", Vec::new())]));
        let unreachable = LirBlock {
            label: "entry".into(),
            insts: Vec::new(),
            term: LirTerm::Unreachable { span: span() },
            span: span(),
        };
        module.funcs.push(func("dead", &[], &[], vec![unreachable]));
        let mut drop_insts = Vec::new();
        alloca(&mut drop_insts, "p", "#bit32");
        drop_insts.push(LirInst::Drop {
            place: "p".into(),
            span: span(),
        });
        module.funcs.push(func(
            "plain",
            &[],
            &[],
            vec![ret_block("entry", drop_insts)],
        ));
        let text = emit(&module);
        assert!(text.contains("ret void"), "{text}");
        assert!(text.contains("unreachable"), "{text}");
        assert!(!text.contains("drop"), "drop must not lower:\n{text}");
    }

    /// A data-symbol `const` is its address: the verifier rejects one of an
    /// aggregate type (registers hold scalars), and the backend, given one
    /// anyway, raises an ICE rather than build an aggregate value.
    #[test]
    #[should_panic(expected = "cannot be a")]
    fn aggregate_data_const_is_ice() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module.data.push(LirData {
            name: "c0".into(),
            bytes: "hi".as_bytes().to_vec(),
            span: span(),
        });
        module.funcs.push(LirFunc {
            name: "f".into(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            export: None,
            variadic: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".into(),
                insts: vec![LirInst::Const {
                    dst: "x".into(),
                    value: LirConst::Data("c0".into()),
                    ty: t("#format_args"),
                    span: span(),
                }],
                term: LirTerm::Return { span: span() },
                span: span(),
            }],
        });
        let _ = emit(&module);
    }

    #[test]
    #[should_panic(expected = "unknown LIR type")]
    fn unknown_type_is_ice() {
        let context = inkwell::context::Context::create();
        let module = context.create_module("ice");
        let lir =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let lower = crate::lower::Lower::new(&context, module, &lir, "x86_64-pc-windows-msvc");
        let _ = lower.lir_type(&t("#bogus"));
    }

    #[test]
    fn lir_type_maps_fbit_types() {
        let context = inkwell::context::Context::create();
        let module = context.create_module("fbits");
        let lir =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let lower = crate::lower::Lower::new(&context, module, &lir, "x86_64-pc-windows-msvc");
        for (ty, expected) in [
            ("#fbit16", "half"),
            ("#fbit32", "float"),
            ("#fbit64", "double"),
            ("#fbit128", "fp128"),
        ] {
            assert_eq!(
                lower.lir_type(&t(ty)).print_to_string().to_string(),
                expected,
                "{ty}"
            );
        }
    }

    #[test]
    fn lowers_fbit_bitcast() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let insts = vec![
            LirInst::CopyReg {
                src: LirOperand::Place("x".into()),
                dst: "r0".into(),
                ty: t("#bit32"),
                span: span(),
            },
            LirInst::Convert {
                op: ConvertOp::Bitcast,
                src: "r0".into(),
                dst: "r1".into(),
                ty: t("#fbit32"),
                span: span(),
            },
            LirInst::CopyPlace {
                src: LirOperand::Reg("r1".into()),
                dst: "out".into(),
                span: span(),
            },
        ];
        module.funcs.push(LirFunc {
            name: "cast".into(),
            ins: vec![("x".into(), t("#bit32"))],
            outs: vec![("out".into(), t("#fbit32"))],
            is_extern: false,
            export: None,
            variadic: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".into(),
                insts,
                term: LirTerm::Return { span: span() },
                span: span(),
            }],
        });
        let text = emit(&module);
        assert!(text.contains("bitcast i32"), "{text}");
        assert!(text.contains("to float"), "{text}");
    }

    /// `==` on two `@fbit32` operands is strict bit-equality: the operands are
    /// bitcast to `i32` and compared with `icmp`, never `fcmp`.
    #[test]
    fn lowers_float_eq_as_bit_equality() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let insts = vec![
            LirInst::CopyReg {
                src: LirOperand::Place("a".into()),
                dst: "r0".into(),
                ty: t("#fbit32"),
                span: span(),
            },
            LirInst::CopyReg {
                src: LirOperand::Place("b".into()),
                dst: "r1".into(),
                ty: t("#fbit32"),
                span: span(),
            },
            LirInst::Arith {
                op: ArithOp::Eq,
                lhs: "r0".into(),
                rhs: "r1".into(),
                dst: "r2".into(),
                ty: t("#bit8"),
                span: span(),
            },
            LirInst::CopyPlace {
                src: LirOperand::Reg("r2".into()),
                dst: "out".into(),
                span: span(),
            },
        ];
        module.funcs.push(LirFunc {
            name: "feq".into(),
            ins: vec![("a".into(), t("#fbit32")), ("b".into(), t("#fbit32"))],
            outs: vec![("out".into(), t("#bit8"))],
            is_extern: false,
            export: None,
            variadic: false,
            inline: false,
            blocks: vec![LirBlock {
                label: "entry".into(),
                insts,
                term: LirTerm::Return { span: span() },
                span: span(),
            }],
        });
        let text = emit(&module);
        assert!(text.contains("bitcast float"), "{text}");
        assert!(text.contains("icmp eq i32"), "{text}");
        assert!(!text.contains("fcmp"), "{text}");
    }

    #[test]
    fn default_emit_llvm_is_unsupported() {
        struct Noop;
        impl lilac_codegen::Backend for Noop {
            fn name(&self) -> &'static str {
                "noop"
            }
        }
        let m = lilac_ir::LirModule::new(
            lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"),
        );
        let err = Noop.emit_llvm(&m).expect_err("noop cannot emit LLVM IR");
        assert!(err.contains("noop"), "error must name the backend: {err}");
        let err = Noop
            .emit_object(&m, Entry::CMain, &mut |_| {})
            .expect_err("noop cannot emit objects");
        assert!(err.contains("noop"), "error must name the backend: {err}");
    }

    fn main_module(triple: &str) -> LirModule {
        let mut module = LirModule::new(lilac_target::Target::parse(triple).expect("triple"));
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", Vec::new())]));
        module
    }

    /// An executable: `main` with `insts`, an `@entry` hook (`.lilac.entry`)
    /// calling it, the other two hooks declared, and the entry stub.
    fn executable_module(triple: &str, insts: Vec<LirInst>) -> LirModule {
        let mut module = LirModule::new(lilac_target::Target::parse(triple).expect("triple"));
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let mut entry = Vec::new();
        call(&mut entry, LirCallee::Func("main".into()), &[], &[]);
        module.funcs.push(func(
            ".lilac.entry",
            &[
                ("count", "#bit_native"),
                ("arguments", "ref #unknown"),
                ("environment", "ref #unknown"),
            ],
            &[],
            vec![ret_block("entry", entry)],
        ));
        module.funcs.extend(lilac_ir::hook_externs());
        module.funcs.push(lilac_ir::entry_point());
        module
    }

    /// A COFF x86-64 object starts with the AMD64 machine field, and the
    /// steps fire in order (no `Optimize` at O0).
    #[test]
    fn emits_coff_object_with_steps() {
        let mut steps = Vec::new();
        let bytes = backend()
            .emit_object(
                &main_module("x86_64-pc-windows-msvc"),
                Entry::CMain,
                &mut |step| steps.push(step),
            )
            .expect("object");
        assert_eq!(&bytes[..2], &[0x64, 0x86]);
        assert_eq!(
            steps,
            [CodegenStep::Lower, CodegenStep::Verify, CodegenStep::Emit]
        );
    }

    /// An ELF target gets an ELF object; `Optimize` fires above O0.
    #[test]
    fn emits_elf_object_when_optimizing() {
        let backend =
            lilac_codegen_llvm::LlvmBackend::new("x86_64-unknown-linux-gnu", OptLevel::O2);
        let mut steps = Vec::new();
        let bytes = backend
            .emit_object(
                &main_module("x86_64-unknown-linux-gnu"),
                Entry::CMain,
                &mut |step| steps.push(step),
            )
            .expect("object");
        assert_eq!(&bytes[..4], b"\x7fELF");
        assert!(steps.contains(&CodegenStep::Optimize));
    }

    /// The native IR for `triple`, as `emit_object` builds it.
    fn native_ir(triple: &str) -> String {
        let backend = lilac_codegen_llvm::LlvmBackend::new(triple, OptLevel::O0);
        let context = inkwell::context::Context::create();
        let (module, _) = backend
            .build_module(
                &context,
                &main_module(triple),
                inkwell::targets::RelocMode::Default,
                Entry::Native,
                &mut |_| {},
            )
            .expect("builds");
        module.print_to_string().to_string()
    }

    /// A native object has no C `main` wrapper and defines what LLVM may call
    /// in place of the C runtime; Windows adds `_fltused` and `__chkstk`.
    #[test]
    fn native_entry_defines_its_own_support() {
        let windows = native_ir("x86_64-pc-windows-msvc");
        assert!(!windows.contains("define i32 @main()"), "{windows}");
        for symbol in ["@memcpy(", "@memmove(", "@memset("] {
            assert!(
                windows.contains(&format!("define ptr {symbol}")),
                "{symbol}"
            );
        }
        assert!(windows.contains("define i32 @memcmp("), "{windows}");
        assert!(windows.contains("\"no-builtins\""), "{windows}");
        assert!(
            !windows.contains("ptrtoint") && !windows.contains("inttoptr"),
            "native support addresses bytes with `getelementptr`:\n{windows}"
        );
        assert!(windows.contains("@_fltused = global i32 0"), "{windows}");
        assert!(windows.contains("__chkstk:"), "{windows}");
        let arm = native_ir("aarch64-pc-windows-msvc");
        assert!(arm.contains("ldr xzr"), "{arm}");
        let linux = native_ir("x86_64-unknown-linux-gnu");
        assert!(
            !linux.contains("_fltused") && !linux.contains("__chkstk"),
            "{linux}"
        );
        assert!(linux.contains("define ptr @memcpy("), "{linux}");
    }

    /// `@linux_syscall` reaches the kernel through `.lilac.linux_syscall`, the
    /// architecture's system-call instruction, and the process is entered
    /// through the assembly `.lilac.native_entry`, which calls
    /// `.lilac.native_main` with the arguments from the stack.
    #[test]
    fn linux_system_calls_and_entry() {
        for (triple, instruction, entry) in [
            (
                "x86_64-unknown-linux-gnu",
                "asm sideeffect \"syscall\"",
                "leaq 16(%rsp,%rdi,8), %rdx",
            ),
            (
                "aarch64-unknown-linux-gnu",
                "asm sideeffect \"svc #0\"",
                "add x2, x1, x0, lsl #3",
            ),
        ] {
            let mut insts = Vec::new();
            let args = ["n", "a0", "a1", "a2", "a3", "a4", "a5"];
            for place in args.iter().chain(&["r"]) {
                alloca(&mut insts, place, "#bit_native");
            }
            call(
                &mut insts,
                LirCallee::Builtin("@linux_syscall".to_owned()),
                &args,
                &["r"],
            );
            let module = executable_module(triple, insts);
            let backend = lilac_codegen_llvm::LlvmBackend::new(triple, OptLevel::O0);
            let context = inkwell::context::Context::create();
            let (built, _) = backend
                .build_module(
                    &context,
                    &module,
                    inkwell::targets::RelocMode::PIC,
                    Entry::Native,
                    &mut |_| {},
                )
                .expect("builds");
            let text = built.print_to_string().to_string();
            assert!(text.contains("define i64 @.lilac.linux_syscall("), "{text}");
            assert!(text.contains(instruction), "{triple}: {text}");
            assert!(text.contains(entry), "{triple}: {text}");
            assert!(
                text.contains("define void @.lilac.native_main(i64"),
                "{triple}: {text}"
            );
        }
    }

    /// The native module text of an executable whose `main` runs `insts`.
    fn native_text(triple: &str, insts: Vec<LirInst>) -> String {
        let module = executable_module(triple, insts);
        let backend = lilac_codegen_llvm::LlvmBackend::new(triple, OptLevel::O0);
        let context = inkwell::context::Context::create();
        let (built, _) = backend
            .build_module(
                &context,
                &module,
                inkwell::targets::RelocMode::PIC,
                Entry::Native,
                &mut |_| {},
            )
            .expect("builds");
        built.print_to_string().to_string()
    }

    /// The trace head and a thread's start, in `insts`.
    fn thread_insts(clone: bool) -> Vec<LirInst> {
        let mut insts = Vec::new();
        alloca(&mut insts, "head", "ref ref #.lilac.frame");
        call(
            &mut insts,
            LirCallee::Builtin("@trace_head".to_owned()),
            &[],
            &["head"],
        );
        if clone {
            let args = ["f", "s", "p", "c", "a", "n", "e", "st"];
            for (place, ty) in args.iter().zip([
                "#bit_native",
                "ref #unknown",
                "ref #unknown",
                "ref #unknown",
                "ref #unknown",
                "#bit_native",
                "ref #unknown",
                "ref #unknown",
            ]) {
                alloca(&mut insts, place, ty);
            }
            alloca(&mut insts, "id", "#bit_native");
            call(
                &mut insts,
                LirCallee::Builtin("@linux_thread_clone".to_owned()),
                &args,
                &["id"],
            );
        }
        insts
    }

    /// Linux threads: `.lilac.linux_thread_clone` lays out the TLS block and runs
    /// the assembly `clone`, whose child calls the entry it is given; the
    /// trace head is a local-exec thread-local, so `.lilac.native_main`
    /// first sets up the main thread's block. The entry assembly survives
    /// the clone assembly being appended.
    #[test]
    fn linux_threads() {
        for (triple, entry) in [
            ("x86_64-unknown-linux-gnu", "leaq 16(%rsp,%rdi,8), %rdx"),
            ("aarch64-unknown-linux-gnu", "add x2, x1, x0, lsl #3"),
        ] {
            let text = native_text(triple, thread_insts(true));
            for want in [
                "@.lilac.trace_head.slot = internal thread_local(localexec) global ptr null",
                "define i64 @.lilac.linux_thread_clone(i64",
                "define ptr @.lilac.tls_init(ptr",
                "\\22.lilac.clone_raw\\22:",
                "call void @.lilac.main_tls()",
                entry,
            ] {
                assert!(text.contains(want), "{triple}: {want}: {text}");
            }
        }
    }

    /// Native Windows: the thread-local trace head brings the TLS directory
    /// a C runtime would define, beside `__chkstk`.
    #[test]
    fn windows_thread_locals() {
        let text = native_text("x86_64-pc-windows-msvc", thread_insts(false));
        for want in [
            "@.lilac.trace_head.slot = internal thread_local global ptr null",
            "@_tls_used = constant",
            "@_tls_index = global i32 0",
            "__chkstk:",
        ] {
            assert!(text.contains(want), "{want}: {text}");
        }
    }

    /// A native macOS module defines none of the C-runtime replacements: it
    /// links `libSystem`, which has them.
    #[test]
    fn macos_native_uses_libsystem() {
        let triple = "aarch64-apple-darwin";
        let module = executable_module(triple, Vec::new());
        let backend = lilac_codegen_llvm::LlvmBackend::new(triple, OptLevel::O0);
        let context = inkwell::context::Context::create();
        let (built, _) = backend
            .build_module(
                &context,
                &module,
                inkwell::targets::RelocMode::PIC,
                Entry::Native,
                &mut |_| {},
            )
            .expect("builds");
        let text = built.print_to_string().to_string();
        assert!(text.contains("define void @.lilac.start("), "{text}");
        assert!(
            text.contains("define i32 @.lilac.native_entry(i32"),
            "{text}"
        );
        for symbol in ["@memcpy(", "@memmove(", "@memset("] {
            assert!(!text.contains(symbol), "{symbol}: {text}");
        }
    }

    /// `Entry::Native` emits a COFF object (linking and running it is covered
    /// by the CLI's build and run tests).
    #[test]
    fn native_entry_emits_an_object() {
        let bytes = backend()
            .emit_object(
                &main_module("x86_64-pc-windows-msvc"),
                Entry::Native,
                &mut |_| {},
            )
            .expect("native object");
        assert_eq!(bytes.get(..2), Some(&[0x64, 0x86][..]));
    }

    /// Every target is initialized, not just the host: emitting for a triple
    /// that is not the build host's architecture must succeed. `aarch64-apple-
    /// darwin` is deliberately neither `x86_64` nor Windows.
    #[test]
    fn emits_for_non_native_triple() {
        let backend = lilac_codegen_llvm::LlvmBackend::new("aarch64-apple-darwin", OptLevel::O0);
        let module = lilac_ir::LirModule::new(
            lilac_target::Target::parse("aarch64-apple-darwin").expect("triple"),
        );
        let text = backend.emit_llvm(&module).expect("non-native target emits");
        assert!(
            text.contains("target triple = \"aarch64-apple-darwin\""),
            "{text}"
        );
    }

    #[test]
    fn empty_module_emits_verifyable_ir() {
        let backend = lilac_codegen_llvm::LlvmBackend::new(
            "x86_64-pc-windows-msvc",
            lilac_codegen::OptLevel::O0,
        );
        let module = lilac_ir::LirModule::new(
            lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"),
        );
        let text = backend.emit_llvm(&module).expect("emits");
        assert!(
            text.contains("target triple = \"x86_64-pc-windows-msvc\""),
            "{text}"
        );
    }

    fn backend_with(optimize: OptLevel) -> lilac_codegen_llvm::LlvmBackend {
        lilac_codegen_llvm::LlvmBackend::new("x86_64-pc-windows-msvc", optimize)
    }

    /// A LIR `main` is emitted as `.lilac.main`, and an executable gains an
    /// `i32 @main(argc, argv, envp)` wrapper that calls the entry stub, which
    /// never returns.
    #[test]
    fn emits_main_entry_wrapper() {
        let text = emit(&executable_module("x86_64-pc-windows-msvc", Vec::new()));
        assert!(text.contains("define void @.lilac.main()"), "{text}");
        assert!(text.contains("define i32 @main(i32"), "{text}");
        assert!(text.contains("call void @.lilac.start(ptr"), "{text}");
        assert!(text.contains("call void @.lilac.main()"), "{text}");
    }

    /// A value place passed to a `ref` parameter is spilled into a pointer
    /// slot: the callee expects the address of a reference slot (and loads the
    /// pointer out of it), not the value's own address.
    #[test]
    fn coerces_value_argument_to_ref_parameter() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        module
            .layouts
            .insert("Pair".to_owned(), struct_layout(&[("a", "#bit32")]));
        module.funcs.push(func(
            "hook",
            &[("from", "ref #Pair")],
            &[],
            vec![ret_block("entry", Vec::new())],
        ));
        let mut insts = Vec::new();
        alloca(&mut insts, "p", "#Pair");
        call(&mut insts, LirCallee::Func("hook".into()), &["p"], &[]);
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(text.contains("define void @hook(ptr %in0)"), "{text}");
        assert!(text.contains("store ptr %p, ptr %ref.arg"), "{text}");
        assert!(text.contains("call void @hook(ptr %ref.arg)"), "{text}");
    }

    /// A module without the entry stub (not an executable) gets no C entry
    /// wrapper, even with a `main`.
    #[test]
    fn omits_entry_wrapper_without_the_stub() {
        let text = emit(&LirModule::new(
            lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"),
        ));
        assert!(!text.contains(".lilac.main"), "{text}");
        assert!(!text.contains("define i32 @main"), "{text}");
        let text = emit(&main_module("x86_64-pc-windows-msvc"));
        assert!(text.contains("define void @.lilac.main()"), "{text}");
        assert!(!text.contains("define i32 @main"), "{text}");
    }

    /// A `call` and an `addr.func` targeting LIR `main` are rewritten to the
    /// renamed `.lilac.main` symbol.
    #[test]
    fn renames_calls_and_addr_func_to_main() {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut insts = Vec::new();
        alloca(&mut insts, "fptr", "#bit64");
        call(&mut insts, LirCallee::Func("main".into()), &[], &[]);
        insts.push(LirInst::AddrFunc {
            func: "main".into(),
            dst: "1".into(),
            ty: t("#bit64"),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("1".into()), "fptr");
        module
            .funcs
            .push(func("h", &[], &[], vec![ret_block("entry", insts)]));
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", Vec::new())]));
        let text = emit(&module);
        assert!(text.contains("call void @.lilac.main()"), "{text}");
        assert!(
            text.contains("ptrtoint (ptr @.lilac.main to i64)"),
            "{text}"
        );
        assert!(!text.contains("call void @main()"), "{text}");
        assert!(!text.contains("ptr @main to i64"), "{text}");
    }

    /// A function whose arithmetic is redundant under a loaded value: `-O0`
    /// keeps the `add`/`mul`, `-O2` folds them away, so the two printouts
    /// differ (the pass pipeline ran).
    fn redundant_arith_module() -> LirModule {
        let mut module =
            LirModule::new(lilac_target::Target::parse("x86_64-pc-windows-msvc").expect("triple"));
        let mut insts = Vec::new();
        alloca(&mut insts, "a", "#bit32");
        insts.push(LirInst::Const {
            dst: "one".into(),
            value: LirConst::Int(1),
            ty: t("#bit32"),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("one".into()), "a");
        insts.push(LirInst::CopyReg {
            src: LirOperand::Place("a".into()),
            dst: "ra".into(),
            ty: t("#bit32"),
            span: span(),
        });
        insts.push(LirInst::CopyReg {
            src: LirOperand::Place("a".into()),
            dst: "rb".into(),
            ty: t("#bit32"),
            span: span(),
        });
        arith(&mut insts, ArithOp::IAdd, "ra", "rb", "sum", "#bit32");
        arith(&mut insts, ArithOp::IMul, "sum", "ra", "prod", "#bit32");
        copy_place(&mut insts, LirOperand::Reg("prod".into()), "a");
        module
            .funcs
            .push(func("f", &[], &[], vec![ret_block("entry", insts)]));
        module
    }

    #[test]
    fn o2_runs_pass_pipeline() {
        let module = redundant_arith_module();
        let o0 = backend_with(OptLevel::O0)
            .emit_llvm(&module)
            .expect("O0 emits");
        let o2 = backend_with(OptLevel::O2)
            .emit_llvm(&module)
            .expect("O2 emits");
        assert_ne!(o0, o2, "the -O2 pipeline must change the IR");
    }
}

/// The target table against LLVM: every target `Target::parse` accepts
/// builds a `TargetMachine` (2.5), and LLVM's data layout agrees with
/// `Target::scalar_layout` on every scalar (2.6) and with `lilac-layout` on
/// a struct of them, so the table cannot drift from what LLVM emits.
#[cfg(test)]
mod target_tests {
    use inkwell::context::Context;
    use inkwell::targets::{
        CodeModel, InitializationConfig, RelocMode, Target as LlvmTarget, TargetMachine,
        TargetTriple,
    };
    use inkwell::types::BasicTypeEnum;
    use inkwell::{AddressSpace, OptimizationLevel};
    use lilac_codegen::{Backend, OptLevel};
    use lilac_ir::{LirLayout, LirModule, LirTy};
    use lilac_target::{Abi, Arch, Os, Scalar, Target, Vendor};

    /// Every four-component triple `Target::parse` accepts.
    fn accepted() -> Vec<Target> {
        let mut targets = Vec::new();
        for arch in Arch::ALL {
            for vendor in Vendor::ALL {
                for os in Os::ALL {
                    for abi in Abi::ALL {
                        let triple = format!(
                            "{}-{}-{}-{}",
                            arch.as_str(),
                            vendor.as_str(),
                            os.as_str(),
                            abi.as_str()
                        );
                        if let Ok(target) = Target::parse(&triple) {
                            targets.push(target);
                        }
                    }
                }
            }
        }
        targets
    }

    fn machine(target: Target) -> TargetMachine {
        LlvmTarget::initialize_all(&InitializationConfig::default());
        let triple = TargetTriple::create(&target.to_triple());
        let llvm = LlvmTarget::from_triple(&triple)
            .unwrap_or_else(|error| panic!("LLVM has no target `{target}`: {error}"));
        llvm.create_target_machine(
            &triple,
            "generic",
            "",
            OptimizationLevel::None,
            RelocMode::Default,
            CodeModel::Default,
        )
        .unwrap_or_else(|| panic!("no target machine for `{target}`"))
    }

    #[test]
    fn every_accepted_target_builds_a_target_machine() {
        let targets = accepted();
        assert!(targets.len() > 300, "{} targets", targets.len());
        for target in &targets {
            machine(*target);
        }
    }

    #[test]
    fn scalar_layouts_match_llvm() {
        let context = Context::create();
        for target in accepted() {
            let data = machine(target).get_target_data();
            for scalar in Scalar::ALL {
                let ty: BasicTypeEnum = match scalar {
                    Scalar::Bit8 => context.i8_type().into(),
                    Scalar::Bit16 => context.i16_type().into(),
                    Scalar::Bit32 => context.i32_type().into(),
                    Scalar::Bit64 => context.i64_type().into(),
                    Scalar::Float16 => context.f16_type().into(),
                    Scalar::Float32 => context.f32_type().into(),
                    Scalar::Float64 => context.f64_type().into(),
                    Scalar::Float128 => context.f128_type().into(),
                    Scalar::Pointer => context.ptr_type(AddressSpace::default()).into(),
                };
                let llvm = (
                    data.get_abi_size(&ty),
                    u64::from(data.get_abi_alignment(&ty)),
                );
                assert_eq!(target.scalar_layout(scalar), llvm, "{target} {scalar:?}");
            }
            // `@bit_native` is LLVM's pointer-sized integer.
            let native = context.ptr_sized_int_type(&data, None);
            assert_eq!(
                target.scalar_layout(Scalar::Pointer),
                (
                    data.get_abi_size(&native),
                    u64::from(data.get_abi_alignment(&native))
                ),
                "{target} @bit_native"
            );
        }
    }

    /// The backend checks every layout against LLVM's placement; a struct of
    /// every scalar, an array and a nested struct lowers on every supported
    /// target.
    #[test]
    fn layouts_lower_on_every_supported_target() {
        let t = |text: &str| LirTy::parse(text).expect("type spelling");
        for target in Target::supported() {
            let mut module = LirModule::new(target);
            let inner = LirLayout::of_struct(
                vec![("a".to_owned(), t("#bit8")), ("b".to_owned(), t("#fbit64"))],
                &module.layouts,
                &target,
            )
            .expect("fits");
            module.layouts.insert("Inner".to_owned(), inner);
            let fields = [
                "#bit8",
                "#bit64",
                "#bit16",
                "#fbit128",
                "#bit8",
                "#fbit16",
                "#bit32",
                "ref #Inner",
                "#fbit32",
                "#bit_native",
                "#Inner",
                "[3 x #Inner]",
                "#byte_buffer",
            ]
            .iter()
            .enumerate()
            .map(|(index, ty)| (format!("f{index}"), t(ty)))
            .collect();
            let outer = LirLayout::of_struct(fields, &module.layouts, &target).expect("fits");
            module.layouts.insert("Outer".to_owned(), outer);
            let backend = crate::LlvmBackend::new(&target.to_triple(), OptLevel::O0);
            backend
                .emit_llvm(&module)
                .unwrap_or_else(|error| panic!("{target}: {error}"));
        }
    }
}
