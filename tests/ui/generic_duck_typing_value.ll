; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @Object.get(ptr %in0, ptr %out0) {
entry:
  %tmp.1 = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  call void @llvm.memset.p0.i64(ptr align 4 %out0, i8 0, i64 4, i1 false)
  %0 = load ptr, ptr %in0, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %tmp.1, ptr align 4 %0, i64 4, i1 false)
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %out0, ptr align 4 %tmp.1, i64 4, i1 false)
  ret void
}

define void @.lilac.main() {
entry:
  %source = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %source, i8 0, i64 4, i1 false)
  %tmp.1 = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  %result = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %result, i8 0, i64 4, i1 false)
  %tmp.2 = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.2, i8 0, i64 4, i1 false)
  %tmp.3 = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.3, i8 0, i64 4, i1 false)
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  store i32 2, ptr %tmp.1, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %source, ptr align 4 %tmp.1, i64 4, i1 false)
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %tmp.3, ptr align 4 %source, i64 4, i1 false)
  call void @.root.gen.1.Object(ptr %tmp.3, ptr %tmp.2)
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %result, ptr align 4 %tmp.2, i64 4, i1 false)
  ret void
}

define void @.root.gen.1.Object(ptr %in0, ptr %out0) {
entry:
  %tmp.1 = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  call void @llvm.memset.p0.i64(ptr align 4 %out0, i8 0, i64 4, i1 false)
  store ptr %in0, ptr %tmp.2, align 8
  call void @Object.get(ptr %tmp.2, ptr %tmp.1)
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %out0, ptr align 4 %tmp.1, i64 4, i1 false)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #1

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
