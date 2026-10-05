; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @add_one(ptr %in0, ptr %out0) {
entry:
  %0 = load i32, ptr %in0, align 4
  %1 = add i32 %0, 1
  store i32 %1, ptr %out0, align 4
  ret void
}

define void @.lilac.main(ptr %in0) {
entry:
  %y = alloca i32, align 4
  store i32 0, ptr %y, align 4
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  call void @add_one(ptr %in0, ptr %tmp.1)
  %0 = load i32, ptr %tmp.1, align 4
  store i32 %0, ptr %y, align 4
  ret void
}

define i32 @main() {
entry:
  %n = alloca i32, align 4
  store i32 0, ptr %n, align 4
  call void @.lilac.main(ptr %n)
  ret i32 0
}
