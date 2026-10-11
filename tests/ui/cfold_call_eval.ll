; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [2 x i8] c"AB"

; Function Attrs: alwaysinline nounwind
define ptr @.lilac.ptr_add(ptr %base, i64 %offset) #0 {
entry:
  %address = getelementptr i8, ptr %base, i64 %offset
  ret ptr %address
}

define void @.root.parse(ptr %in0, ptr %out0) {
entry:
  %i = alloca i64, align 8
  store i64 0, ptr %i, align 8
  %acc = alloca i64, align 8
  store i64 0, ptr %acc, align 8
  %len = alloca i64, align 8
  store i64 0, ptr %len, align 8
  %tmp.1 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  %tmp.2 = alloca i64, align 8
  store i64 0, ptr %tmp.2, align 8
  %ptr = alloca ptr, align 8
  store ptr null, ptr %ptr, align 8
  %tmp.3 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.3, i8 0, i64 16, i1 false)
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
  call void @llvm.memset.p0.i64(ptr align 8 %out0, i8 0, i64 8, i1 false)
  store i64 0, ptr %i, align 8
  store i64 0, ptr %acc, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %tmp.1, ptr align 8 %in0, i64 16, i1 false)
  call void @.lilac.byte_buffer_len(ptr %tmp.1, ptr %tmp.2)
  %0 = load i64, ptr %tmp.2, align 8
  store i64 %0, ptr %len, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %tmp.3, ptr align 8 %in0, i64 16, i1 false)
  call void @.lilac.byte_buffer_ptr(ptr %tmp.3, ptr %tmp.4)
  %1 = load ptr, ptr %tmp.4, align 8
  store ptr %1, ptr %ptr, align 8
  br label %l.1

l.1:                                              ; preds = %l.5, %entry
  %2 = load i64, ptr %i, align 8
  %3 = load i64, ptr %len, align 8
  %4 = icmp slt i64 %2, %3
  %5 = zext i1 %4 to i8
  %6 = icmp ne i8 %5, 0
  br i1 %6, label %l.3, label %l.4

l.3:                                              ; preds = %l.1
  %7 = load ptr, ptr %ptr, align 8
  store ptr %7, ptr %tmp.5, align 8
  %8 = load i64, ptr %i, align 8
  store i64 %8, ptr %tmp.6, align 8
  call void @.lilac.load_u8(ptr %tmp.5, ptr %tmp.6, ptr %tmp.7)
  %9 = load i8, ptr %tmp.7, align 1
  %10 = zext i8 %9 to i64
  store i64 %10, ptr %c, align 8
  %11 = load i64, ptr %acc, align 8
  %12 = load i64, ptr %c, align 8
  %13 = add i64 %11, %12
  store i64 %13, ptr %acc, align 8
  %14 = load i64, ptr %i, align 8
  %15 = add i64 %14, 1
  store i64 %15, ptr %i, align 8
  br label %l.5

l.4:                                              ; preds = %l.1
  br label %l.2

l.5:                                              ; preds = %l.3
  br label %l.1

l.2:                                              ; preds = %l.4
  %16 = load i64, ptr %acc, align 8
  store i64 %16, ptr %out0, align 8
  ret void
}

define void @.lilac.main() {
entry:
  %total = alloca i64, align 8
  store i64 0, ptr %total, align 8
  %tmp.1 = alloca i64, align 8
  store i64 0, ptr %tmp.1, align 8
  %tmp.2 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.2, i8 0, i64 16, i1 false)
  %0 = call ptr @.lilac.offset(ptr %tmp.2, i64 8)
  store ptr @c0, ptr %0, align 8
  store i64 2, ptr %tmp.2, align 8
  call void @.root.parse(ptr %tmp.2, ptr %tmp.1)
  %1 = load i64, ptr %tmp.1, align 8
  store i64 %1, ptr %total, align 8
  ret void
}

define void @.lilac.byte_buffer_ptr(ptr %in0, ptr %out0) {
entry:
  %0 = call ptr @.lilac.offset(ptr %in0, i64 8)
  %1 = load ptr, ptr %0, align 8
  store ptr %1, ptr %out0, align 8
  ret void
}

define void @.lilac.byte_buffer_len(ptr %in0, ptr %out0) {
entry:
  %0 = load i64, ptr %in0, align 8
  store i64 %0, ptr %out0, align 8
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
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #1

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #2

; Function Attrs: alwaysinline nounwind
define internal ptr @.lilac.offset(ptr %base, i64 %offset) #0 {
entry:
  %address = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %address
}

attributes #0 = { alwaysinline nounwind }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #2 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
