//! LLVM backend behind [`Backend`].
//!
//! Type/layout/data/place lowering lives in the `lower` module; instruction
//! bodies and control flow land in later M5b tasks. This crate is the ONLY one
//! allowed to link LLVM (enforced by CI grep gate).

use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::targets::{
    CodeModel, InitializationConfig, RelocMode, Target, TargetMachine, TargetTriple,
};
use inkwell::OptimizationLevel;

use lilac_codegen::{Backend, OptLevel};
use lilac_ir::LirModule;

mod lower;

/// Whether `triple`'s architecture is 64-bit (drives `@bitNative`).
fn is_64_bit_triple(triple: &str) -> bool {
    triple
        .split('-')
        .next()
        .is_some_and(|arch| arch.contains("64"))
}

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

    /// Maps the codegen [`OptLevel`] onto inkwell's optimization level.
    fn optimization_level(&self) -> OptimizationLevel {
        match self.optimize {
            OptLevel::O0 => OptimizationLevel::None,
            OptLevel::O1 => OptimizationLevel::Less,
            OptLevel::O2 | OptLevel::Os => OptimizationLevel::Default,
            OptLevel::O3 => OptimizationLevel::Aggressive,
        }
    }

    /// Builds an empty module carrying the target triple and data layout.
    /// Lowering populates it; [`Backend::emit_llvm`] verifies the result.
    fn prepare_module<'ctx>(&self, context: &'ctx Context) -> Result<Module<'ctx>, String> {
        let module = context.create_module("lilac");
        let triple = TargetTriple::create(&self.triple);
        module.set_triple(&triple);

        Target::initialize_native(&InitializationConfig::default())?;
        let target = Target::from_triple(&triple)
            .map_err(|e| format!("unknown target triple `{}`: {e}", self.triple))?;
        let machine: TargetMachine = target
            .create_target_machine(
                &triple,
                "generic",
                "",
                self.optimization_level(),
                RelocMode::Default,
                CodeModel::Default,
            )
            .ok_or_else(|| format!("failed to create target machine for `{}`", self.triple))?;
        module.set_data_layout(&machine.get_target_data().get_data_layout());

        Ok(module)
    }
}

impl Backend for LlvmBackend {
    fn name(&self) -> &'static str {
        "llvm"
    }

    fn codegen(&self, _module: &LirModule) -> Result<(), String> {
        Err("LLVM codegen lands in M5; M0 stops at `emit-ir`".to_owned())
    }

    fn emit_llvm(&self, lir: &LirModule) -> Result<String, String> {
        let context = Context::create();
        let module = self.prepare_module(&context)?;
        let mut lower = lower::Lower::new(&context, module, lir, is_64_bit_triple(&self.triple));
        lower.run();
        let module = lower.into_module();
        module
            .verify()
            .map_err(|e| format!("module verification failed: {e}"))?;
        Ok(module.print_to_string().to_string())
    }
}

#[cfg(test)]
mod tests {
    use crate as lilac_codegen_llvm;
    use lilac_codegen::{Backend, OptLevel};
    use lilac_ir::{
        ArithOp, ConvertOp, LirBlock, LirData, LirDataKind, LirFunc, LirInst, LirModule,
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
            ("pn", "#bitNative"),
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
                ("e".to_owned(), "#bitNative".to_owned()),
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

    #[test]
    fn lowers_types_layouts_and_zero_init() {
        let text = emit(&type_module());
        for needle in [
            "i8",
            "i16",
            "i32",
            "i64",
            "ptr",
            "%Pair = type { i32, i8 }",
            "{ i64, ptr }",
            "{ i64, [16 x { i64, ptr }] }",
        ] {
            assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
        }
        // Scalar zero values print as `0`/`null`; aggregate ones as
        // `zeroinitializer` (LLVM canonicalizes both to the same constant).
        for needle in [
            "store i8 0",
            "store i16 0",
            "store i32 0",
            "store i64 0",
            "store ptr null",
            "store %Pair zeroinitializer",
            "store { i64, ptr } zeroinitializer",
            "store { i64, [16 x { i64, ptr }] } zeroinitializer",
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
            (ArithOp::IMod, "ir"),
        ] {
            arith(&mut insts, op, "cr", "cr", dst, "#bit32");
        }
        for (op, dst) in [
            (ArithOp::Eq, "eq"),
            (ArithOp::ILt, "lt"),
            (ArithOp::IGt, "gt"),
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
            "srem i32",
            "icmp eq i32",
            "icmp slt i32",
            "icmp sgt i32",
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
        assert!(Noop.emit_llvm(&m).is_err());
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
}
