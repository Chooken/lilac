; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %number = alloca [1 x i8], align 1
  call void @llvm.memset.p0.i64(ptr align 1 %number, i8 0, i64 1, i1 false)
  %tmp.1 = alloca [1 x i8], align 1
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 1, i1 false)
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 1, i1 false)
  store i8 0, ptr %tmp.1, align 1
  call void @llvm.memcpy.p0.p0.i64(ptr align 1 %number, ptr align 1 %tmp.1, i64 1, i1 false)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #1

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
