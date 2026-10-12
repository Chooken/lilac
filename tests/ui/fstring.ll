; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [1 x i8] c"x"
@0 = private unnamed_addr constant [1 x i8] c"a"
@1 = private unnamed_addr constant [1 x i8] c"b"
@fmt0 = private unnamed_addr constant <{ i64, i64, ptr, ptr, i64, ptr, ptr }> <{ i64 1, i64 1, ptr @0, ptr @"@format(ReR3OutE)", i64 1, ptr @1, ptr null }>, align 8

define void @"@format(ReR3OutE)"(ptr %in0, ptr %in1) {
entry:
  ret void
}

define void @.lilac.main() {
entry:
  %0 = alloca [8 x i8], align 8
  %part = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %part, i8 0, i64 16, i1 false)
  %tmp.1 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  %msg = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %msg, i8 0, i64 16, i1 false)
  %tmp.2 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.2, i8 0, i64 16, i1 false)
  %1 = call ptr @.lilac.offset(ptr %tmp.1, i64 8)
  store ptr @c0, ptr %1, align 8
  store i64 1, ptr %tmp.1, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %part, ptr align 8 %tmp.1, i64 16, i1 false)
  store ptr %part, ptr %0, align 8
  store ptr @fmt0, ptr %tmp.2, align 8
  %2 = call ptr @.lilac.offset(ptr %tmp.2, i64 8)
  store ptr %0, ptr %2, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %msg, ptr align 8 %tmp.2, i64 16, i1 false)
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
