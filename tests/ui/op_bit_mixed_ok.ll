; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %a = alloca i8, align 1
  store i8 0, ptr %a, align 1
  %b = alloca i32, align 4
  store i32 0, ptr %b, align 4
  %c = alloca i32, align 4
  store i32 0, ptr %c, align 4
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  store i8 1, ptr %a, align 1
  store i32 2, ptr %b, align 4
  store i32 0, ptr %tmp.1, align 4
  %0 = load i32, ptr %tmp.1, align 4
  store i32 %0, ptr %c, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
