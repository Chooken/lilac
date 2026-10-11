; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %variable = alloca [8 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %variable, i8 0, i64 8, i1 false)
  %tmp.1 = alloca [8 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 8, i1 false)
  %tmp.2 = alloca [8 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.2, i8 0, i64 8, i1 false)
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 8, i1 false)
  store i8 0, ptr %tmp.1, align 1
  %0 = call ptr @.lilac.offset(ptr %tmp.1, i64 4)
  store i32 0, ptr %0, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %variable, ptr align 4 %tmp.1, i64 8, i1 false)
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.2, i8 0, i64 8, i1 false)
  store i8 1, ptr %tmp.2, align 1
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %variable, ptr align 4 %tmp.2, i64 8, i1 false)
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
