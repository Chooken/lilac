; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %int_exp = alloca double, align 8
  store double 0.000000e+00, ptr %int_exp, align 8
  %big_exp = alloca double, align 8
  store double 0.000000e+00, ptr %big_exp, align 8
  %frac = alloca double, align 8
  store double 0.000000e+00, ptr %frac, align 8
  %single = alloca float, align 4
  store float 0.000000e+00, ptr %single, align 4
  store double 1.000000e+03, ptr %int_exp, align 8
  store double 2.000000e+10, ptr %big_exp, align 8
  store double 1.500000e-04, ptr %frac, align 8
  store float 0x40091EB860000000, ptr %single, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
