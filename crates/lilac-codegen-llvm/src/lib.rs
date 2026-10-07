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
    CodeModel, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use inkwell::OptimizationLevel;

use lilac_codegen::{Backend, OptLevel};
use lilac_error::Emitter;
use lilac_ir::LirModule;
use lilac_target::is_64_bit_triple;

mod lower;
mod runtime;

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
    /// populates the module; [`Backend::emit_llvm`] verifies and optimizes it.
    fn prepare_module<'ctx>(
        &self,
        context: &'ctx Context,
    ) -> Result<(Module<'ctx>, TargetMachine), String> {
        let module = context.create_module("lilac");
        let triple = TargetTriple::create(&self.triple);
        module.set_triple(&triple);

        Target::initialize_all(&InitializationConfig::default());
        let target = Target::from_triple(&triple)
            .map_err(|e| format!("unknown target triple `{}`: {e}", self.triple))?;
        let (optimize, _) = self.optimize_settings();
        let machine: TargetMachine = target
            .create_target_machine(
                &triple,
                "generic",
                "",
                optimize,
                RelocMode::Default,
                CodeModel::Default,
            )
            .ok_or_else(|| format!("failed to create target machine for `{}`", self.triple))?;
        module.set_data_layout(&machine.get_target_data().get_data_layout());

        Ok((module, machine))
    }
}

impl Backend for LlvmBackend {
    fn name(&self) -> &'static str {
        "llvm"
    }

    fn codegen(&self, _module: &LirModule) -> Result<(), String> {
        Err("LLVM object codegen is not implemented; `emit-llvm` emits textual IR".to_owned())
    }

    fn emit_llvm(&self, lir: &LirModule) -> Result<String, String> {
        let context = Context::create();
        let (module, machine) = self.prepare_module(&context)?;
        let mut lower = lower::Lower::new(&context, module, lir, is_64_bit_triple(&self.triple));
        if let Err(diagnostic) = lower.run() {
            return Err(lilac_error::EmitterHuman::new()
                .render(&diagnostic, &lilac_span::SourceMap::default()));
        }
        let module = lower.into_module();
        module
            .verify()
            .map_err(|e| format!("module verification failed: {e}"))?;
        if let Some(pipeline) = self.optimize_settings().1 {
            module
                .run_passes(pipeline, &machine, PassBuilderOptions::create())
                .map_err(|e| format!("optimization pipeline `{pipeline}` failed: {e}"))?;
        }
        Ok(module.print_to_string().to_string())
    }
}

#[cfg(test)]
mod tests {
    use crate as lilac_codegen_llvm;
    use lilac_codegen::{Backend, OptLevel};
    use lilac_ir::{
        ArithOp, ConvertOp, LirBlock, LirCallee, LirData, LirDataKind, LirFunc, LirInst, LirModule,
        LirOperand, LirTerm,
    };
    use lilac_span::Span;

    fn span() -> Span {
        Span::dummy()
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.layouts.insert(
            "#Pair".to_owned(),
            vec![
                ("a".to_owned(), "#bit32".to_owned()),
                ("b".to_owned(), "#bit8".to_owned()),
            ],
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
                ty: (*ty).to_owned(),
                place: (*place).to_owned(),
                span: span(),
            })
            .collect();
        module.funcs.push(LirFunc {
            name: "f".to_owned(),
            ins: vec![
                ("a".to_owned(), "#bit8".to_owned()),
                ("b".to_owned(), "#bit16".to_owned()),
                ("c".to_owned(), "#bit32".to_owned()),
                ("d".to_owned(), "#bit64".to_owned()),
                ("e".to_owned(), "#bit_native".to_owned()),
                ("r".to_owned(), "ref #bit32".to_owned()),
                ("bb".to_owned(), "#byte_buffer".to_owned()),
                ("fa".to_owned(), "#format_args".to_owned()),
            ],
            outs: vec![("out".to_owned(), "#bit32".to_owned())],
            is_extern: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts,
                term: LirTerm::Return { span: span() },
                span: span(),
            }],
        });
        module
    }

    /// The `#format_args` LLVM struct is the 17-field expansion of
    /// `builtin_layout` (a count plus sixteen named pieces), not an array.
    fn format_args_llvm_type() -> String {
        format!("{{ i64, {0} }}", ["{ i64, ptr }"; 16].join(", "))
    }

    #[test]
    fn lowers_types_layouts_and_zero_init() {
        let text = emit(&type_module());
        let format_args = format_args_llvm_type();
        for needle in [
            "i8",
            "i16",
            "i32",
            "i64",
            "ptr",
            "%Pair = type { i32, i8 }",
            "{ i64, ptr }",
            format_args.as_str(),
        ] {
            assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
        }
        // Scalar zero values print as `0`/`null`; aggregate ones as
        // `zeroinitializer` (LLVM canonicalizes both to the same constant).
        let format_args_store = format!("store {format_args} zeroinitializer");
        for needle in [
            "store i8 0",
            "store i16 0",
            "store i32 0",
            "store i64 0",
            "store ptr null",
            "store %Pair zeroinitializer",
            "store { i64, ptr } zeroinitializer",
            format_args_store.as_str(),
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.funcs.push(LirFunc {
            name: "write".to_owned(),
            ins: vec![
                ("fd".to_owned(), "#bit32".to_owned()),
                ("buf".to_owned(), "ref #bit8".to_owned()),
                ("n".to_owned(), "#bit_native".to_owned()),
            ],
            outs: vec![("written".to_owned(), "#bit_native".to_owned())],
            is_extern: true,
            blocks: Vec::new(),
        });
        let text = emit(&module);
        assert!(text.contains("declare i64 @write(i32, ptr, i64)"), "{text}");
        assert!(!text.contains("define i64 @write"), "{text}");
    }

    /// A call to an `extern` uses the platform C ABI: scalar `in` places are
    /// loaded and passed by value, `ref T` places pass their pointer, and the
    /// single return is stored into the out place — not the internal
    /// pointer-place ABI.
    #[test]
    fn extern_call_uses_c_abi() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.data.push(LirData {
            name: "data".to_owned(),
            kind: LirDataKind::Str("hi".to_owned()),
            span: span(),
        });
        module.funcs.push(LirFunc {
            name: "write".to_owned(),
            ins: vec![
                ("fd".to_owned(), "#bit32".to_owned()),
                ("buf".to_owned(), "ref #bit8".to_owned()),
                ("n".to_owned(), "#bit_native".to_owned()),
            ],
            outs: vec![("written".to_owned(), "#bit_native".to_owned())],
            is_extern: true,
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
            dst: "1".to_owned(),
            lit: "1".to_owned(),
            ty: "#bit32".to_owned(),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("1".to_owned()), "fd");
        insts.push(LirInst::Const {
            dst: "2".to_owned(),
            lit: "!data".to_owned(),
            ty: "ref #bit8".to_owned(),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("2".to_owned()), "buf");
        insts.push(LirInst::Const {
            dst: "3".to_owned(),
            lit: "2".to_owned(),
            ty: "#bit_native".to_owned(),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("3".to_owned()), "n");
        call(
            &mut insts,
            LirCallee::Func("write".to_owned()),
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

    /// An `extern` parameter outside the scalar/pointer set is `E028`, not a
    /// mis-compiled struct pointer.
    #[test]
    fn extern_aggregate_param_is_e028() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.funcs.push(LirFunc {
            name: "puts".to_owned(),
            ins: vec![("s".to_owned(), "#byte_buffer".to_owned())],
            outs: Vec::new(),
            is_extern: true,
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.funcs.push(LirFunc {
            name: "pair".to_owned(),
            ins: Vec::new(),
            outs: vec![
                ("a".to_owned(), "#bit32".to_owned()),
                ("b".to_owned(), "#bit32".to_owned()),
            ],
            is_extern: true,
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.funcs.push(LirFunc {
            name: "main".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: true,
            blocks: Vec::new(),
        });
        let text = emit(&module);
        assert!(text.contains("declare void @main()"), "{text}");
        assert!(!text.contains("define i32 @main()"), "{text}");
    }

    #[test]
    fn lowers_data_globals_in_order() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.data.push(LirData {
            name: "c0".to_owned(),
            kind: LirDataKind::Str("hi".to_owned()),
            span: span(),
        });
        module.data.push(LirData {
            name: "c1".to_owned(),
            kind: LirDataKind::NumLit("10.5".to_owned()),
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
            ty: ty.to_owned(),
            place: place.to_owned(),
            span: span(),
        });
    }

    fn copy_place(insts: &mut Vec<LirInst>, src: LirOperand, dst: &str) {
        insts.push(LirInst::CopyPlace {
            src,
            dst: dst.to_owned(),
            span: span(),
        });
    }

    fn arith(insts: &mut Vec<LirInst>, op: ArithOp, lhs: &str, rhs: &str, dst: &str, ty: &str) {
        insts.push(LirInst::Arith {
            op,
            lhs: lhs.to_owned(),
            rhs: rhs.to_owned(),
            dst: dst.to_owned(),
            ty: ty.to_owned(),
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
            name: name.to_owned(),
            ins: ins
                .iter()
                .map(|(place, ty)| ((*place).to_owned(), (*ty).to_owned()))
                .collect(),
            outs: outs
                .iter()
                .map(|(place, ty)| ((*place).to_owned(), (*ty).to_owned()))
                .collect(),
            is_extern: false,
            blocks,
        }
    }

    fn ret_block(label: &str, insts: Vec<LirInst>) -> LirBlock {
        LirBlock {
            label: label.to_owned(),
            insts,
            term: LirTerm::Return { span: span() },
            span: span(),
        }
    }

    fn call(insts: &mut Vec<LirInst>, func: LirCallee, ins: &[&str], outs: &[&str]) {
        insts.push(LirInst::Call {
            func,
            ins: ins.iter().map(|place| (*place).to_owned()).collect(),
            outs: outs.iter().map(|place| (*place).to_owned()).collect(),
            span: span(),
        });
    }

    /// A `main` calling all three leaf builtins, so every leaf must be emitted.
    fn all_leaves_module() -> LirModule {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.layouts.insert(
            "#Pair".to_owned(),
            vec![
                ("a".to_owned(), "#bit32".to_owned()),
                ("b".to_owned(), "#bit8".to_owned()),
            ],
        );
        module.data.push(LirData {
            name: "c0".to_owned(),
            kind: LirDataKind::Str("hi".to_owned()),
            span: span(),
        });

        let mut insts = Vec::new();
        for (place, ty) in [
            ("pair", "#Pair"),
            ("x", "#bit32"),
            ("x2", "#bit32"),
            ("b", "#bit8"),
            ("r", "ref #bit32"),
            ("bbslot", "#byte_buffer"),
            ("adslot", "ref #bit32"),
            ("afslot", "#bit64"),
        ] {
            alloca(&mut insts, place, ty);
        }

        insts.push(LirInst::Const {
            dst: "c".to_owned(),
            lit: "0x2a".to_owned(),
            ty: "#bit32".to_owned(),
            span: span(),
        });
        insts.push(LirInst::Const {
            dst: "dec".to_owned(),
            lit: "7".to_owned(),
            ty: "#bit8".to_owned(),
            span: span(),
        });
        insts.push(LirInst::Const {
            dst: "bb".to_owned(),
            lit: "!c0".to_owned(),
            ty: "#byte_buffer".to_owned(),
            span: span(),
        });
        insts.push(LirInst::Addr {
            place: "x".to_owned(),
            dst: "ad".to_owned(),
            ty: "ref #bit32".to_owned(),
            span: span(),
        });
        insts.push(LirInst::AddrFunc {
            func: "f".to_owned(),
            dst: "af".to_owned(),
            ty: "#bit64".to_owned(),
            span: span(),
        });

        copy_place(&mut insts, LirOperand::Reg("c".to_owned()), "x");
        copy_place(&mut insts, LirOperand::Place("x".to_owned()), "x2");
        copy_place(&mut insts, LirOperand::Reg("dec".to_owned()), "b");
        copy_place(&mut insts, LirOperand::Reg("bb".to_owned()), "bbslot");
        copy_place(&mut insts, LirOperand::Reg("ad".to_owned()), "adslot");
        copy_place(&mut insts, LirOperand::Reg("af".to_owned()), "afslot");
        insts.push(LirInst::CopyReg {
            src: LirOperand::Place("x".to_owned()),
            dst: "cr".to_owned(),
            ty: "#bit32".to_owned(),
            span: span(),
        });
        insts.push(LirInst::Load {
            src: "r".to_owned(),
            dst: "ld".to_owned(),
            ty: "ref #bit32".to_owned(),
            span: span(),
        });
        insts.push(LirInst::Field {
            obj: "pair".to_owned(),
            name: "a".to_owned(),
            idx: 0,
            dst: "fa".to_owned(),
            ty: "#bit32".to_owned(),
            span: span(),
        });
        insts.push(LirInst::FieldAddr {
            obj: "pair".to_owned(),
            name: "a".to_owned(),
            idx: 0,
            dst: "fad".to_owned(),
            ty: "ref #bit32".to_owned(),
            span: span(),
        });
        insts.push(LirInst::StoreField {
            value: LirOperand::Reg("c".to_owned()),
            obj: "pair".to_owned(),
            name: "a".to_owned(),
            idx: 0,
            span: span(),
        });
        insts.push(LirInst::Store {
            src: LirOperand::Reg("c".to_owned()),
            dst: "r".to_owned(),
            span: span(),
        });
        insts.push(LirInst::Convert {
            op: ConvertOp::Trunc,
            src: "cr".to_owned(),
            dst: "t".to_owned(),
            ty: "#bit8".to_owned(),
            span: span(),
        });
        insts.push(LirInst::Convert {
            op: ConvertOp::ExtendZero,
            src: "t".to_owned(),
            dst: "z".to_owned(),
            ty: "#bit64".to_owned(),
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
            arith(&mut insts, op, "cr", "cr", dst, "#bit32");
        }
        for (op, dst) in [(ArithOp::FLt, "fl"), (ArithOp::FGt, "fg")] {
            arith(&mut insts, op, "cr", "cr", dst, "#bit8");
        }

        module.funcs.push(LirFunc {
            name: "f".to_owned(),
            ins: vec![("p".to_owned(), "ref #bit32".to_owned())],
            outs: vec![("o".to_owned(), "#bit32".to_owned())],
            is_extern: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
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
            "i64 2",
            "store ptr %x, ptr %adslot",
            "getelementptr inbounds %Pair",
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
    /// out of the place before the GEP (the pointer ABI passes the address of
    /// the caller's reference slot).
    #[test]
    fn field_through_ref_loads_reference_first() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.layouts.insert(
            "#Pair".to_owned(),
            vec![
                ("a".to_owned(), "#bit32".to_owned()),
                ("b".to_owned(), "#bit8".to_owned()),
            ],
        );
        let insts = vec![
            LirInst::Field {
                obj: "rp".to_owned(),
                name: "a".to_owned(),
                idx: 0,
                dst: "v".to_owned(),
                ty: "#bit32".to_owned(),
                span: span(),
            },
            LirInst::CopyPlace {
                src: LirOperand::Reg("v".to_owned()),
                dst: "o".to_owned(),
                span: span(),
            },
        ];
        module.funcs.push(LirFunc {
            name: "g".to_owned(),
            ins: vec![("rp".to_owned(), "ref #Pair".to_owned())],
            outs: vec![("o".to_owned(), "#bit32".to_owned())],
            is_extern: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts,
                term: LirTerm::Return { span: span() },
                span: span(),
            }],
        });
        let text = emit(&module);
        assert!(text.contains("load ptr, ptr %in0"), "{text}");
        assert!(text.contains("getelementptr inbounds %Pair, ptr"), "{text}");
    }

    /// A direct user call passes the address of every listed place (ins then
    /// outs); the place ABI means the callee signature is all `ptr`.
    #[test]
    fn lowers_direct_call_with_place_pointers() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.funcs.push(func(
            "g",
            &[("a", "#bit32")],
            &[("o", "#bit32")],
            vec![ret_block("entry", Vec::new())],
        ));
        let mut insts = Vec::new();
        alloca(&mut insts, "a", "#bit32");
        alloca(&mut insts, "o", "#bit32");
        call(&mut insts, LirCallee::Func("g".to_owned()), &["a"], &["o"]);
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module
            .funcs
            .push(func("g", &[], &[], vec![ret_block("entry", Vec::new())]));
        let mut insts = Vec::new();
        call(&mut insts, LirCallee::Func("g".to_owned()), &[], &[]);
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        let mut insts = Vec::new();
        alloca(&mut insts, "x", "#bit32");
        alloca(&mut insts, "y", "#bit32");
        alloca(&mut insts, "fptr", "#bit64");
        call(
            &mut insts,
            LirCallee::Func("later".to_owned()),
            &["x"],
            &["y"],
        );
        insts.push(LirInst::AddrFunc {
            func: "later".to_owned(),
            dst: "1".to_owned(),
            ty: "#bit64".to_owned(),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("1".to_owned()), "fptr");
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
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
            func: "addOne".to_owned(),
            dst: "1".to_owned(),
            ty: "#bit64".to_owned(),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("1".to_owned()), "add");
        insts.push(LirInst::CopyReg {
            src: LirOperand::Place("add".to_owned()),
            dst: "3".to_owned(),
            ty: "#bit64".to_owned(),
            span: span(),
        });
        call(
            &mut insts,
            LirCallee::Reg("3".to_owned()),
            &["value"],
            &["out"],
        );
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
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

    /// `@ptr_add` is emitted as `.lilac.ptr_add(ptr, iNative) -> ptr` using
    /// safe `ptrtoint`/`add`/`inttoptr` integer arithmetic.
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
        assert!(body.contains("ptrtoint ptr"), "{body}");
        assert!(body.contains("inttoptr i64"), "{body}");
    }

    /// A module that calls no leaf builtin and has no `panic` emits none of the
    /// three leaf definitions and never declares `llvm.trap`.
    #[test]
    fn culls_all_unused_leaf_builtins() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
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

    /// The builtin aggregate LLVM types come from `builtin_layout`: a
    /// `{ iNative, ptr }` `#byte_buffer` and a 17-field `#format_args` (a count
    /// plus sixteen named pieces, no array).
    #[test]
    fn builtin_aggregates_follow_builtin_layout() {
        let text = emit(&type_module());
        assert!(text.contains("{ i64, ptr }"), "{text}");
        assert!(text.contains(format_args_llvm_type().as_str()), "{text}");
        assert!(
            !text.contains("[16 x { i64, ptr }]"),
            "`#format_args` must be sixteen named fields, not an array:\n{text}"
        );
    }

    /// The backend still declares and defines the `llvm.trap`-backed abort
    /// leaf when a module calls `@lilac_abort`; the moved
    /// `format_args`/`byte_buffer` runtime is gone.
    #[test]
    fn keeps_only_the_trap_backed_leaf_runtime() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        let mut entry = Vec::new();
        alloca(&mut entry, "c", "#bit8");
        entry.push(LirInst::CopyReg {
            src: LirOperand::Place("c".to_owned()),
            dst: "1".to_owned(),
            ty: "#bit8".to_owned(),
            span: span(),
        });
        let entry = LirBlock {
            label: "entry".to_owned(),
            insts: entry,
            term: LirTerm::BranchIf {
                cond: "1".to_owned(),
                then: "then".to_owned(),
                els: Some("els".to_owned()),
                span: span(),
            },
            span: span(),
        };
        let then = LirBlock {
            label: "then".to_owned(),
            insts: Vec::new(),
            term: LirTerm::Branch {
                target: "join".to_owned(),
                span: span(),
            },
            span: span(),
        };
        let els = LirBlock {
            label: "els".to_owned(),
            insts: Vec::new(),
            term: LirTerm::Branch {
                target: "join".to_owned(),
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        let mut entry = Vec::new();
        alloca(&mut entry, "c", "#bit8");
        entry.push(LirInst::CopyReg {
            src: LirOperand::Place("c".to_owned()),
            dst: "1".to_owned(),
            ty: "#bit8".to_owned(),
            span: span(),
        });
        let entry = LirBlock {
            label: "entry".to_owned(),
            insts: entry,
            term: LirTerm::BranchIf {
                cond: "1".to_owned(),
                then: "then".to_owned(),
                els: None,
                span: span(),
            },
            span: span(),
        };
        let then = LirBlock {
            label: "then".to_owned(),
            insts: Vec::new(),
            term: LirTerm::Branch {
                target: "join".to_owned(),
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

    /// `return` → `ret void`, `unreachable` → `unreachable`, `panic` → a
    /// `@.lilac.abort` call plus `unreachable`, and `drop` emits nothing.
    #[test]
    fn lowers_terminators_and_drop() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.data.push(LirData {
            name: "c0".to_owned(),
            kind: LirDataKind::Str("boom".to_owned()),
            span: span(),
        });
        module
            .funcs
            .push(func("ret", &[], &[], vec![ret_block("entry", Vec::new())]));
        let unreachable = LirBlock {
            label: "entry".to_owned(),
            insts: Vec::new(),
            term: LirTerm::Unreachable { span: span() },
            span: span(),
        };
        module.funcs.push(func("dead", &[], &[], vec![unreachable]));
        let panic = LirBlock {
            label: "entry".to_owned(),
            insts: Vec::new(),
            term: LirTerm::Panic {
                msg: "c0".to_owned(),
                span: span(),
            },
            span: span(),
        };
        module.funcs.push(func("boom", &[], &[], vec![panic]));
        let mut drop_insts = Vec::new();
        alloca(&mut drop_insts, "p", "#bit32");
        drop_insts.push(LirInst::Drop {
            place: "p".to_owned(),
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
        assert!(text.contains("define void @.lilac.abort()"), "{text}");
        assert!(text.contains("call void @.lilac.abort()"), "{text}");
        assert!(text.contains("unreachable"), "{text}");
        assert!(!text.contains("drop"), "drop must not lower:\n{text}");
    }

    /// The LIR verifier permits a `#format_args` data-symbol `const`, but a
    /// byte global cannot become a piece-table value; codegen raises an
    /// explicit ICE pinning the backend/verifier divergence.
    #[test]
    #[should_panic(expected = "no codegen lowering")]
    fn format_args_data_const_is_ice() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.data.push(LirData {
            name: "c0".to_owned(),
            kind: LirDataKind::Str("hi".to_owned()),
            span: span(),
        });
        module.funcs.push(LirFunc {
            name: "f".to_owned(),
            ins: Vec::new(),
            outs: Vec::new(),
            is_extern: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts: vec![LirInst::Const {
                    dst: "x".to_owned(),
                    lit: "!c0".to_owned(),
                    ty: "#format_args".to_owned(),
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
        let lir = LirModule::new("x86_64-pc-windows-msvc");
        let lower = crate::lower::Lower::new(&context, module, &lir, true);
        let _ = lower.lir_type("#bogus");
    }

    #[test]
    #[should_panic(expected = "data-only type")]
    fn data_only_type_is_ice() {
        let context = inkwell::context::Context::create();
        let module = context.create_module("ice");
        let lir = LirModule::new("x86_64-pc-windows-msvc");
        let lower = crate::lower::Lower::new(&context, module, &lir, true);
        let _ = lower.lir_type("#str");
    }

    #[test]
    fn lir_type_maps_fbit_types() {
        let context = inkwell::context::Context::create();
        let module = context.create_module("fbits");
        let lir = LirModule::new("x86_64-pc-windows-msvc");
        let lower = crate::lower::Lower::new(&context, module, &lir, true);
        for (ty, expected) in [
            ("#fbit16", "half"),
            ("#fbit32", "float"),
            ("#fbit64", "double"),
            ("#fbit128", "fp128"),
        ] {
            assert_eq!(
                lower.lir_type(ty).print_to_string().to_string(),
                expected,
                "{ty}"
            );
        }
    }

    #[test]
    fn lowers_fbit_bitcast() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        let insts = vec![
            LirInst::CopyReg {
                src: LirOperand::Place("x".to_owned()),
                dst: "r0".to_owned(),
                ty: "#bit32".to_owned(),
                span: span(),
            },
            LirInst::Convert {
                op: ConvertOp::Bitcast,
                src: "r0".to_owned(),
                dst: "r1".to_owned(),
                ty: "#fbit32".to_owned(),
                span: span(),
            },
            LirInst::CopyPlace {
                src: LirOperand::Reg("r1".to_owned()),
                dst: "out".to_owned(),
                span: span(),
            },
        ];
        module.funcs.push(LirFunc {
            name: "cast".to_owned(),
            ins: vec![("x".to_owned(), "#bit32".to_owned())],
            outs: vec![("out".to_owned(), "#fbit32".to_owned())],
            is_extern: false,
            blocks: vec![LirBlock {
                label: "entry".to_owned(),
                insts,
                term: LirTerm::Return { span: span() },
                span: span(),
            }],
        });
        let text = emit(&module);
        assert!(text.contains("bitcast i32"), "{text}");
        assert!(text.contains("to float"), "{text}");
    }

    #[test]
    fn default_emit_llvm_is_unsupported() {
        struct Noop;
        impl lilac_codegen::Backend for Noop {
            fn name(&self) -> &'static str {
                "noop"
            }
            fn codegen(&self, _m: &lilac_ir::LirModule) -> Result<(), String> {
                Ok(())
            }
        }
        let m = lilac_ir::LirModule::new("x86_64-pc-windows-msvc");
        let err = Noop.emit_llvm(&m).expect_err("noop cannot emit LLVM IR");
        assert!(err.contains("noop"), "error must name the backend: {err}");
    }

    /// Every target is initialized, not just the host: emitting for a triple
    /// that is not the build host's architecture must succeed. `aarch64-apple-
    /// darwin` is deliberately neither `x86_64` nor Windows.
    #[test]
    fn emits_for_non_native_triple() {
        let backend = lilac_codegen_llvm::LlvmBackend::new("aarch64-apple-darwin", OptLevel::O0);
        let module = lilac_ir::LirModule::new("aarch64-apple-darwin");
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
        let module = lilac_ir::LirModule::new("x86_64-pc-windows-msvc");
        let text = backend.emit_llvm(&module).expect("emits");
        assert!(
            text.contains("target triple = \"x86_64-pc-windows-msvc\""),
            "{text}"
        );
    }

    fn backend_with(optimize: OptLevel) -> lilac_codegen_llvm::LlvmBackend {
        lilac_codegen_llvm::LlvmBackend::new("x86_64-pc-windows-msvc", optimize)
    }

    /// A LIR `main` is emitted as `.lilac.main` and the module gains an `i32
    /// @main()` wrapper that calls it and returns 0.
    #[test]
    fn emits_main_entry_wrapper() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", Vec::new())]));
        let text = emit(&module);
        assert!(text.contains("define void @.lilac.main()"), "{text}");
        assert!(text.contains("define i32 @main()"), "{text}");
        assert!(text.contains("call void @.lilac.main()"), "{text}");
        assert!(text.contains("ret i32 0"), "{text}");
    }

    /// A `main` with an `in` place still gets a valid C entry wrapper: the
    /// wrapper allocates a zeroed slot and passes its address (the pointer
    /// ABI), so the call's argument count matches `.lilac.main`'s signature.
    #[test]
    fn emits_main_wrapper_with_parameter_slots() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.funcs.push(func(
            "main",
            &[("n", "#bit32")],
            &[],
            vec![ret_block("entry", Vec::new())],
        ));
        let text = emit(&module);
        assert!(
            text.contains("define void @.lilac.main(ptr %in0)"),
            "{text}"
        );
        assert!(text.contains("store i32 0, ptr %n"), "{text}");
        assert!(text.contains("call void @.lilac.main(ptr %n)"), "{text}");
    }

    /// A value place passed to a `ref` parameter is spilled into a pointer
    /// slot: the callee expects the address of a reference slot (and loads the
    /// pointer out of it), not the value's own address.
    #[test]
    fn coerces_value_argument_to_ref_parameter() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        module.layouts.insert(
            "#Pair".to_owned(),
            vec![("a".to_owned(), "#bit32".to_owned())],
        );
        module.funcs.push(func(
            "hook",
            &[("from", "ref #Pair")],
            &[],
            vec![ret_block("entry", Vec::new())],
        ));
        let mut insts = Vec::new();
        alloca(&mut insts, "p", "#Pair");
        call(&mut insts, LirCallee::Func("hook".to_owned()), &["p"], &[]);
        module
            .funcs
            .push(func("main", &[], &[], vec![ret_block("entry", insts)]));
        let text = emit(&module);
        assert!(text.contains("define void @hook(ptr %in0)"), "{text}");
        assert!(text.contains("store ptr %p, ptr %ref.arg"), "{text}");
        assert!(text.contains("call void @hook(ptr %ref.arg)"), "{text}");
    }

    /// A module with no `main` gets no `.lilac.main` and no entry wrapper, yet
    /// still verifies (Empty module has only the runtime).
    #[test]
    fn omits_entry_wrapper_without_main() {
        let text = emit(&LirModule::new("x86_64-pc-windows-msvc"));
        assert!(!text.contains(".lilac.main"), "{text}");
        assert!(!text.contains("define i32 @main"), "{text}");
    }

    /// A `call` and an `addr.func` targeting LIR `main` are rewritten to the
    /// renamed `.lilac.main` symbol.
    #[test]
    fn renames_calls_and_addr_func_to_main() {
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        let mut insts = Vec::new();
        alloca(&mut insts, "fptr", "#bit64");
        call(&mut insts, LirCallee::Func("main".to_owned()), &[], &[]);
        insts.push(LirInst::AddrFunc {
            func: "main".to_owned(),
            dst: "1".to_owned(),
            ty: "#bit64".to_owned(),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("1".to_owned()), "fptr");
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
        let mut module = LirModule::new("x86_64-pc-windows-msvc");
        let mut insts = Vec::new();
        alloca(&mut insts, "a", "#bit32");
        insts.push(LirInst::Const {
            dst: "one".to_owned(),
            lit: "1".to_owned(),
            ty: "#bit32".to_owned(),
            span: span(),
        });
        copy_place(&mut insts, LirOperand::Reg("one".to_owned()), "a");
        insts.push(LirInst::CopyReg {
            src: LirOperand::Place("a".to_owned()),
            dst: "ra".to_owned(),
            ty: "#bit32".to_owned(),
            span: span(),
        });
        insts.push(LirInst::CopyReg {
            src: LirOperand::Place("a".to_owned()),
            dst: "rb".to_owned(),
            ty: "#bit32".to_owned(),
            span: span(),
        });
        arith(&mut insts, ArithOp::IAdd, "ra", "rb", "sum", "#bit32");
        arith(&mut insts, ArithOp::IMul, "sum", "ra", "prod", "#bit32");
        copy_place(&mut insts, LirOperand::Reg("prod".to_owned()), "a");
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
