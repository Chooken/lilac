; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [2 x i8] c"xy"
@c1 = private unnamed_addr constant [32 x i8] c"format piece index out of bounds"
@c2 = private unnamed_addr constant [40 x i8] c"  at main (format_args_use.lilac:14:27)\0A"
@c3 = private unnamed_addr constant [31 x i8] c"format hole index out of bounds"
@c4 = private unnamed_addr constant [39 x i8] c"  at main (format_args_use.lilac:15:5)\0A"
@.lilac.panic.head = private unnamed_addr constant [7 x i8] c"panic: "
@.lilac.panic.newline = private unnamed_addr constant [1 x i8] c"\0A"
@.lilac.panic.elided = private unnamed_addr constant [6 x i8] c"  ...\0A"
@.lilac.panic.no_trace = private unnamed_addr constant [55 x i8] c"  (stack trace disabled; build with --stack-traces=on)\0A"
@.lilac.panic.nested = private unnamed_addr constant [37 x i8] c"\0Apanic while writing a panic message\0A"
@.lilac.trace_head.slot = internal global ptr null
@.lilac.panic_depth.slot = internal global i64 0
@0 = private unnamed_addr constant [1 x i8] c"a"
@1 = private unnamed_addr constant [1 x i8] c"b"
@fmt0 = private unnamed_addr constant <{ i64, i64, ptr, ptr, i64, ptr, ptr }> <{ i64 1, i64 1, ptr @0, ptr @"@format(ReR3OutE)", i64 1, ptr @1, ptr null }>, align 8

; Function Attrs: alwaysinline nounwind
define ptr @.lilac.ptr_add(ptr %base, i64 %offset) #0 {
entry:
  %address = getelementptr i8, ptr %base, i64 %offset
  ret ptr %address
}

; Function Attrs: cold noreturn nounwind memory(inaccessiblemem: write)
declare void @llvm.trap() #1

define void @.lilac.abort() {
entry:
  call void @llvm.trap()
  unreachable
}

define ptr @.lilac.trace_head() {
entry:
  ret ptr @.lilac.trace_head.slot
}

define ptr @.lilac.panic_depth() {
entry:
  ret ptr @.lilac.panic_depth.slot
}

define void @"@format(ReR3OutE)"(ptr %in0, ptr %in1) {
entry:
  %tmp.1 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  %tmp.2 = alloca i64, align 8
  store i64 0, ptr %tmp.2, align 8
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  %0 = load ptr, ptr %in1, align 8
  %1 = load i64, ptr %0, align 8
  %2 = load ptr, ptr %in0, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %tmp.1, ptr align 8 %2, i64 16, i1 false)
  call void @.lilac.byte_buffer_len(ptr %tmp.1, ptr %tmp.2)
  %3 = load i64, ptr %tmp.2, align 8
  %4 = add i64 %1, %3
  %5 = load ptr, ptr %in1, align 8
  store ptr %5, ptr %tmp.3, align 8
  %6 = load ptr, ptr %tmp.3, align 8
  store i64 %4, ptr %6, align 8
  ret void
}

define void @.lilac.main() {
entry:
  %0 = alloca [8 x i8], align 8
  %part = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %part, i8 0, i64 16, i1 false)
  %tmp.1 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  %out = alloca [8 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %out, i8 0, i64 8, i1 false)
  %tmp.2 = alloca [8 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.2, i8 0, i64 8, i1 false)
  %msg = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %msg, i8 0, i64 16, i1 false)
  %tmp.3 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.3, i8 0, i64 16, i1 false)
  %holes = alloca i64, align 8
  store i64 0, ptr %holes, align 8
  %tmp.4 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.4, i8 0, i64 16, i1 false)
  %tmp.5 = alloca i64, align 8
  store i64 0, ptr %tmp.5, align 8
  %first = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %first, i8 0, i64 16, i1 false)
  %tmp.6 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.6, i8 0, i64 16, i1 false)
  %tmp.7 = alloca i64, align 8
  store i64 0, ptr %tmp.7, align 8
  %tmp.8 = alloca i64, align 8
  store i64 0, ptr %tmp.8, align 8
  %tmp.9 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.9, i8 0, i64 16, i1 false)
  %tmp.10 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.10, i8 0, i64 16, i1 false)
  %tmp.11 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.11, i8 0, i64 16, i1 false)
  %tmp.12 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.12, i8 0, i64 16, i1 false)
  %tmp.13 = alloca i64, align 8
  store i64 0, ptr %tmp.13, align 8
  %tmp.14 = alloca ptr, align 8
  store ptr null, ptr %tmp.14, align 8
  %tmp.15 = alloca i64, align 8
  store i64 0, ptr %tmp.15, align 8
  %tmp.16 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.16, i8 0, i64 16, i1 false)
  %tmp.17 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.17, i8 0, i64 16, i1 false)
  %1 = call ptr @.lilac.offset(ptr %tmp.1, i64 8)
  store ptr @c0, ptr %1, align 8
  store i64 2, ptr %tmp.1, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %part, ptr align 8 %tmp.1, i64 16, i1 false)
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.2, i8 0, i64 8, i1 false)
  store i64 0, ptr %tmp.2, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %out, ptr align 8 %tmp.2, i64 8, i1 false)
  store ptr %part, ptr %0, align 8
  store ptr @fmt0, ptr %tmp.3, align 8
  %2 = call ptr @.lilac.offset(ptr %tmp.3, i64 8)
  store ptr %0, ptr %2, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %msg, ptr align 8 %tmp.3, i64 16, i1 false)
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %tmp.4, ptr align 8 %msg, i64 16, i1 false)
  call void @.lilac.format_args_holes(ptr %tmp.4, ptr %tmp.5)
  %3 = load i64, ptr %tmp.5, align 8
  store i64 %3, ptr %holes, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %tmp.6, ptr align 8 %msg, i64 16, i1 false)
  store i64 0, ptr %tmp.7, align 8
  call void @.lilac.format_args_holes(ptr %tmp.6, ptr %tmp.8)
  %4 = load i64, ptr %tmp.8, align 8
  %5 = load i64, ptr %tmp.7, align 8
  %6 = icmp ult i64 %4, %5
  %7 = zext i1 %6 to i8
  %8 = icmp ne i8 %7, 0
  br i1 %8, label %l.2, label %l.1

l.2:                                              ; preds = %entry
  %9 = call ptr @.lilac.offset(ptr %tmp.9, i64 8)
  store ptr @c1, ptr %9, align 8
  store i64 32, ptr %tmp.9, align 8
  %10 = call ptr @.lilac.offset(ptr %tmp.10, i64 8)
  store ptr @c2, ptr %10, align 8
  store i64 40, ptr %tmp.10, align 8
  call void @.lilac.panic(ptr %tmp.9, ptr %tmp.10)
  unreachable

l.1:                                              ; preds = %entry
  call void @.lilac.format_args_piece(ptr %tmp.6, ptr %tmp.7, ptr %tmp.11)
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %first, ptr align 8 %tmp.11, i64 16, i1 false)
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %tmp.12, ptr align 8 %msg, i64 16, i1 false)
  store i64 0, ptr %tmp.13, align 8
  store ptr %out, ptr %tmp.14, align 8
  call void @.lilac.format_args_holes(ptr %tmp.12, ptr %tmp.15)
  %11 = load i64, ptr %tmp.15, align 8
  %12 = load i64, ptr %tmp.13, align 8
  %13 = icmp ult i64 %12, %11
  %14 = zext i1 %13 to i8
  %15 = icmp ne i8 %14, 0
  br i1 %15, label %l.3, label %l.4

l.4:                                              ; preds = %l.1
  %16 = call ptr @.lilac.offset(ptr %tmp.16, i64 8)
  store ptr @c3, ptr %16, align 8
  store i64 31, ptr %tmp.16, align 8
  %17 = call ptr @.lilac.offset(ptr %tmp.17, i64 8)
  store ptr @c4, ptr %17, align 8
  store i64 39, ptr %tmp.17, align 8
  call void @.lilac.panic(ptr %tmp.16, ptr %tmp.17)
  unreachable

l.3:                                              ; preds = %l.1
  call void @.lilac.format_args_hole(ptr %tmp.12, ptr %tmp.13, ptr %tmp.14)
  ret void
}

define void @.lilac.byte_buffer_len(ptr %in0, ptr %out0) {
entry:
  %0 = load i64, ptr %in0, align 8
  store i64 %0, ptr %out0, align 8
  ret void
}

; Function Attrs: alwaysinline
define void @.lilac.format_args_holes(ptr %in0, ptr %out0) #2 {
entry:
  %template = alloca ptr, align 8
  store ptr null, ptr %template, align 8
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %template, align 8
  %1 = load ptr, ptr %template, align 8
  %2 = load i64, ptr %1, align 8
  store i64 %2, ptr %out0, align 8
  ret void
}

; Function Attrs: alwaysinline
define void @.lilac.format_args_piece(ptr %in0, ptr %in1, ptr %out0) #2 {
entry:
  %part.base = alloca ptr, align 8
  store ptr null, ptr %part.base, align 8
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %part.base, align 8
  %1 = load i64, ptr %in1, align 8
  %2 = mul i64 %1, 24
  %3 = add i64 %2, 8
  %part.at = alloca i64, align 8
  store i64 0, ptr %part.at, align 8
  store i64 %3, ptr %part.at, align 8
  %part = alloca ptr, align 8
  store ptr null, ptr %part, align 8
  %4 = load ptr, ptr %part.base, align 8
  %5 = load i64, ptr %part.at, align 8
  %6 = call ptr @.lilac.ptr_add(ptr %4, i64 %5)
  store ptr %6, ptr %part, align 8
  %7 = load ptr, ptr %part, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %out0, ptr align 8 %7, i64 16, i1 false)
  ret void
}

; Function Attrs: alwaysinline
define void @.lilac.format_args_hole(ptr %in0, ptr %in1, ptr %in2) #2 {
entry:
  %part.base = alloca ptr, align 8
  store ptr null, ptr %part.base, align 8
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %part.base, align 8
  %1 = load i64, ptr %in1, align 8
  %2 = mul i64 %1, 24
  %3 = add i64 %2, 24
  %part.at = alloca i64, align 8
  store i64 0, ptr %part.at, align 8
  store i64 %3, ptr %part.at, align 8
  %part = alloca ptr, align 8
  store ptr null, ptr %part, align 8
  %4 = load ptr, ptr %part.base, align 8
  %5 = load i64, ptr %part.at, align 8
  %6 = call ptr @.lilac.ptr_add(ptr %4, i64 %5)
  store ptr %6, ptr %part, align 8
  %hole.base = alloca ptr, align 8
  store ptr null, ptr %hole.base, align 8
  %7 = call ptr @.lilac.offset(ptr %in0, i64 8)
  %8 = load ptr, ptr %7, align 8
  store ptr %8, ptr %hole.base, align 8
  %9 = load i64, ptr %in1, align 8
  %10 = mul i64 %9, 8
  %11 = add i64 %10, 0
  %hole.at = alloca i64, align 8
  store i64 0, ptr %hole.at, align 8
  store i64 %11, ptr %hole.at, align 8
  %hole = alloca ptr, align 8
  store ptr null, ptr %hole, align 8
  %12 = load ptr, ptr %hole.base, align 8
  %13 = load i64, ptr %hole.at, align 8
  %14 = call ptr @.lilac.ptr_add(ptr %12, i64 %13)
  store ptr %14, ptr %hole, align 8
  %format = alloca ptr, align 8
  store ptr null, ptr %format, align 8
  %15 = load ptr, ptr %part, align 8
  %16 = load ptr, ptr %15, align 8
  store ptr %16, ptr %format, align 8
  %value = alloca ptr, align 8
  store ptr null, ptr %value, align 8
  %17 = load ptr, ptr %hole, align 8
  %18 = load ptr, ptr %17, align 8
  store ptr %18, ptr %value, align 8
  %19 = load ptr, ptr %format, align 8
  call void %19(ptr %value, ptr %in2)
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
  %depth = alloca ptr, align 8
  store ptr null, ptr %depth, align 8
  %0 = call ptr @.lilac.panic_depth()
  store ptr %0, ptr %depth, align 8
  %1 = load ptr, ptr %depth, align 8
  %2 = load i64, ptr %1, align 8
  %3 = add i64 %2, 1
  %4 = load ptr, ptr %depth, align 8
  store i64 %3, ptr %4, align 8
  %5 = icmp eq i64 %2, 0
  %6 = zext i1 %5 to i8
  %7 = icmp ne i8 %6, 0
  br i1 %7, label %start, label %again

again:                                            ; preds = %entry
  %8 = load ptr, ptr %depth, align 8
  %9 = load i64, ptr %8, align 8
  %10 = icmp eq i64 %9, 2
  %11 = zext i1 %10 to i8
  %12 = icmp ne i8 %11, 0
  br i1 %12, label %nested, label %broken

nested:                                           ; preds = %again
  %13 = call ptr @.lilac.offset(ptr %text, i64 8)
  store ptr @.lilac.panic.nested, ptr %13, align 8
  store i64 37, ptr %text, align 8
  call void @.lilac.compiler_print(ptr %text)
  br label %exit

broken:                                           ; preds = %again
  call void @.lilac.abort()
  unreachable

start:                                            ; preds = %entry
  %14 = call ptr @.lilac.offset(ptr %text, i64 8)
  store ptr @.lilac.panic.head, ptr %14, align 8
  store i64 7, ptr %text, align 8
  call void @.lilac.compiler_print(ptr %text)
  call void @.lilac.compiler_print(ptr %in0)
  %15 = call ptr @.lilac.offset(ptr %text, i64 8)
  store ptr @.lilac.panic.newline, ptr %15, align 8
  store i64 1, ptr %text, align 8
  call void @.lilac.compiler_print(ptr %text)
  %16 = call ptr @.lilac.trace_head()
  store ptr %16, ptr %head, align 8
  %17 = load ptr, ptr %head, align 8
  %18 = load ptr, ptr %17, align 8
  store ptr %18, ptr %frame, align 8
  %19 = ptrtoint ptr %18 to i64
  %20 = icmp eq i64 %19, 0
  %21 = zext i1 %20 to i8
  %22 = icmp ne i8 %21, 0
  br i1 %22, label %untraced, label %walk

untraced:                                         ; preds = %start
  call void @.lilac.compiler_print(ptr %in1)
  %23 = call ptr @.lilac.offset(ptr %text, i64 8)
  store ptr @.lilac.panic.no_trace, ptr %23, align 8
  store i64 55, ptr %text, align 8
  call void @.lilac.compiler_print(ptr %text)
  br label %exit

walk:                                             ; preds = %print, %start
  %24 = load ptr, ptr %frame, align 8
  %25 = ptrtoint ptr %24 to i64
  %26 = icmp eq i64 %25, 0
  %27 = zext i1 %26 to i8
  %28 = icmp ne i8 %27, 0
  br i1 %28, label %exit, label %check

check:                                            ; preds = %walk
  %29 = load i64, ptr %count, align 8
  %30 = icmp eq i64 %29, 128
  %31 = zext i1 %30 to i8
  %32 = icmp ne i8 %31, 0
  br i1 %32, label %elided, label %print

print:                                            ; preds = %check
  %33 = load ptr, ptr %frame, align 8
  %34 = call ptr @.lilac.offset(ptr %33, i64 8)
  store ptr %34, ptr %frame.site, align 8
  %35 = load ptr, ptr %frame.site, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %text, ptr align 8 %35, i64 16, i1 false)
  call void @.lilac.compiler_print(ptr %text)
  %36 = load ptr, ptr %frame, align 8
  %37 = load ptr, ptr %36, align 8
  store ptr %37, ptr %frame, align 8
  %38 = load i64, ptr %count, align 8
  %39 = add i64 %38, 1
  store i64 %39, ptr %count, align 8
  br label %walk

elided:                                           ; preds = %check
  %40 = call ptr @.lilac.offset(ptr %text, i64 8)
  store ptr @.lilac.panic.elided, ptr %40, align 8
  store i64 6, ptr %text, align 8
  call void @.lilac.compiler_print(ptr %text)
  br label %exit

exit:                                             ; preds = %elided, %walk, %untraced, %nested
  %code = alloca i32, align 4
  store i32 0, ptr %code, align 4
  store i32 101, ptr %code, align 4
  call void @.lilac.exit(ptr %code)
  call void @.lilac.abort()
  unreachable
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #3

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #4

; Function Attrs: alwaysinline nounwind
define internal ptr @.lilac.offset(ptr %base, i64 %offset) #0 {
entry:
  %address = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %address
}

attributes #0 = { alwaysinline nounwind }
attributes #1 = { cold noreturn nounwind memory(inaccessiblemem: write) }
attributes #2 = { alwaysinline }
attributes #3 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #4 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
