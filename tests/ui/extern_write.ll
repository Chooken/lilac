; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [3 x i8] c"hi\0A"

declare i64 @write(i32, ptr, i64)

define void @.lilac.main() {
entry:
  %s = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %s, i8 0, i64 16, i1 false)
  %tmp.1 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  %n = alloca i64, align 8
  store i64 0, ptr %n, align 8
  %tmp.2 = alloca i64, align 8
  store i64 0, ptr %tmp.2, align 8
  %tmp.3 = alloca i32, align 4
  store i32 0, ptr %tmp.3, align 4
  %tmp.4 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.4, i8 0, i64 16, i1 false)
  %tmp.5 = alloca ptr, align 8
  store ptr null, ptr %tmp.5, align 8
  %tmp.6 = alloca ptr, align 8
  store ptr null, ptr %tmp.6, align 8
  %tmp.7 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.7, i8 0, i64 16, i1 false)
  %tmp.8 = alloca i64, align 8
  store i64 0, ptr %tmp.8, align 8
  %0 = call ptr @.lilac.offset(ptr %tmp.1, i64 8)
  store ptr @c0, ptr %0, align 8
  store i64 3, ptr %tmp.1, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %s, ptr align 8 %tmp.1, i64 16, i1 false)
  store i32 1, ptr %tmp.3, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %tmp.4, ptr align 8 %s, i64 16, i1 false)
  call void @.lilac.byte_buffer_ptr(ptr %tmp.4, ptr %tmp.5)
  %1 = load ptr, ptr %tmp.5, align 8
  store ptr %1, ptr %tmp.6, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %tmp.7, ptr align 8 %s, i64 16, i1 false)
  call void @.lilac.byte_buffer_len(ptr %tmp.7, ptr %tmp.8)
  %2 = load i32, ptr %tmp.3, align 4
  %3 = load ptr, ptr %tmp.6, align 8
  %4 = load i64, ptr %tmp.8, align 8
  %5 = call i64 @write(i32 %2, ptr %3, i64 %4)
  store i64 %5, ptr %tmp.2, align 8
  %6 = load i64, ptr %tmp.2, align 8
  store i64 %6, ptr %n, align 8
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

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

; Function Attrs: alwaysinline nounwind
define internal ptr @.lilac.offset(ptr %base, i64 %offset) #1 {
entry:
  %address = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %address
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #2

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #1 = { alwaysinline nounwind }
attributes #2 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
