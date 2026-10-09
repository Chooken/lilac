; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @"@conversion:bit32:bit8"(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (i8, ptr null, i32 1) to i64), i1 false)
  %0 = load i32, ptr %in0, align 4
  %1 = trunc i32 %0 to i8
  store i8 %1, ptr %out0, align 1
  ret void
}

define void @.lilac.main() {
entry:
  %a = alloca i32, align 4
  store i32 0, ptr %a, align 4
  %b = alloca i8, align 1
  store i8 0, ptr %b, align 1
  %tmp.1 = alloca i8, align 1
  store i8 0, ptr %tmp.1, align 1
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  store i32 1, ptr %a, align 4
  %0 = load i32, ptr %a, align 4
  store i32 %0, ptr %tmp.2, align 4
  call void @"@conversion:bit32:bit8"(ptr %tmp.2, ptr %tmp.1)
  %1 = load i8, ptr %tmp.1, align 1
  store i8 %1, ptr %b, align 1
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
