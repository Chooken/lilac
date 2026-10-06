; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

declare i64 @write(i32, ptr, i64)

define void @.lilac.main() {
entry:
  %w = alloca i64, align 8
  store i64 0, ptr %w, align 8
  store i64 ptrtoint (ptr @write to i64), ptr %w, align 8
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
