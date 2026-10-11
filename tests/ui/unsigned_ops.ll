; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [16 x i8] c"division by zero"
@c1 = private unnamed_addr constant [35 x i8] c"  at run (unsigned_ops.lilac:4:24)\0A"
@c2 = private unnamed_addr constant [35 x i8] c"  at run (unsigned_ops.lilac:5:25)\0A"
@c3 = private unnamed_addr constant [36 x i8] c"  at run (unsigned_ops.lilac:14:34)\0A"
@c4 = private unnamed_addr constant [36 x i8] c"  at run (unsigned_ops.lilac:15:35)\0A"
@.lilac.panic.head = private unnamed_addr constant [7 x i8] c"panic: "
@.lilac.panic.newline = private unnamed_addr constant [1 x i8] c"\0A"
@.lilac.panic.elided = private unnamed_addr constant [6 x i8] c"  ...\0A"
@.lilac.panic.no_trace = private unnamed_addr constant [55 x i8] c"  (stack trace disabled; build with --stack-traces=on)\0A"
@.lilac.trace_head.slot = internal global ptr null

; Function Attrs: cold noreturn nounwind memory(inaccessiblemem: write)
declare void @llvm.trap() #0

define void @.lilac.abort() {
entry:
  call void @llvm.trap()
  unreachable
}

define ptr @.lilac.trace_head() {
entry:
  ret ptr @.lilac.trace_head.slot
}

define void @.root.run(ptr %in0, ptr %in1, ptr %in2) {
entry:
  %one = alloca i32, align 4
  store i32 0, ptr %one, align 4
  %divisor = alloca i32, align 4
  store i32 0, ptr %divisor, align 4
  %quotient = alloca i32, align 4
  store i32 0, ptr %quotient, align 4
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.3, i8 0, i64 16, i1 false)
  %tmp.4 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.4, i8 0, i64 16, i1 false)
  %remainder = alloca i32, align 4
  store i32 0, ptr %remainder, align 4
  %tmp.5 = alloca i32, align 4
  store i32 0, ptr %tmp.5, align 4
  %tmp.6 = alloca i32, align 4
  store i32 0, ptr %tmp.6, align 4
  %tmp.7 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.7, i8 0, i64 16, i1 false)
  %tmp.8 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.8, i8 0, i64 16, i1 false)
  %native_one = alloca i64, align 8
  store i64 0, ptr %native_one, align 8
  %wide_divisor = alloca i64, align 8
  store i64 0, ptr %wide_divisor, align 8
  %wide_quotient = alloca i64, align 8
  store i64 0, ptr %wide_quotient, align 8
  %tmp.9 = alloca i64, align 8
  store i64 0, ptr %tmp.9, align 8
  %tmp.10 = alloca i64, align 8
  store i64 0, ptr %tmp.10, align 8
  %tmp.11 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.11, i8 0, i64 16, i1 false)
  %tmp.12 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.12, i8 0, i64 16, i1 false)
  %wide_remainder = alloca i64, align 8
  store i64 0, ptr %wide_remainder, align 8
  %tmp.13 = alloca i64, align 8
  store i64 0, ptr %tmp.13, align 8
  %tmp.14 = alloca i64, align 8
  store i64 0, ptr %tmp.14, align 8
  %tmp.15 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.15, i8 0, i64 16, i1 false)
  %tmp.16 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.16, i8 0, i64 16, i1 false)
  store i32 1, ptr %one, align 4
  %0 = load i32, ptr %in1, align 4
  %1 = load i32, ptr %one, align 4
  %2 = add i32 %0, %1
  store i32 %2, ptr %divisor, align 4
  %3 = load i32, ptr %in0, align 4
  %4 = load i32, ptr %divisor, align 4
  store i32 %3, ptr %tmp.1, align 4
  store i32 %4, ptr %tmp.2, align 4
  %5 = icmp eq i32 %4, 0
  %6 = zext i1 %5 to i8
  %7 = icmp ne i8 %6, 0
  br i1 %7, label %l.1, label %l.2

l.1:                                              ; preds = %entry
  %8 = call ptr @.lilac.offset(ptr %tmp.3, i64 8)
  store ptr @c0, ptr %8, align 8
  store i64 16, ptr %tmp.3, align 8
  %9 = call ptr @.lilac.offset(ptr %tmp.4, i64 8)
  store ptr @c1, ptr %9, align 8
  store i64 35, ptr %tmp.4, align 8
  call void @.lilac.panic(ptr %tmp.3, ptr %tmp.4)
  unreachable

l.2:                                              ; preds = %entry
  %10 = load i32, ptr %tmp.1, align 4
  %11 = load i32, ptr %tmp.2, align 4
  %12 = udiv i32 %10, %11
  store i32 %12, ptr %quotient, align 4
  %13 = load i32, ptr %in0, align 4
  %14 = load i32, ptr %divisor, align 4
  store i32 %13, ptr %tmp.5, align 4
  store i32 %14, ptr %tmp.6, align 4
  %15 = icmp eq i32 %14, 0
  %16 = zext i1 %15 to i8
  %17 = icmp ne i8 %16, 0
  br i1 %17, label %l.3, label %l.4

l.3:                                              ; preds = %l.2
  %18 = call ptr @.lilac.offset(ptr %tmp.7, i64 8)
  store ptr @c0, ptr %18, align 8
  store i64 16, ptr %tmp.7, align 8
  %19 = call ptr @.lilac.offset(ptr %tmp.8, i64 8)
  store ptr @c2, ptr %19, align 8
  store i64 35, ptr %tmp.8, align 8
  call void @.lilac.panic(ptr %tmp.7, ptr %tmp.8)
  unreachable

l.4:                                              ; preds = %l.2
  %20 = load i32, ptr %tmp.5, align 4
  %21 = load i32, ptr %tmp.6, align 4
  %22 = urem i32 %20, %21
  store i32 %22, ptr %remainder, align 4
  %23 = load i32, ptr %in0, align 4
  %24 = load i32, ptr %divisor, align 4
  %25 = icmp ult i32 %23, %24
  %26 = zext i1 %25 to i8
  %27 = icmp ne i8 %26, 0
  br i1 %27, label %l.5, label %l.6

l.5:                                              ; preds = %l.4
  store i32 1, ptr %quotient, align 4
  br label %l.7

l.6:                                              ; preds = %l.4
  br label %l.7

l.7:                                              ; preds = %l.6, %l.5
  %28 = load i32, ptr %in0, align 4
  %29 = load i32, ptr %divisor, align 4
  %30 = icmp ugt i32 %28, %29
  %31 = zext i1 %30 to i8
  %32 = icmp ne i8 %31, 0
  br i1 %32, label %l.8, label %l.9

l.8:                                              ; preds = %l.7
  store i32 1, ptr %remainder, align 4
  br label %l.10

l.9:                                              ; preds = %l.7
  br label %l.10

l.10:                                             ; preds = %l.9, %l.8
  store i64 1, ptr %native_one, align 8
  %33 = load i64, ptr %in2, align 8
  %34 = load i64, ptr %native_one, align 8
  %35 = add i64 %33, %34
  store i64 %35, ptr %wide_divisor, align 8
  %36 = load i64, ptr %in2, align 8
  %37 = load i64, ptr %wide_divisor, align 8
  store i64 %36, ptr %tmp.9, align 8
  store i64 %37, ptr %tmp.10, align 8
  %38 = icmp eq i64 %37, 0
  %39 = zext i1 %38 to i8
  %40 = icmp ne i8 %39, 0
  br i1 %40, label %l.11, label %l.12

l.11:                                             ; preds = %l.10
  %41 = call ptr @.lilac.offset(ptr %tmp.11, i64 8)
  store ptr @c0, ptr %41, align 8
  store i64 16, ptr %tmp.11, align 8
  %42 = call ptr @.lilac.offset(ptr %tmp.12, i64 8)
  store ptr @c3, ptr %42, align 8
  store i64 36, ptr %tmp.12, align 8
  call void @.lilac.panic(ptr %tmp.11, ptr %tmp.12)
  unreachable

l.12:                                             ; preds = %l.10
  %43 = load i64, ptr %tmp.9, align 8
  %44 = load i64, ptr %tmp.10, align 8
  %45 = udiv i64 %43, %44
  store i64 %45, ptr %wide_quotient, align 8
  %46 = load i64, ptr %in2, align 8
  %47 = load i64, ptr %wide_divisor, align 8
  store i64 %46, ptr %tmp.13, align 8
  store i64 %47, ptr %tmp.14, align 8
  %48 = icmp eq i64 %47, 0
  %49 = zext i1 %48 to i8
  %50 = icmp ne i8 %49, 0
  br i1 %50, label %l.13, label %l.14

l.13:                                             ; preds = %l.12
  %51 = call ptr @.lilac.offset(ptr %tmp.15, i64 8)
  store ptr @c0, ptr %51, align 8
  store i64 16, ptr %tmp.15, align 8
  %52 = call ptr @.lilac.offset(ptr %tmp.16, i64 8)
  store ptr @c4, ptr %52, align 8
  store i64 36, ptr %tmp.16, align 8
  call void @.lilac.panic(ptr %tmp.15, ptr %tmp.16)
  unreachable

l.14:                                             ; preds = %l.12
  %53 = load i64, ptr %tmp.13, align 8
  %54 = load i64, ptr %tmp.14, align 8
  %55 = urem i64 %53, %54
  store i64 %55, ptr %wide_remainder, align 8
  ret void
}

define void @.lilac.main() {
entry:
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca i64, align 8
  store i64 0, ptr %tmp.3, align 8
  store i32 0, ptr %tmp.1, align 4
  store i32 0, ptr %tmp.2, align 4
  store i64 0, ptr %tmp.3, align 8
  call void @.root.run(ptr %tmp.1, ptr %tmp.2, ptr %tmp.3)
  ret void
}

declare void @.lilac.exit(ptr)

declare void @.lilac.compiler_print(ptr)

define void @.lilac.panic(ptr %in0, ptr %in1) {
entry:
  %text = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %text, i8 0, i64 16, i1 false)
  %head = alloca ptr, align 8
  store ptr null, ptr %head, align 8
  %frame = alloca ptr, align 8
  store ptr null, ptr %frame, align 8
  %count = alloca i64, align 8
  store i64 0, ptr %count, align 8
  %frame.site = alloca ptr, align 8
  store ptr null, ptr %frame.site, align 8
  %0 = call ptr @.lilac.offset(ptr %text, i64 8)
  store ptr @.lilac.panic.head, ptr %0, align 8
  store i64 7, ptr %text, align 8
  call void @.lilac.compiler_print(ptr %text)
  call void @.lilac.compiler_print(ptr %in0)
  %1 = call ptr @.lilac.offset(ptr %text, i64 8)
  store ptr @.lilac.panic.newline, ptr %1, align 8
  store i64 1, ptr %text, align 8
  call void @.lilac.compiler_print(ptr %text)
  %2 = call ptr @.lilac.trace_head()
  store ptr %2, ptr %head, align 8
  %3 = load ptr, ptr %head, align 8
  %4 = load ptr, ptr %3, align 8
  store ptr %4, ptr %frame, align 8
  %5 = ptrtoint ptr %4 to i64
  %6 = icmp eq i64 %5, 0
  %7 = zext i1 %6 to i8
  %8 = icmp ne i8 %7, 0
  br i1 %8, label %untraced, label %walk

untraced:                                         ; preds = %entry
  call void @.lilac.compiler_print(ptr %in1)
  %9 = call ptr @.lilac.offset(ptr %text, i64 8)
  store ptr @.lilac.panic.no_trace, ptr %9, align 8
  store i64 55, ptr %text, align 8
  call void @.lilac.compiler_print(ptr %text)
  br label %exit

walk:                                             ; preds = %print, %entry
  %10 = load ptr, ptr %frame, align 8
  %11 = ptrtoint ptr %10 to i64
  %12 = icmp eq i64 %11, 0
  %13 = zext i1 %12 to i8
  %14 = icmp ne i8 %13, 0
  br i1 %14, label %exit, label %check

check:                                            ; preds = %walk
  %15 = load i64, ptr %count, align 8
  %16 = icmp eq i64 %15, 128
  %17 = zext i1 %16 to i8
  %18 = icmp ne i8 %17, 0
  br i1 %18, label %elided, label %print

print:                                            ; preds = %check
  %19 = load ptr, ptr %frame, align 8
  %20 = call ptr @.lilac.offset(ptr %19, i64 8)
  store ptr %20, ptr %frame.site, align 8
  %21 = load ptr, ptr %frame.site, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %text, ptr align 8 %21, i64 16, i1 false)
  call void @.lilac.compiler_print(ptr %text)
  %22 = load ptr, ptr %frame, align 8
  %23 = load ptr, ptr %22, align 8
  store ptr %23, ptr %frame, align 8
  %24 = load i64, ptr %count, align 8
  %25 = add i64 %24, 1
  store i64 %25, ptr %count, align 8
  br label %walk

elided:                                           ; preds = %check
  %26 = call ptr @.lilac.offset(ptr %text, i64 8)
  store ptr @.lilac.panic.elided, ptr %26, align 8
  store i64 6, ptr %text, align 8
  call void @.lilac.compiler_print(ptr %text)
  br label %exit

exit:                                             ; preds = %elided, %walk, %untraced
  %code = alloca i32, align 4
  store i32 0, ptr %code, align 4
  store i32 101, ptr %code, align 4
  call void @.lilac.exit(ptr %code)
  call void @.lilac.abort()
  unreachable
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #1

; Function Attrs: alwaysinline nounwind
define internal ptr @.lilac.offset(ptr %base, i64 %offset) #2 {
entry:
  %address = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %address
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #3

attributes #0 = { cold noreturn nounwind memory(inaccessiblemem: write) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #2 = { alwaysinline nounwind }
attributes #3 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
