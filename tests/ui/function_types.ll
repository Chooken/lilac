; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %add = alloca i64, align 8
  store i64 0, ptr %add, align 8
  %value = alloca i32, align 4
  store i32 0, ptr %value, align 4
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  store i64 ptrtoint (ptr @addOne to i64), ptr %add, align 8
  store i32 0, ptr %value, align 4
  %0 = load i64, ptr %add, align 8
  %1 = inttoptr i64 %0 to ptr
  call void %1(ptr %value, ptr %tmp.1)
  %2 = load i32, ptr %tmp.1, align 4
  store i32 %2, ptr %value, align 4
  store i64 ptrtoint (ptr @addTwo to i64), ptr %add, align 8
  %3 = load i64, ptr %add, align 8
  %4 = inttoptr i64 %3 to ptr
  call void %4(ptr %value, ptr %tmp.2)
  %5 = load i32, ptr %tmp.2, align 4
  store i32 %5, ptr %value, align 4
  ret void
}

define void @addOne(ptr %in0, ptr %out0) {
entry:
  %0 = load i32, ptr %in0, align 4
  %1 = add i32 %0, 1
  store i32 %1, ptr %out0, align 4
  ret void
}

define void @addTwo(ptr %in0, ptr %out0) {
entry:
  %0 = load i32, ptr %in0, align 4
  %1 = add i32 %0, 2
  store i32 %1, ptr %out0, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
