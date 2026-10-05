; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %variable = alloca i32, align 4
  store i32 0, ptr %variable, align 4
  %ref_to_variable = alloca ptr, align 8
  store ptr null, ptr %ref_to_variable, align 8
  store i32 0, ptr %variable, align 4
  store ptr %variable, ptr %ref_to_variable, align 8
  %0 = load ptr, ptr %ref_to_variable, align 8
  store i32 1, ptr %0, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
