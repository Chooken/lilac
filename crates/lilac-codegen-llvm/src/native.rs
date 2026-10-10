//! What a native object (no C runtime) defines for itself.
//!
//! LLVM lowers `llvm.memcpy`, `llvm.memmove`, `llvm.memset` and large copies
//! to calls to the C library's `memcpy`, `memmove` and `memset`. On Windows
//! it also references `_fltused` from any function that uses floats, and
//! calls `__chkstk` to probe stack frames larger than a page. With no C
//! runtime linked, the object defines all of them itself, as Rust's
//! `compiler-builtins` does.
//!
//! On Linux it also defines the process entry `.lilac.native_entry` in
//! assembly: only assembly can read the arguments the kernel leaves on the
//! stack.
//!
//! macOS needs none of this: its system interface is `libSystem`, which a
//! native program links anyway, and which already defines the memory
//! functions.

use inkwell::attributes::AttributeLoc;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::{BasicMetadataTypeEnum, IntType, PointerType};
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

use lilac_error::bug;
use lilac_span::Span;

use crate::lower::checked;

/// `__chkstk` for x86-64 Windows: probes each page from the stack pointer
/// down to `rsp - rax`, preserving every register. From compiler-rt's
/// `chkstk.S`.
const CHKSTK_X86_64: &str = "\
.text
.globl __chkstk
.p2align 4
__chkstk:
    pushq %rcx
    pushq %rax
    cmpq $0x1000, %rax
    leaq 24(%rsp), %rcx
    jb 1f
2:
    subq $0x1000, %rcx
    testq %rcx, (%rcx)
    subq $0x1000, %rax
    cmpq $0x1000, %rax
    ja 2b
1:
    subq %rax, %rcx
    testq %rcx, (%rcx)
    popq %rax
    popq %rcx
    retq
";

/// `__chkstk` for 64-bit Arm Windows: `x15` holds the frame size in 16-byte
/// units; only `x16` and `x17` may be clobbered. From compiler-rt's
/// `chkstk.S`.
const CHKSTK_AARCH64: &str = "\
.text
.globl __chkstk
.p2align 2
__chkstk:
    lsl x16, x15, #4
    mov x17, sp
1:
    sub x17, x17, #4096
    subs x16, x16, #4096
    ldr xzr, [x17]
    b.gt 1b
    ret
";

/// The Linux x86-64 process entry: the kernel leaves `argc`, then the
/// `argv` pointers and a null, then the `envp` pointers on the stack, with
/// no return address. It passes them to `.lilac.native_main` (the C-ABI
/// wrapper of `.lilac.start`) after aligning the stack for a call, and
/// clears the frame pointer so a stack walk ends here.
const ENTRY_X86_64: &str = r#"
.text
.globl ".lilac.native_entry"
.type ".lilac.native_entry",@function
.p2align 4
".lilac.native_entry":
    xorl %ebp, %ebp
    movq (%rsp), %rdi
    leaq 8(%rsp), %rsi
    leaq 16(%rsp,%rdi,8), %rdx
    andq $-16, %rsp
    callq ".lilac.native_main"
    ud2
"#;

/// The Linux 64-bit Arm process entry, as [`ENTRY_X86_64`] (the stack
/// pointer is already aligned).
const ENTRY_AARCH64: &str = r#"
.text
.globl ".lilac.native_entry"
.type ".lilac.native_entry",%function
.p2align 2
".lilac.native_entry":
    mov x29, #0
    mov x30, #0
    ldr x0, [sp]
    add x1, sp, #8
    add x2, x1, x0, lsl #3
    add x2, x2, #8
    bl ".lilac.native_main"
    brk #0
"#;

/// Defines the native support symbols in `module` for `triple`.
pub(crate) fn define<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    triple: &str,
    native: IntType<'ctx>,
) {
    if is_apple(triple) {
        return;
    }
    let support = Support {
        context,
        module,
        native,
    };
    support.memcpy();
    support.memmove();
    support.memset();
    if triple.contains("windows") {
        let fltused = module.add_global(context.i32_type(), None, "_fltused");
        fltused.set_initializer(&context.i32_type().const_zero());
        fltused.set_linkage(Linkage::External);
        if triple.starts_with("x86_64") {
            module.set_inline_assembly(CHKSTK_X86_64);
        } else if triple.starts_with("aarch64") {
            module.set_inline_assembly(CHKSTK_AARCH64);
        }
    }
    // Linux enters a process with its arguments on the stack, which only
    // assembly can read: the entry passes them to `.lilac.native_main`.
    if triple.contains("linux") && module.get_function(".lilac.native_main").is_some() {
        if triple.starts_with("x86_64") {
            module.set_inline_assembly(ENTRY_X86_64);
        } else if triple.starts_with("aarch64") {
            module.set_inline_assembly(ENTRY_AARCH64);
        }
    }
}

/// Whether `triple` is an Apple target (`-apple-darwin`, `-apple-macos…`).
fn is_apple(triple: &str) -> bool {
    triple.contains("-apple-")
}

struct Support<'ctx, 'm> {
    context: &'ctx Context,
    module: &'m Module<'ctx>,
    native: IntType<'ctx>,
}

impl<'ctx> Support<'ctx, '_> {
    fn ptr(&self) -> PointerType<'ctx> {
        self.context.ptr_type(AddressSpace::default())
    }

    /// Declares `name(ptr, <second>, iNative) -> ptr` with an `entry`
    /// block, marked `no-builtins` so the optimizer cannot turn its loop
    /// back into a call to itself. Returns the function, its three
    /// parameters, and a builder at the entry.
    fn function(
        &self,
        name: &str,
        second: BasicMetadataTypeEnum<'ctx>,
    ) -> (
        FunctionValue<'ctx>,
        [BasicValueEnum<'ctx>; 3],
        Builder<'ctx>,
    ) {
        let fn_type = self
            .ptr()
            .fn_type(&[self.ptr().into(), second, self.native.into()], false);
        let function = self.module.add_function(name, fn_type, None);
        let attribute = self.context.create_string_attribute("no-builtins", "");
        function.add_attribute(AttributeLoc::Function, attribute);
        let params = match function.get_params().as_slice() {
            [a, b, c] => [*a, *b, *c],
            _ => bug!(Span::dummy(), "`{name}` must take three parameters"),
        };
        let builder = self.context.create_builder();
        let block = self.context.append_basic_block(function, "entry");
        builder.position_at_end(block);
        (function, params, builder)
    }

    /// The byte address `base + index`, by integer arithmetic (as
    /// `lower::byte_ptr` does) rather than a GEP.
    fn byte_at(
        &self,
        builder: &Builder<'ctx>,
        base: PointerValue<'ctx>,
        index: IntValue<'ctx>,
    ) -> PointerValue<'ctx> {
        let base = checked(
            builder.build_ptr_to_int(base, self.native, ""),
            "base address",
        );
        let address = checked(builder.build_int_add(base, index, ""), "byte address");
        checked(
            builder.build_int_to_ptr(address, self.ptr(), ""),
            "byte pointer",
        )
    }

    /// Emits a loop running `body(index)` for each index below `len`,
    /// counting up (or down when `down`), and leaves the builder after it.
    fn byte_loop(
        &self,
        function: FunctionValue<'ctx>,
        builder: &Builder<'ctx>,
        len: IntValue<'ctx>,
        down: bool,
        body: &dyn Fn(&Builder<'ctx>, IntValue<'ctx>),
    ) {
        let Some(start) = builder.get_insert_block() else {
            bug!(Span::dummy(), "native support builder is not positioned");
        };
        let check = self.context.append_basic_block(function, "check");
        let step = self.context.append_basic_block(function, "step");
        let done = self.context.append_basic_block(function, "done");
        checked(builder.build_unconditional_branch(check), "loop entry");

        builder.position_at_end(check);
        let remaining = checked(builder.build_phi(self.native, "remaining"), "loop phi");
        let left = remaining.as_basic_value().into_int_value();
        let more = checked(
            builder.build_int_compare(IntPredicate::NE, left, self.native.const_zero(), ""),
            "loop test",
        );
        checked(
            builder.build_conditional_branch(more, step, done),
            "loop branch",
        );

        builder.position_at_end(step);
        let one = self.native.const_int(1, false);
        let next = checked(builder.build_int_sub(left, one, ""), "loop decrement");
        let index = if down {
            next
        } else {
            checked(builder.build_int_sub(len, left, ""), "loop index")
        };
        body(builder, index);
        checked(builder.build_unconditional_branch(check), "loop back");
        remaining.add_incoming(&[(&len, start), (&next, step)]);

        builder.position_at_end(done);
    }

    fn copy_loop(
        &self,
        function: FunctionValue<'ctx>,
        builder: &Builder<'ctx>,
        params: [BasicValueEnum<'ctx>; 3],
        down: bool,
    ) {
        let [dest, src, len] = params;
        let (dest, src) = (dest.into_pointer_value(), src.into_pointer_value());
        self.byte_loop(
            function,
            builder,
            len.into_int_value(),
            down,
            &|builder, index| {
                let from = self.byte_at(builder, src, index);
                let byte = checked(
                    builder.build_load(self.context.i8_type(), from, ""),
                    "load byte",
                );
                let to = self.byte_at(builder, dest, index);
                checked(builder.build_store(to, byte), "store byte");
            },
        );
    }

    /// `memcpy(dest, src, len) -> dest`, copying forwards.
    fn memcpy(&self) {
        let (function, params, builder) = self.function("memcpy", self.ptr().into());
        self.copy_loop(function, &builder, params, false);
        checked(builder.build_return(Some(&params[0])), "memcpy return");
    }

    /// `memmove(dest, src, len) -> dest`: forwards when `dest < src`, else
    /// backwards, so overlapping ranges copy correctly.
    fn memmove(&self) {
        let (function, params, builder) = self.function("memmove", self.ptr().into());
        let forward = self.context.append_basic_block(function, "forward");
        let backward = self.context.append_basic_block(function, "backward");
        let dest = checked(
            builder.build_ptr_to_int(params[0].into_pointer_value(), self.native, ""),
            "dest address",
        );
        let src = checked(
            builder.build_ptr_to_int(params[1].into_pointer_value(), self.native, ""),
            "src address",
        );
        let below = checked(
            builder.build_int_compare(IntPredicate::ULT, dest, src, ""),
            "copy direction",
        );
        checked(
            builder.build_conditional_branch(below, forward, backward),
            "direction branch",
        );
        for (block, down) in [(forward, false), (backward, true)] {
            builder.position_at_end(block);
            self.copy_loop(function, &builder, params, down);
            checked(builder.build_return(Some(&params[0])), "memmove return");
        }
    }

    /// `memset(dest, value, len) -> dest`, storing the low byte of `value`.
    fn memset(&self) {
        let (function, params, builder) = self.function("memset", self.context.i32_type().into());
        let [dest, value, len] = params;
        let dest = dest.into_pointer_value();
        let byte = checked(
            builder.build_int_truncate(value.into_int_value(), self.context.i8_type(), ""),
            "memset byte",
        );
        self.byte_loop(
            function,
            &builder,
            len.into_int_value(),
            false,
            &|builder, index| {
                let to = self.byte_at(builder, dest, index);
                checked(builder.build_store(to, byte), "store byte");
            },
        );
        checked(builder.build_return(Some(&params[0])), "memset return");
    }
}
