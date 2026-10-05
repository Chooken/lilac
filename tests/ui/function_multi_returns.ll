; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %condition = alloca i32, align 4
  store i32 0, ptr %condition, align 4
  %result = alloca i32, align 4
  store i32 0, ptr %result, align 4
  store i32 1, ptr %tmp.1, align 4
  store i32 2, ptr %tmp.2, align 4
  %0 = load i32, ptr %tmp.1, align 4
  store i32 %0, ptr %condition, align 4
  %1 = load i32, ptr %tmp.2, align 4
  store i32 %1, ptr %result, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
