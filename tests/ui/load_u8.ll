; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [5 x i8] c"hello"

define ptr @.lilac.ptr_add(ptr %base, i64 %offset) {
entry:
  %0 = ptrtoint ptr %base to i64
  %1 = add i64 %0, %offset
  %2 = inttoptr i64 %1 to ptr
  ret ptr %2
}

define void @.lilac.main() {
entry:
  %s = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %s, align 8
  %p = alloca ptr, align 8
  store ptr null, ptr %p, align 8
  %tmp.1 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.1, align 8
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  %c = alloca i8, align 1
  store i8 0, ptr %c, align 1
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  %tmp.4 = alloca i64, align 8
  store i64 0, ptr %tmp.4, align 8
  %tmp.5 = alloca i8, align 1
  store i8 0, ptr %tmp.5, align 1
  %cell = alloca i8, align 1
  store i8 0, ptr %cell, align 1
  %q = alloca ptr, align 8
  store ptr null, ptr %q, align 8
  %tmp.6 = alloca ptr, align 8
  store ptr null, ptr %tmp.6, align 8
  %tmp.7 = alloca i64, align 8
  store i64 0, ptr %tmp.7, align 8
  %tmp.8 = alloca i8, align 1
  store i8 0, ptr %tmp.8, align 1
  %d = alloca i8, align 1
  store i8 0, ptr %d, align 1
  %tmp.9 = alloca ptr, align 8
  store ptr null, ptr %tmp.9, align 8
  %tmp.10 = alloca i64, align 8
  store i64 0, ptr %tmp.10, align 8
  %tmp.11 = alloca i8, align 1
  store i8 0, ptr %tmp.11, align 1
  store { i64, ptr } { i64 5, ptr @c0 }, ptr %s, align 8
  %0 = load { i64, ptr }, ptr %s, align 8
  store { i64, ptr } %0, ptr %tmp.1, align 8
  call void @.lilac.byte_buffer_ptr(ptr %tmp.1, ptr %tmp.2)
  %1 = load ptr, ptr %tmp.2, align 8
  store ptr %1, ptr %p, align 8
  %2 = load ptr, ptr %p, align 8
  store ptr %2, ptr %tmp.3, align 8
  store i64 0, ptr %tmp.4, align 8
  call void @.lilac.load_u8(ptr %tmp.3, ptr %tmp.4, ptr %tmp.5)
  %3 = load i8, ptr %tmp.5, align 1
  store i8 %3, ptr %c, align 1
  store i8 0, ptr %cell, align 1
  store ptr %cell, ptr %q, align 8
  %4 = load ptr, ptr %q, align 8
  store ptr %4, ptr %tmp.6, align 8
  store i64 0, ptr %tmp.7, align 8
  %5 = load i8, ptr %c, align 1
  store i8 %5, ptr %tmp.8, align 1
  call void @.lilac.store_u8(ptr %tmp.6, ptr %tmp.7, ptr %tmp.8)
  %6 = load ptr, ptr %q, align 8
  store ptr %6, ptr %tmp.9, align 8
  store i64 0, ptr %tmp.10, align 8
  call void @.lilac.load_u8(ptr %tmp.9, ptr %tmp.10, ptr %tmp.11)
  %7 = load i8, ptr %tmp.11, align 1
  store i8 %7, ptr %d, align 1
  ret void
}

define void @.lilac.byte_buffer_ptr(ptr %in0, ptr %out0) {
entry:
  %0 = getelementptr inbounds { i64, ptr }, ptr %in0, i32 0, i32 1
  %1 = load ptr, ptr %0, align 8
  store ptr %1, ptr %out0, align 8
  ret void
}

define void @.lilac.load_u8(ptr %in0, ptr %in1, ptr %out0) {
entry:
  %slot = alloca ptr, align 8
  store ptr null, ptr %slot, align 8
  %0 = load ptr, ptr %in0, align 8
  %1 = load i64, ptr %in1, align 8
  %2 = call ptr @.lilac.ptr_add(ptr %0, i64 %1)
  store ptr %2, ptr %slot, align 8
  %3 = load ptr, ptr %slot, align 8
  %4 = load i8, ptr %3, align 1
  store i8 %4, ptr %out0, align 1
  ret void
}

define void @.lilac.store_u8(ptr %in0, ptr %in1, ptr %in2) {
entry:
  %slot = alloca ptr, align 8
  store ptr null, ptr %slot, align 8
  %0 = load ptr, ptr %in0, align 8
  %1 = load i64, ptr %in1, align 8
  %2 = call ptr @.lilac.ptr_add(ptr %0, i64 %1)
  store ptr %2, ptr %slot, align 8
  %3 = load ptr, ptr %slot, align 8
  %4 = load i8, ptr %in2, align 1
  store i8 %4, ptr %3, align 1
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
