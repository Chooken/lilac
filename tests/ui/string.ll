; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [5 x i8] c"hello"

define void @.lilac.main() {
entry:
  %s = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %s, align 8
  %n = alloca i64, align 8
  store i64 0, ptr %n, align 8
  %tmp.1 = alloca i64, align 8
  store i64 0, ptr %tmp.1, align 8
  %p = alloca ptr, align 8
  store ptr null, ptr %p, align 8
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  store { i64, ptr } { i64 5, ptr @c0 }, ptr %s, align 8
  store i64 5, ptr %tmp.1, align 8
  %0 = load i64, ptr %tmp.1, align 8
  store i64 %0, ptr %n, align 8
  call void @.lilac.byte_buffer_ptr(ptr %s, ptr %tmp.2)
  %1 = load ptr, ptr %tmp.2, align 8
  store ptr %1, ptr %p, align 8
  ret void
}

define void @.lilac.byte_buffer_ptr(ptr %in0, ptr %out0) {
entry:
  %0 = getelementptr inbounds { i64, ptr }, ptr %in0, i32 0, i32 1
  %1 = load ptr, ptr %0, align 8
  store ptr %1, ptr %out0, align 8
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
