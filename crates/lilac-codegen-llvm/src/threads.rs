//! Threads: the entry every new thread runs, Linux's raw `clone`, and the
//! thread-local storage a native program sets up for itself.
//!
//! `.lilac.thread_entry(ptr start) -> ptr` is a C-ABI function an OS thread
//! starts in (`CreateThread`, `pthread_create`, or the child of `clone`).
//! `start` points at a `{function, argument}` pair; it calls `function`
//! with the place ABI, passing `argument`, and returns null. `@thread_entry`
//! is its address.
//!
//! `@linux_thread_clone` (Linux x86-64 and aarch64) makes a thread with the
//! `clone` system call. The child needs its own thread-local block, which
//! `.lilac.tls_init` lays out in a caller-given zeroed area from the
//! executable's `PT_TLS` program header (found through `__ehdr_start`): x86-64
//! uses variant II (the block ends at the thread pointer, which points at
//! itself), aarch64 variant I (a 16-byte header, then the block). The
//! system call itself is assembly, since the child starts on a fresh stack.
//!
//! With no C library, nothing sets up the main thread's block either: a
//! native Linux program with thread-locals calls `.lilac.main_tls` before
//! `.lilac.start`, which builds one in a static area unless the thread
//! pointer is already set (a dynamic loader set it). Native Windows defines
//! the TLS directory a C runtime would ([`windows_tls_directory`]).
//!
//! These are written as LLVM IR text and linked into the module, which also
//! appends their module assembly rather than replacing what is there.

use inkwell::memory_buffer::MemoryBuffer;
use inkwell::module::Module;

use lilac_error::bug;
use lilac_span::Span;
use lilac_target::Arch;

use crate::lower::{checked, Lower};
use crate::runtime::{byte_ptr, entry, param};

/// Bytes of the static area the main thread's TLS block is built in.
const MAIN_TLS_BYTES: u64 = 4096;

/// Links LLVM IR `text` into `module`, giving it the module's triple and data
/// layout first. The text is the compiler's own, so a parse or link failure
/// is an internal error.
pub(crate) fn append_ir(module: &Module<'_>, name: &str, text: &str) {
    let buffer = MemoryBuffer::create_from_memory_range_copy(text.as_bytes(), name);
    let extra = match module.get_context().create_module_from_ir(buffer) {
        Ok(extra) => extra,
        Err(error) => bug!(Span::dummy(), "`{name}` does not parse: {error}"),
    };
    extra.set_triple(&module.get_triple());
    extra.set_data_layout(&module.get_data_layout());
    if let Err(error) = module.link_in_module(extra) {
        bug!(Span::dummy(), "`{name}` does not link: {error}");
    }
}

/// Appends `assembly` to `module`'s module-level assembly, keeping what is
/// already there (`Module::set_inline_assembly` replaces it).
pub(crate) fn append_assembly(module: &Module<'_>, name: &str, assembly: &str) {
    let mut text = String::new();
    for line in assembly.lines() {
        text.push_str("module asm \"");
        for ch in line.chars() {
            match ch {
                '"' => text.push_str("\\22"),
                '\\' => text.push_str("\\5C"),
                other => text.push(other),
            }
        }
        text.push_str("\"\n");
    }
    append_ir(module, name, &text);
}

/// `.lilac.thread_entry(ptr start) -> ptr`: calls `start`'s function with its
/// argument, then returns null.
pub(crate) fn thread_entry(lower: &Lower<'_, '_>) {
    if lower.module().get_function(".lilac.thread_entry").is_some() {
        return;
    }
    let ptr = lower.ptr_type();
    let function = lower.runtime_fn(".lilac.thread_entry", ptr.fn_type(&[ptr.into()], false));
    let builder = entry(lower, function);
    let start = param(function, 0, "thread_entry").into_pointer_value();
    let callee = checked(builder.build_load(ptr, start, "function"), "load function");
    let word = lower
        .native_int()
        .const_int(u64::from(lower.native_int().get_bit_width() / 8), false);
    let at = byte_ptr(lower, &builder, start, word);
    let argument = checked(builder.build_load(ptr, at, "argument"), "load argument");
    let slot = checked(builder.build_alloca(ptr, "slot"), "argument slot");
    checked(builder.build_store(slot, argument), "store argument");
    let fn_type = lower.context().void_type().fn_type(&[ptr.into()], false);
    checked(
        builder.build_indirect_call(fn_type, callee.into_pointer_value(), &[slot.into()], ""),
        "call thread function",
    );
    checked(
        builder.build_return(Some(&ptr.const_null())),
        "return thread entry",
    );
}

/// `@thread_entry() -> ref unknown`: the address of `.lilac.thread_entry`.
pub(crate) fn thread_entry_address(lower: &Lower<'_, '_>) {
    thread_entry(lower);
    let Some(target) = lower.module().get_function(".lilac.thread_entry") else {
        bug!(Span::dummy(), "`.lilac.thread_entry` was not defined");
    };
    let ptr = lower.ptr_type();
    let function = lower.runtime_fn(".lilac.thread_entry.address", ptr.fn_type(&[], false));
    let builder = entry(lower, function);
    checked(
        builder.build_return(Some(&target.as_global_value().as_pointer_value())),
        "return thread entry address",
    );
}

/// `@linux_thread_clone(flags, stack, parent_tid, child_tid, tls_area, tls_size,
/// start) -> iNative`: lays out the child's TLS block in `tls_area`, then
/// `clone`s a thread on `stack` (its top) that runs `.lilac.thread_entry(start)`
/// and exits. Returns the child's id, a negated errno, or `-ENOMEM` when the
/// area is too small.
pub(crate) fn thread_clone(lower: &Lower<'_, '_>) {
    thread_entry(lower);
    tls_init(lower.module(), lower.arch());
    append_ir(lower.module(), ".lilac.linux_thread_clone", THREAD_CLONE);
    let assembly = match lower.arch() {
        Some(Arch::X86_64) => CLONE_X86_64,
        Some(Arch::Aarch64) => CLONE_AARCH64,
        other => bug!(
            Span::dummy(),
            "`@linux_thread_clone` has no `clone` for {other:?}"
        ),
    };
    append_assembly(lower.module(), ".lilac.clone_raw", assembly);
}

/// Defines `.lilac.main_tls()`, which gives the main thread a TLS block
/// unless it has one, and returns it.
pub(crate) fn main_tls<'ctx>(
    module: &Module<'ctx>,
    arch: Option<Arch>,
) -> inkwell::values::FunctionValue<'ctx> {
    tls_init(module, arch);
    let (read, write) = match arch {
        Some(Arch::X86_64) => (
            // `arch_prctl(ARCH_GET_FS, &slot)`.
            "%slot = alloca i64\n  store i64 0, ptr %slot\n  \
             %got = call i64 asm sideeffect \"syscall\", \"={rax},{rax},{rdi},{rsi},~{rcx},~{r11},~{memory}\"(i64 158, i64 4099, ptr %slot)\n  \
             %current = load i64, ptr %slot",
            // `arch_prctl(ARCH_SET_FS, tp)`.
            "%address = ptrtoint ptr %tp to i64\n  \
             %set = call i64 asm sideeffect \"syscall\", \"={rax},{rax},{rdi},{rsi},~{rcx},~{r11},~{memory}\"(i64 158, i64 4098, i64 %address)",
        ),
        Some(Arch::Aarch64) => (
            "%current = call i64 asm sideeffect \"mrs $0, tpidr_el0\", \"=r\"()",
            "call void asm sideeffect \"msr tpidr_el0, $0\", \"r\"(ptr %tp)",
        ),
        other => bug!(Span::dummy(), "no thread pointer for {other:?}"),
    };
    let text = format!(
        "@.lilac.main_tls.area = internal global [{MAIN_TLS_BYTES} x i8] zeroinitializer, align 64\n\
         declare ptr @.lilac.tls_init(ptr, i64)\n\
         declare void @llvm.trap()\n\
         define void @.lilac.main_tls() {{\n\
         entry:\n  {read}\n  \
         %ready = icmp ne i64 %current, 0\n  \
         br i1 %ready, label %done, label %build\n\
         build:\n  \
         %tp = call ptr @.lilac.tls_init(ptr @.lilac.main_tls.area, i64 {MAIN_TLS_BYTES})\n  \
         %small = icmp eq ptr %tp, null\n  \
         br i1 %small, label %fail, label %install\n\
         install:\n  {write}\n  \
         br label %done\n\
         fail:\n  \
         call void @llvm.trap()\n  \
         unreachable\n\
         done:\n  \
         ret void\n\
         }}\n"
    );
    append_ir(module, ".lilac.main_tls", &text);
    match module.get_function(".lilac.main_tls") {
        Some(function) => function,
        None => bug!(Span::dummy(), "`.lilac.main_tls` was not defined"),
    }
}

/// Defines `.lilac.tls_init(ptr area, i64 size) -> ptr` once: lays out a
/// thread's TLS block in the zeroed `area` and returns its thread pointer,
/// or null when `size` bytes are too few.
fn tls_init(module: &Module<'_>, arch: Option<Arch>) {
    if module.get_function(".lilac.tls_init").is_some() {
        return;
    }
    let layout = match arch {
        Some(Arch::X86_64) => TLS_LAYOUT_X86_64,
        Some(Arch::Aarch64) => TLS_LAYOUT_AARCH64,
        other => bug!(Span::dummy(), "no TLS layout for {other:?}"),
    };
    append_ir(
        module,
        ".lilac.tls_init",
        &format!("{TLS_INIT_SCAN}{layout}{TLS_INIT_COPY}"),
    );
}

/// Defines on native Windows what a C runtime would for thread-locals: the
/// `_tls_used` directory the linker points the image at, bracketing the
/// `.tls` section, and the `_tls_index` slot the loader fills.
pub(crate) fn windows_tls_directory(module: &Module<'_>) {
    append_ir(module, "_tls_used", WINDOWS_TLS);
}

/// Whether `module` declares a thread-local global.
pub(crate) fn has_thread_locals(module: &Module<'_>) -> bool {
    module
        .get_globals()
        .any(inkwell::values::GlobalValue::is_thread_local)
}

/// The scan of `.lilac.tls_init`: walks the program headers for `PT_TLS`
/// (7) and the load bias from `PT_PHDR` (6), then ends in `%found` with the
/// TLS header's fields, or returns `area` when there is none.
const TLS_INIT_SCAN: &str = r"
@__ehdr_start = external hidden global i8
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)

define ptr @.lilac.tls_init(ptr %area, i64 %size) {
entry:
  %ehdr = ptrtoint ptr @__ehdr_start to i64
  %phoff.at = getelementptr i8, ptr @__ehdr_start, i64 32
  %phoff = load i64, ptr %phoff.at, align 1
  %phentsize.at = getelementptr i8, ptr @__ehdr_start, i64 54
  %phentsize.16 = load i16, ptr %phentsize.at, align 1
  %phentsize = zext i16 %phentsize.16 to i64
  %phnum.at = getelementptr i8, ptr @__ehdr_start, i64 56
  %phnum.16 = load i16, ptr %phnum.at, align 1
  %phnum = zext i16 %phnum.16 to i64
  %phdrs = add i64 %ehdr, %phoff
  br label %scan
scan:
  %index = phi i64 [ 0, %entry ], [ %next, %look ]
  %bias = phi i64 [ 0, %entry ], [ %bias.next, %look ]
  %tls = phi i64 [ 0, %entry ], [ %tls.next, %look ]
  %more = icmp ult i64 %index, %phnum
  br i1 %more, label %look, label %scanned
look:
  %offset = mul i64 %index, %phentsize
  %header = add i64 %phdrs, %offset
  %header.at = inttoptr i64 %header to ptr
  %type = load i32, ptr %header.at, align 1
  %header.vaddr.at = getelementptr i8, ptr %header.at, i64 16
  %header.vaddr = load i64, ptr %header.vaddr.at, align 1
  %is.phdr = icmp eq i32 %type, 6
  %phdr.bias = sub i64 %phdrs, %header.vaddr
  %bias.next = select i1 %is.phdr, i64 %phdr.bias, i64 %bias
  %is.tls = icmp eq i32 %type, 7
  %tls.next = select i1 %is.tls, i64 %header, i64 %tls
  %next = add i64 %index, 1
  br label %scan
scanned:
  %none = icmp eq i64 %tls, 0
  br i1 %none, label %empty, label %found
empty:
  ret ptr %area
found:
  %tls.at = inttoptr i64 %tls to ptr
  %vaddr.at = getelementptr i8, ptr %tls.at, i64 16
  %vaddr = load i64, ptr %vaddr.at, align 1
  %filesz.at = getelementptr i8, ptr %tls.at, i64 32
  %filesz = load i64, ptr %filesz.at, align 1
  %memsz.at = getelementptr i8, ptr %tls.at, i64 40
  %memsz = load i64, ptr %memsz.at, align 1
  %align.at = getelementptr i8, ptr %tls.at, i64 48
  %align.raw = load i64, ptr %align.at, align 1
  %align.zero = icmp eq i64 %align.raw, 0
  %align = select i1 %align.zero, i64 1, i64 %align.raw
  %mask = sub i64 %align, 1
  %unmask = xor i64 %mask, -1
  %image = add i64 %vaddr, %bias
  %start = ptrtoint ptr %area to i64
";

/// x86-64 (variant II): the block ends at the thread pointer, both aligned;
/// the thread pointer's word holds its own address.
const TLS_LAYOUT_X86_64: &str = r"
  %memsz.bumped = add i64 %memsz, %mask
  %memsz.up = and i64 %memsz.bumped, %unmask
  %raw = add i64 %start, %memsz.up
  %raw.bumped = add i64 %raw, %mask
  %tp = and i64 %raw.bumped, %unmask
  %block = sub i64 %tp, %memsz.up
  %end = add i64 %tp, 8
  %need = sub i64 %end, %start
  %fits = icmp ule i64 %need, %size
  br i1 %fits, label %copy, label %small
";

/// aarch64 (variant I): the thread pointer, aligned to the block's alignment
/// and at least 16, is followed by a 16-byte header and then the block, at
/// the offset the linker gave it.
const TLS_LAYOUT_AARCH64: &str = r"
  %wide = icmp ult i64 %align, 16
  %tp.align = select i1 %wide, i64 16, i64 %align
  %tp.mask = sub i64 %tp.align, 1
  %tp.unmask = xor i64 %tp.mask, -1
  %start.bumped = add i64 %start, %tp.mask
  %tp = and i64 %start.bumped, %tp.unmask
  %skew.raw = sub i64 %vaddr, 16
  %skew = and i64 %skew.raw, %mask
  %header.end = add i64 %tp, 16
  %block = add i64 %header.end, %skew
  %end = add i64 %block, %memsz
  %need = sub i64 %end, %start
  %fits = icmp ule i64 %need, %size
  br i1 %fits, label %copy, label %small
";

/// The end of `.lilac.tls_init`: copies the initialized image into the block
/// and stores the thread pointer's self-reference (variant II reads it;
/// variant I's header has room for it).
const TLS_INIT_COPY: &str = r"
small:
  ret ptr null
copy:
  %block.at = inttoptr i64 %block to ptr
  %image.at = inttoptr i64 %image to ptr
  call void @llvm.memcpy.p0.p0.i64(ptr %block.at, ptr %image.at, i64 %filesz, i1 false)
  %tp.at = inttoptr i64 %tp to ptr
  store i64 %tp, ptr %tp.at, align 8
  ret ptr %tp.at
}
";

/// `.lilac.linux_thread_clone`: the TLS block, then the assembly `clone`.
const THREAD_CLONE: &str = r"
declare ptr @.lilac.tls_init(ptr, i64)
declare i64 @.lilac.clone_raw(i64, ptr, ptr, ptr, ptr, ptr)

define i64 @.lilac.linux_thread_clone(i64 %flags, ptr %stack, ptr %parent, ptr %child, ptr %area, i64 %size, ptr %start) {
entry:
  %tp = call ptr @.lilac.tls_init(ptr %area, i64 %size)
  %small = icmp eq ptr %tp, null
  br i1 %small, label %fail, label %clone
fail:
  ret i64 -12
clone:
  %id = call i64 @.lilac.clone_raw(i64 %flags, ptr %stack, ptr %parent, ptr %child, ptr %tp, ptr %start)
  ret i64 %id
}
";

/// `.lilac.clone_raw(flags, stack, parent_tid, child_tid, tp, start)` on
/// x86-64: `start` goes on the child's stack, `clone` (56) takes `child_tid`
/// in `r10` and the thread pointer in `r8`. The child clears the frame
/// pointer, runs `.lilac.thread_entry(start)` and exits the thread (60).
const CLONE_X86_64: &str = r#"
.text
.type ".lilac.clone_raw",@function
.p2align 4
".lilac.clone_raw":
    andq $-16, %rsi
    subq $16, %rsi
    movq %r9, (%rsi)
    movq %rcx, %r10
    movl $56, %eax
    syscall
    testq %rax, %rax
    jnz 1f
    xorl %ebp, %ebp
    movq (%rsp), %rdi
    callq ".lilac.thread_entry"
    movl $60, %eax
    xorl %edi, %edi
    syscall
    ud2
1:
    retq
"#;

/// `.lilac.clone_raw` on aarch64: `clone` (220) takes the thread pointer in
/// `x3` and `child_tid` in `x4`. The child clears the frame and link
/// registers, runs `.lilac.thread_entry(start)` and exits the thread (93).
const CLONE_AARCH64: &str = r#"
.text
.type ".lilac.clone_raw",%function
.p2align 2
".lilac.clone_raw":

    and x1, x1, #-16
    sub x1, x1, #16
    str x5, [x1]
    mov x6, x3
    mov x3, x4
    mov x4, x6
    mov x8, #220
    svc #0
    cbnz x0, 1f
    mov x29, #0
    mov x30, #0
    ldr x0, [sp]
    bl ".lilac.thread_entry"
    mov x0, #0
    mov x8, #93
    svc #0
    brk #0
1:
    ret
"#;

/// The image TLS directory (`IMAGE_TLS_DIRECTORY64`) and its symbols, as the
/// MSVC runtime's `tlssup.c` defines them: the `.tls` contributions sort
/// between `_tls_start` and `_tls_end`.
const WINDOWS_TLS: &str = r#"
@_tls_index = global i32 0
@_tls_start = global i8 0, section ".tls"
@_tls_end = global i8 0, section ".tls$ZZZ"
@.lilac.tls_callbacks = internal constant [1 x ptr] zeroinitializer
@_tls_used = constant { ptr, ptr, ptr, ptr, i32, i32 } { ptr @_tls_start, ptr @_tls_end, ptr @_tls_index, ptr @.lilac.tls_callbacks, i32 0, i32 0 }, section ".rdata$T"
"#;
