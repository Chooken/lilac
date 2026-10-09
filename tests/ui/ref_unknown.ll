; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%Store = type { ptr }

define ptr @.lilac.ptr_add(ptr %base, i64 %offset) {
entry:
  %0 = ptrtoint ptr %base to i64
  %1 = add i64 %0, %offset
  %2 = inttoptr i64 %1 to ptr
  ret ptr %2
}

declare ptr @malloc(i64)

declare void @free(ptr)

define void @.root.put(ptr %in0, ptr %in1) {
entry:
  %p = alloca ptr, align 8
  store ptr null, ptr %p, align 8
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %tmp.2 = alloca i64, align 8
  store i64 0, ptr %tmp.2, align 8
  %tmp.3 = alloca i8, align 1
  store i8 0, ptr %tmp.3, align 1
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %p, align 8
  %1 = load ptr, ptr %p, align 8
  store ptr %1, ptr %tmp.1, align 8
  store i64 0, ptr %tmp.2, align 8
  %2 = load i8, ptr %in1, align 1
  store i8 %2, ptr %tmp.3, align 1
  call void @.lilac.store_u8(ptr %tmp.1, ptr %tmp.2, ptr %tmp.3)
  ret void
}

define void @.lilac.main() {
entry:
  %raw = alloca ptr, align 8
  store ptr null, ptr %raw, align 8
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %tmp.2 = alloca i64, align 8
  store i64 0, ptr %tmp.2, align 8
  %s = alloca %Store, align 8
  store %Store zeroinitializer, ptr %s, align 8
  %tmp.3 = alloca %Store, align 8
  store %Store zeroinitializer, ptr %tmp.3, align 8
  %tmp.4 = alloca ptr, align 8
  store ptr null, ptr %tmp.4, align 8
  %tmp.5 = alloca i8, align 1
  store i8 0, ptr %tmp.5, align 1
  %tmp.6 = alloca ptr, align 8
  store ptr null, ptr %tmp.6, align 8
  store i64 8, ptr %tmp.2, align 8
  %0 = load i64, ptr %tmp.2, align 8
  %1 = call ptr @malloc(i64 %0)
  store ptr %1, ptr %tmp.1, align 8
  %2 = load ptr, ptr %tmp.1, align 8
  store ptr %2, ptr %raw, align 8
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.3, i8 0, i64 ptrtoint (ptr getelementptr (%Store, ptr null, i32 1) to i64), i1 false)
  %3 = getelementptr inbounds %Store, ptr %tmp.3, i32 0, i32 0
  %4 = load ptr, ptr %raw, align 8
  store ptr %4, ptr %3, align 8
  %5 = load %Store, ptr %tmp.3, align 8
  store %Store %5, ptr %s, align 8
  %6 = getelementptr inbounds %Store, ptr %s, i32 0, i32 0
  %7 = load ptr, ptr %6, align 8
  store ptr %7, ptr %tmp.4, align 8
  store i8 42, ptr %tmp.5, align 1
  call void @.root.put(ptr %tmp.4, ptr %tmp.5)
  %8 = getelementptr inbounds %Store, ptr %s, i32 0, i32 0
  %9 = load ptr, ptr %8, align 8
  store ptr %9, ptr %tmp.6, align 8
  %10 = load ptr, ptr %tmp.6, align 8
  call void @free(ptr %10)
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

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
