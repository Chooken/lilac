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
  %__m0 = alloca i32, align 4
  store i32 0, ptr %__m0, align 4
  %__m1 = alloca i32, align 4
  store i32 0, ptr %__m1, align 4
  %condition = alloca i8, align 1
  store i8 0, ptr %condition, align 1
  %tmp.3 = alloca i8, align 1
  store i8 0, ptr %tmp.3, align 1
  %return_val = alloca i32, align 4
  store i32 0, ptr %return_val, align 4
  store i32 1, ptr %tmp.1, align 4
  store i32 2, ptr %tmp.2, align 4
  %0 = load i32, ptr %tmp.1, align 4
  store i32 %0, ptr %__m0, align 4
  %1 = load i32, ptr %tmp.2, align 4
  store i32 %1, ptr %__m1, align 4
  store i8 1, ptr %tmp.3, align 1
  %2 = load i8, ptr %tmp.3, align 1
  store i8 %2, ptr %condition, align 1
  %3 = load i32, ptr %__m1, align 4
  store i32 %3, ptr %return_val, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
