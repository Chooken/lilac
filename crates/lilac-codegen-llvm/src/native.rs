//! What a native object (no C runtime) defines for itself.
//!
//! LLVM lowers `llvm.memcpy`, `llvm.memmove`, `llvm.memset` and large copies
//! to calls to the C library's `memcpy`, `memmove` and `memset`, and
//! aggregate `==` calls `memcmp`. On Windows
//! it also references `_fltused` from any function that uses floats, and
//! calls `__chkstk` to probe stack frames larger than a page. With no C
//! runtime linked, the object defines all of them itself, as Rust's
//! `compiler-builtins` does.
//!
//! On Linux it also defines the process entry `.lilac.native_entry` in
//! assembly: only assembly can read the arguments the kernel leaves on the
//! stack. On Windows, a module with thread-locals gets the TLS directory a C
//! runtime would define.
//!
//! macOS needs none of the above: its system interface is `libSystem`, which a
//! native program links anyway, and which already defines the memory
//! functions.
//!
//! On x86-64 (macOS too) it also defines the half-precision conversions
//! LLVM calls (`float::HALF`).

use inkwell::context::Context;
use inkwell::module::{Linkage, Module};
use inkwell::types::IntType;

use crate::float::HALF;
use crate::threads::{append_assembly, append_ir, has_thread_locals, windows_tls_directory};

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
    // x86-64 converts half-precision floats by calling compiler-rt's
    // routines, which no native program links (`libSystem` exports them,
    // but its stub lists only what `Standard` declares).
    if triple.starts_with("x86_64") {
        append_ir(module, "half", HALF);
    }
    if is_apple(triple) {
        return;
    }
    let width = format!("i{}", native.get_bit_width());
    append_ir(module, "memory", &MEMORY.replace("INATIVE", &width));
    if triple.contains("windows") {
        let fltused = module.add_global(context.i32_type(), None, "_fltused");
        fltused.set_initializer(&context.i32_type().const_zero());
        fltused.set_linkage(Linkage::External);
        if triple.starts_with("x86_64") {
            append_assembly(module, "__chkstk", CHKSTK_X86_64);
        } else if triple.starts_with("aarch64") {
            append_assembly(module, "__chkstk", CHKSTK_AARCH64);
        }
        if has_thread_locals(module) {
            windows_tls_directory(module);
        }
    }
    // Linux enters a process with its arguments on the stack, which only
    // assembly can read: the entry passes them to `.lilac.native_main`.
    if triple.contains("linux") && module.get_function(".lilac.native_main").is_some() {
        if triple.starts_with("x86_64") {
            append_assembly(module, ".lilac.native_entry", ENTRY_X86_64);
        } else if triple.starts_with("aarch64") {
            append_assembly(module, ".lilac.native_entry", ENTRY_AARCH64);
        }
    }
}

/// Whether `triple` is an Apple target (`-apple-darwin`, `-apple-macos…`).
fn is_apple(triple: &str) -> bool {
    triple.contains("-apple-")
}

/// `memcpy`, `memmove`, `memset` and `memcmp`, as C declares them, each a
/// byte loop. They are marked `no-builtins` so the optimizer cannot turn a
/// loop back into a call to itself, and address bytes with
/// `getelementptr` (inkwell's GEP builders are `unsafe`, so they are IR
/// text). `INATIVE` stands for the pointer-sized integer.
const MEMORY: &str = r#"
define ptr @memcpy(ptr %dest, ptr %src, INATIVE %len) #0 {
entry:
  br label %check
check:
  %index = phi INATIVE [ 0, %entry ], [ %next, %step ]
  %more = icmp ult INATIVE %index, %len
  br i1 %more, label %step, label %done
step:
  %from = getelementptr inbounds i8, ptr %src, INATIVE %index
  %byte = load i8, ptr %from, align 1
  %to = getelementptr inbounds i8, ptr %dest, INATIVE %index
  store i8 %byte, ptr %to, align 1
  %next = add INATIVE %index, 1
  br label %check
done:
  ret ptr %dest
}

define ptr @memmove(ptr %dest, ptr %src, INATIVE %len) #0 {
entry:
  %below = icmp ult ptr %dest, %src
  br i1 %below, label %forward, label %backward
forward:
  %index = phi INATIVE [ 0, %entry ], [ %next, %forward.step ]
  %more = icmp ult INATIVE %index, %len
  br i1 %more, label %forward.step, label %done
forward.step:
  %from = getelementptr inbounds i8, ptr %src, INATIVE %index
  %byte = load i8, ptr %from, align 1
  %to = getelementptr inbounds i8, ptr %dest, INATIVE %index
  store i8 %byte, ptr %to, align 1
  %next = add INATIVE %index, 1
  br label %forward
backward:
  %left = phi INATIVE [ %len, %entry ], [ %last, %backward.step ]
  %any = icmp ne INATIVE %left, 0
  br i1 %any, label %backward.step, label %done
backward.step:
  %last = sub INATIVE %left, 1
  %back.from = getelementptr inbounds i8, ptr %src, INATIVE %last
  %back.byte = load i8, ptr %back.from, align 1
  %back.to = getelementptr inbounds i8, ptr %dest, INATIVE %last
  store i8 %back.byte, ptr %back.to, align 1
  br label %backward
done:
  ret ptr %dest
}

define ptr @memset(ptr %dest, i32 %value, INATIVE %len) #0 {
entry:
  %byte = trunc i32 %value to i8
  br label %check
check:
  %index = phi INATIVE [ 0, %entry ], [ %next, %step ]
  %more = icmp ult INATIVE %index, %len
  br i1 %more, label %step, label %done
step:
  %to = getelementptr inbounds i8, ptr %dest, INATIVE %index
  store i8 %byte, ptr %to, align 1
  %next = add INATIVE %index, 1
  br label %check
done:
  ret ptr %dest
}

define i32 @memcmp(ptr %left, ptr %right, INATIVE %len) #0 {
entry:
  br label %check
check:
  %index = phi INATIVE [ 0, %entry ], [ %next, %same ]
  %more = icmp ult INATIVE %index, %len
  br i1 %more, label %step, label %equal
step:
  %left.at = getelementptr inbounds i8, ptr %left, INATIVE %index
  %left.byte = load i8, ptr %left.at, align 1
  %right.at = getelementptr inbounds i8, ptr %right, INATIVE %index
  %right.byte = load i8, ptr %right.at, align 1
  %match = icmp eq i8 %left.byte, %right.byte
  br i1 %match, label %same, label %differ
same:
  %next = add INATIVE %index, 1
  br label %check
differ:
  %left.wide = zext i8 %left.byte to i32
  %right.wide = zext i8 %right.byte to i32
  %order = sub i32 %left.wide, %right.wide
  ret i32 %order
equal:
  ret i32 0
}

attributes #0 = { nounwind "no-builtins" }
"#;
