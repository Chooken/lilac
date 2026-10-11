; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [5 x i8] c"hello"

; Function Attrs: alwaysinline nounwind
define ptr @.lilac.ptr_add(ptr %base, i64 %offset) #0 {
entry:
  %address = getelementptr i8, ptr %base, i64 %offset
  ret ptr %address
}

define void @.lilac.main() {
entry:
  %s = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %s, i8 0, i64 16, i1 false)
  %tmp.1 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  %p = alloca ptr, align 8
  store ptr null, ptr %p, align 8
  %tmp.2 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.2, i8 0, i64 16, i1 false)
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  %c = alloca i8, align 1
  store i8 0, ptr %c, align 1
  %tmp.4 = alloca ptr, align 8
  store ptr null, ptr %tmp.4, align 8
  %tmp.5 = alloca i64, align 8
  store i64 0, ptr %tmp.5, align 8
  %tmp.6 = alloca i8, align 1
  store i8 0, ptr %tmp.6, align 1
  %cell = alloca i8, align 1
  store i8 0, ptr %cell, align 1
  %q = alloca ptr, align 8
  store ptr null, ptr %q, align 8
  %tmp.7 = alloca ptr, align 8
  store ptr null, ptr %tmp.7, align 8
  %tmp.8 = alloca i64, align 8
  store i64 0, ptr %tmp.8, align 8
  %tmp.9 = alloca i8, align 1
  store i8 0, ptr %tmp.9, align 1
  %d = alloca i8, align 1
  store i8 0, ptr %d, align 1
  %tmp.10 = alloca ptr, align 8
  store ptr null, ptr %tmp.10, align 8
  %tmp.11 = alloca i64, align 8
  store i64 0, ptr %tmp.11, align 8
  %tmp.12 = alloca i8, align 1
  store i8 0, ptr %tmp.12, align 1
  %0 = call ptr @.lilac.offset(ptr %tmp.1, i64 8)
  store ptr @c0, ptr %0, align 8
  store i64 5, ptr %tmp.1, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %s, ptr align 8 %tmp.1, i64 16, i1 false)
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %tmp.2, ptr align 8 %s, i64 16, i1 false)
  call void @.lilac.byte_buffer_ptr(ptr %tmp.2, ptr %tmp.3)
  %1 = load ptr, ptr %tmp.3, align 8
  store ptr %1, ptr %p, align 8
  %2 = load ptr, ptr %p, align 8
  store ptr %2, ptr %tmp.4, align 8
  store i64 0, ptr %tmp.5, align 8
  call void @.lilac.load_u8(ptr %tmp.4, ptr %tmp.5, ptr %tmp.6)
  %3 = load i8, ptr %tmp.6, align 1
  store i8 %3, ptr %c, align 1
  store i8 0, ptr %cell, align 1
  store ptr %cell, ptr %q, align 8
  %4 = load ptr, ptr %q, align 8
  store ptr %4, ptr %tmp.7, align 8
  store i64 0, ptr %tmp.8, align 8
  %5 = load i8, ptr %c, align 1
  store i8 %5, ptr %tmp.9, align 1
  call void @.lilac.store_u8(ptr %tmp.7, ptr %tmp.8, ptr %tmp.9)
  %6 = load ptr, ptr %q, align 8
  store ptr %6, ptr %tmp.10, align 8
  store i64 0, ptr %tmp.11, align 8
  call void @.lilac.load_u8(ptr %tmp.10, ptr %tmp.11, ptr %tmp.12)
  %7 = load i8, ptr %tmp.12, align 1
  store i8 %7, ptr %d, align 1
  ret void
}

define void @.lilac.byte_buffer_ptr(ptr %in0, ptr %out0) {
entry:
  %0 = call ptr @.lilac.offset(ptr %in0, i64 8)
  %1 = load ptr, ptr %0, align 8
  store ptr %1, ptr %out0, align 8
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

define void @.lilac.store_u8(ptr %in0, ptr %in1, ptr %in2) {
entry:
  %slot = alloca ptr, align 8
  store ptr null, ptr %slot, align 8
  %0 = load ptr, ptr %in0, align 8
  %1 = load i64, ptr %in1, align 8
  %2 = call ptr @.lilac.ptr_add(ptr %0, i64 %1)
  store ptr %2, ptr %slot, align 8
  %3 = load ptr, ptr %slot, align 8
  %4 = load i8, ptr %in2, align 1
  store i8 %4, ptr %3, align 1
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #1

; Function Attrs: alwaysinline nounwind
define internal ptr @.lilac.offset(ptr %base, i64 %offset) #0 {
entry:
  %address = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %address
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #2

attributes #0 = { alwaysinline nounwind }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #2 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
