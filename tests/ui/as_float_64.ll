; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [4 x i8] c"5.25"

define void @.lilac.main() {
entry:
  %float = alloca i64, align 8
  store i64 0, ptr %float, align 8
  store i64 4617596992938311680, ptr %float, align 8
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
