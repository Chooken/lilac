; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

; Function Attrs: alwaysinline
define void @.root.addOne(ptr %in0, ptr %out0) #0 {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (i32, ptr null, i32 1) to i64), i1 false)
  %0 = load i32, ptr %in0, align 4
  %1 = add i32 %0, 1
  store i32 %1, ptr %out0, align 4
  ret void
}

define void @.lilac.main() {
entry:
  %variable = alloca i32, align 4
  store i32 0, ptr %variable, align 4
  %result = alloca i32, align 4
  store i32 0, ptr %result, align 4
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  store i32 2, ptr %variable, align 4
  %0 = load i32, ptr %variable, align 4
  store i32 %0, ptr %tmp.2, align 4
  call void @.root.addOne(ptr %tmp.2, ptr %tmp.1)
  %1 = load i32, ptr %tmp.1, align 4
  store i32 %1, ptr %result, align 4
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #1

attributes #0 = { alwaysinline }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: write) }
