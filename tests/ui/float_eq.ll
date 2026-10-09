; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

; Function Attrs: alwaysinline
define void @.root.f(ptr %in0, ptr %in1, ptr %out0) #0 {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (i8, ptr null, i32 1) to i64), i1 false)
  %0 = load float, ptr %in0, align 4
  %1 = load float, ptr %in1, align 4
  %2 = bitcast float %0 to i32
  %3 = bitcast float %1 to i32
  %4 = icmp eq i32 %2, %3
  %5 = zext i1 %4 to i8
  store i8 %5, ptr %out0, align 1
  ret void
}

define void @.lilac.main() {
entry:
  %a = alloca float, align 4
  store float 0.000000e+00, ptr %a, align 4
  %b = alloca float, align 4
  store float 0.000000e+00, ptr %b, align 4
  %eq = alloca i8, align 1
  store i8 0, ptr %eq, align 1
  %tmp.1 = alloca i8, align 1
  store i8 0, ptr %tmp.1, align 1
  %tmp.2 = alloca float, align 4
  store float 0.000000e+00, ptr %tmp.2, align 4
  %tmp.3 = alloca float, align 4
  store float 0.000000e+00, ptr %tmp.3, align 4
  store float 5.250000e+00, ptr %a, align 4
  store float 5.250000e+00, ptr %b, align 4
  %0 = load float, ptr %a, align 4
  store float %0, ptr %tmp.2, align 4
  %1 = load float, ptr %b, align 4
  store float %1, ptr %tmp.3, align 4
  call void @.root.f(ptr %tmp.2, ptr %tmp.3, ptr %tmp.1)
  %2 = load i8, ptr %tmp.1, align 1
  store i8 %2, ptr %eq, align 1
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #1

attributes #0 = { alwaysinline }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: write) }
