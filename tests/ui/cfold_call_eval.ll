; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [2 x i8] c"AB"

define ptr @.lilac.ptr_add(ptr %base, i64 %offset) {
entry:
  %0 = ptrtoint ptr %base to i64
  %1 = add i64 %0, %offset
  %2 = inttoptr i64 %1 to ptr
  ret ptr %2
}

define void @parse(ptr %in0, ptr %out0) {
entry:
  %i = alloca i64, align 8
  store i64 0, ptr %i, align 8
  %acc = alloca i64, align 8
  store i64 0, ptr %acc, align 8
  %len = alloca i64, align 8
  store i64 0, ptr %len, align 8
  %tmp.1 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.1, align 8
  %tmp.2 = alloca i64, align 8
  store i64 0, ptr %tmp.2, align 8
  %ptr = alloca ptr, align 8
  store ptr null, ptr %ptr, align 8
  %tmp.3 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.3, align 8
  %tmp.4 = alloca ptr, align 8
  store ptr null, ptr %tmp.4, align 8
  %c = alloca i64, align 8
  store i64 0, ptr %c, align 8
  %tmp.5 = alloca ptr, align 8
  store ptr null, ptr %tmp.5, align 8
  %tmp.6 = alloca i64, align 8
  store i64 0, ptr %tmp.6, align 8
  %tmp.7 = alloca i8, align 1
  store i8 0, ptr %tmp.7, align 1
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (i64, ptr null, i32 1) to i64), i1 false)
  store i64 0, ptr %i, align 8
  store i64 0, ptr %acc, align 8
  %0 = load { i64, ptr }, ptr %in0, align 8
  store { i64, ptr } %0, ptr %tmp.1, align 8
  call void @.lilac.byte_buffer_len(ptr %tmp.1, ptr %tmp.2)
  %1 = load i64, ptr %tmp.2, align 8
  store i64 %1, ptr %len, align 8
  %2 = load { i64, ptr }, ptr %in0, align 8
  store { i64, ptr } %2, ptr %tmp.3, align 8
  call void @.lilac.byte_buffer_ptr(ptr %tmp.3, ptr %tmp.4)
  %3 = load ptr, ptr %tmp.4, align 8
  store ptr %3, ptr %ptr, align 8
  br label %l.1

l.1:                                              ; preds = %l.5, %entry
  %4 = load i64, ptr %i, align 8
  %5 = load i64, ptr %len, align 8
  %6 = icmp slt i64 %4, %5
  %7 = zext i1 %6 to i8
  %8 = icmp ne i8 %7, 0
  br i1 %8, label %l.3, label %l.4

l.3:                                              ; preds = %l.1
  %9 = load ptr, ptr %ptr, align 8
  store ptr %9, ptr %tmp.5, align 8
  %10 = load i64, ptr %i, align 8
  store i64 %10, ptr %tmp.6, align 8
  call void @.lilac.load_u8(ptr %tmp.5, ptr %tmp.6, ptr %tmp.7)
  %11 = load i8, ptr %tmp.7, align 1
  %12 = zext i8 %11 to i64
  store i64 %12, ptr %c, align 8
  %13 = load i64, ptr %acc, align 8
  %14 = load i64, ptr %c, align 8
  %15 = add i64 %13, %14
  store i64 %15, ptr %acc, align 8
  %16 = load i64, ptr %i, align 8
  %17 = add i64 %16, 1
  store i64 %17, ptr %i, align 8
  br label %l.5

l.4:                                              ; preds = %l.1
  br label %l.2

l.5:                                              ; preds = %l.3
  br label %l.1

l.2:                                              ; preds = %l.4
  %18 = load i64, ptr %acc, align 8
  store i64 %18, ptr %out0, align 8
  ret void
}

define void @.lilac.main() {
entry:
  %total = alloca i64, align 8
  store i64 0, ptr %total, align 8
  %tmp.1 = alloca i64, align 8
  store i64 0, ptr %tmp.1, align 8
  %tmp.2 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.2, align 8
  store { i64, ptr } { i64 2, ptr @c0 }, ptr %tmp.2, align 8
  call void @parse(ptr %tmp.2, ptr %tmp.1)
  %0 = load i64, ptr %tmp.1, align 8
  store i64 %0, ptr %total, align 8
  ret void
}

define void @.lilac.byte_buffer_ptr(ptr %in0, ptr %out0) {
entry:
  %0 = getelementptr inbounds { i64, ptr }, ptr %in0, i32 0, i32 1
  %1 = load ptr, ptr %0, align 8
  store ptr %1, ptr %out0, align 8
  ret void
}

define void @.lilac.byte_buffer_len(ptr %in0, ptr %out0) {
entry:
  %0 = getelementptr inbounds { i64, ptr }, ptr %in0, i32 0, i32 0
  %1 = load i64, ptr %0, align 8
  store i64 %1, ptr %out0, align 8
  ret void
}

define void @.lilac.load_u8(ptr %in0, ptr %in1, ptr %out0) {
entry:
  %slot = alloca ptr, align 8
  store ptr null, ptr %slot, align 8
  %0 = load ptr, ptr %in0, align 8
  %1 = load i64, ptr %in1, align 8
  %2 = call ptr @.lilac.ptr_add(ptr %0, i64 %1)
  store ptr %2, ptr %slot, align 8
  %3 = load ptr, ptr %slot, align 8
  %4 = load i8, ptr %3, align 1
  store i8 %4, ptr %out0, align 1
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
