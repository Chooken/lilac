; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %a = alloca float, align 4
  store float 0.000000e+00, ptr %a, align 4
  %b = alloca float, align 4
  store float 0.000000e+00, ptr %b, align 4
  %eq = alloca i8, align 1
  store i8 0, ptr %eq, align 1
  store float 5.250000e+00, ptr %a, align 4
  store float 5.250000e+00, ptr %b, align 4
  %0 = load float, ptr %a, align 4
  %1 = load float, ptr %b, align 4
  %2 = bitcast float %0 to i32
  %3 = bitcast float %1 to i32
  %4 = icmp eq i32 %2, %3
  %5 = zext i1 %4 to i8
  store i8 %5, ptr %eq, align 1
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
