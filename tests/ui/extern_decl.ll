; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [2 x i8] c"hi"

declare void @puts(ptr)

define void @.lilac.main() {
entry:
  %tmp.1 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.1, align 8
  store { i64, ptr } { i64 2, ptr @c0 }, ptr %tmp.1, align 8
  call void @puts(ptr %tmp.1)
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
