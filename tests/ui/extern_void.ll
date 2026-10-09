; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

declare void @srand(i32)

define void @.lilac.main() {
entry:
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  store i32 1, ptr %tmp.1, align 4
  %0 = load i32, ptr %tmp.1, align 4
  call void @srand(i32 %0)
  ret void
}
