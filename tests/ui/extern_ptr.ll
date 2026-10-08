; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

declare ptr @malloc(i64)

declare void @free(ptr)

define void @.lilac.main() {
entry:
  %p = alloca ptr, align 8
  store ptr null, ptr %p, align 8
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %tmp.2 = alloca i64, align 8
  store i64 0, ptr %tmp.2, align 8
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  store i64 16, ptr %tmp.2, align 8
  %0 = load i64, ptr %tmp.2, align 8
  %1 = call ptr @malloc(i64 %0)
  store ptr %1, ptr %tmp.1, align 8
  %2 = load ptr, ptr %tmp.1, align 8
  store ptr %2, ptr %p, align 8
  %3 = load ptr, ptr %p, align 8
  store ptr %3, ptr %tmp.3, align 8
  %4 = load ptr, ptr %tmp.3, align 8
  call void @free(ptr %4)
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
