; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %variable = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %variable, i8 0, i64 4, i1 false)
  %tmp.1 = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  %value = alloca i32, align 4
  store i32 0, ptr %value, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  store i32 0, ptr %tmp.1, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %variable, ptr align 4 %tmp.1, i64 4, i1 false)
  store ptr %variable, ptr %tmp.3, align 8
  call void @GenObject.1..bit32.get(ptr %tmp.3, ptr %tmp.2)
  %0 = load i32, ptr %tmp.2, align 4
  store i32 %0, ptr %value, align 4
  ret void
}

define void @GenObject.1..bit32.get(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 4 %out0, i8 0, i64 4, i1 false)
  %0 = load ptr, ptr %in0, align 8
  %1 = load i32, ptr %0, align 4
  store i32 %1, ptr %out0, align 4
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #1

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
