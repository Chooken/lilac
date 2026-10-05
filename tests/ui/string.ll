; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [5 x i8] c"hello"

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

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
attributes #1 = { cold noreturn nounwind memory(inaccessiblemem: write) }
