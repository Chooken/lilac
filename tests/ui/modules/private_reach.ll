; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define ptr @.lilac.ptr_add(ptr %base, i64 %offset) {
entry:
  %0 = ptrtoint ptr %base to i64
  %1 = add i64 %0, %offset
  %2 = inttoptr i64 %1 to ptr
  ret ptr %2
}

define void @root_bump(ptr %in0) {
entry:
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %tmp.2 = alloca i64, align 8
  store i64 0, ptr %tmp.2, align 8
  %tmp.3 = alloca i8, align 1
  store i8 0, ptr %tmp.3, align 1
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %tmp.1, align 8
  store i64 0, ptr %tmp.2, align 8
  store i8 1, ptr %tmp.3, align 1
  call void @.lilac.store_u8(ptr %tmp.1, ptr %tmp.2, ptr %tmp.3)
  ret void
}

define void @.lilac.main() {
entry:
  %cell = alloca i8, align 1
  store i8 0, ptr %cell, align 1
  %p = alloca ptr, align 8
  store ptr null, ptr %p, align 8
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  store i8 0, ptr %cell, align 1
  store ptr %cell, ptr %p, align 8
  %0 = load ptr, ptr %p, align 8
  store ptr %0, ptr %tmp.1, align 8
  call void @util.write_byte(ptr %tmp.1)
  ret void
}

define void @util.secret(ptr %in0) {
entry:
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %tmp.2 = alloca i64, align 8
  store i64 0, ptr %tmp.2, align 8
  %tmp.3 = alloca i8, align 1
  store i8 0, ptr %tmp.3, align 1
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %tmp.1, align 8
  store i64 0, ptr %tmp.2, align 8
  store i8 9, ptr %tmp.3, align 1
  call void @.lilac.store_u8(ptr %tmp.1, ptr %tmp.2, ptr %tmp.3)
  ret void
}

define void @util.write_byte(ptr %in0) {
entry:
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %tmp.1, align 8
  call void @root_bump(ptr %tmp.1)
  %1 = load ptr, ptr %in0, align 8
  store ptr %1, ptr %tmp.2, align 8
  call void @util.secret(ptr %tmp.2)
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
