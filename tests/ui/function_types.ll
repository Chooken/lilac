; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.memcpy(ptr %dest, ptr %src, i64 %len) {
entry:
  call void @llvm.memcpy.p0.p0.i64(ptr align 1 %dest, ptr align 1 %src, i64 %len, i1 false)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #0

define ptr @.lilac.ptr_add(ptr %base, i64 %offset) {
entry:
  %0 = ptrtoint ptr %base to i64
  %1 = add i64 %0, %offset
  %2 = inttoptr i64 %1 to ptr
  ret ptr %2
}

; Function Attrs: cold noreturn nounwind memory(inaccessiblemem: write)
declare void @llvm.trap() #1

define void @.lilac.abort() {
entry:
  call void @llvm.trap()
  unreachable
}

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

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
attributes #1 = { cold noreturn nounwind memory(inaccessiblemem: write) }
