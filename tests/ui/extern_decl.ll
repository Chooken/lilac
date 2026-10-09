; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [2 x i8] c"hi"

declare i64 @write(i32, ptr, i64)

define void @.lilac.main() {
entry:
  %s = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %s, align 8
  %n = alloca i64, align 8
  store i64 0, ptr %n, align 8
  %tmp.1 = alloca i64, align 8
  store i64 0, ptr %tmp.1, align 8
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.3, align 8
  %tmp.4 = alloca ptr, align 8
  store ptr null, ptr %tmp.4, align 8
  %tmp.5 = alloca ptr, align 8
  store ptr null, ptr %tmp.5, align 8
  %tmp.6 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.6, align 8
  %tmp.7 = alloca i64, align 8
  store i64 0, ptr %tmp.7, align 8
  store { i64, ptr } { i64 2, ptr @c0 }, ptr %s, align 8
  store i32 1, ptr %tmp.2, align 4
  %0 = load { i64, ptr }, ptr %s, align 8
  store { i64, ptr } %0, ptr %tmp.3, align 8
  call void @.lilac.byte_buffer_ptr(ptr %tmp.3, ptr %tmp.4)
  %1 = load ptr, ptr %tmp.4, align 8
  store ptr %1, ptr %tmp.5, align 8
  %2 = load { i64, ptr }, ptr %s, align 8
  store { i64, ptr } %2, ptr %tmp.6, align 8
  call void @.lilac.byte_buffer_len(ptr %tmp.6, ptr %tmp.7)
  %3 = load i32, ptr %tmp.2, align 4
  %4 = load ptr, ptr %tmp.5, align 8
  %5 = load i64, ptr %tmp.7, align 8
  %6 = call i64 @write(i32 %3, ptr %4, i64 %5)
  store i64 %6, ptr %tmp.1, align 8
  %7 = load i64, ptr %tmp.1, align 8
  store i64 %7, ptr %n, align 8
  ret void
}

define void @.lilac.byte_buffer_ptr(ptr %in0, ptr %out0) {
entry:
  %0 = getelementptr inbounds { i64, ptr }, ptr %in0, i32 0, i32 1
  %1 = load ptr, ptr %0, align 8
  store ptr %1, ptr %out0, align 8
  ret void
}

define void @.lilac.byte_buffer_len(ptr %in0, ptr %out0) {
entry:
  %0 = getelementptr inbounds { i64, ptr }, ptr %in0, i32 0, i32 0
  %1 = load i64, ptr %0, align 8
  store i64 %1, ptr %out0, align 8
  ret void
}
