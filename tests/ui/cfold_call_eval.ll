; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [2 x i8] c"AB"

define void @.lilac.main() {
entry:
  %total = alloca i64, align 8
  store i64 0, ptr %total, align 8
  %tmp.1 = alloca i64, align 8
  store i64 0, ptr %tmp.1, align 8
  %tmp.2 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.2, align 8
  store { i64, ptr } { i64 2, ptr @c0 }, ptr %tmp.2, align 8
  store i64 131, ptr %tmp.1, align 8
  %0 = load i64, ptr %tmp.1, align 8
  store i64 %0, ptr %total, align 8
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
