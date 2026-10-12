; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.memmove(ptr %0, ptr %1, i64 %2) {
entry:
  call void @llvm.memmove.p0.p0.i64(ptr align 1 %0, ptr align 1 %1, i64 %2, i1 false)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memmove.p0.p0.i64(ptr nocapture writeonly, ptr nocapture readonly, i64, i1 immarg) #0

define void @.lilac.memset(ptr %0, i8 %1, i64 %2) {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %0, i8 %1, i64 %2, i1 false)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #1

; Function Attrs: alwaysinline nounwind
define ptr @.lilac.ptr_add(ptr %base, i64 %offset) #2 {
entry:
  %address = getelementptr i8, ptr %base, i64 %offset
  ret ptr %address
}

; Function Attrs: cold noreturn nounwind memory(inaccessiblemem: write)
declare void @llvm.trap() #3

define void @.lilac.abort() {
entry:
  call void @llvm.trap()
  unreachable
}

define void @.lilac.main() {
entry:
  %pair = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %pair, i8 0, i64 16, i1 false)
  %tmp.1 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  %base = alloca ptr, align 8
  store ptr null, ptr %base, align 8
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  %tmp.3 = alloca i64, align 8
  store i64 0, ptr %tmp.3, align 8
  %tmp.4 = alloca ptr, align 8
  store ptr null, ptr %tmp.4, align 8
  %tmp.5 = alloca ptr, align 8
  store ptr null, ptr %tmp.5, align 8
  %tmp.6 = alloca i64, align 8
  store i64 0, ptr %tmp.6, align 8
  %tmp.7 = alloca ptr, align 8
  store ptr null, ptr %tmp.7, align 8
  %tmp.8 = alloca i8, align 1
  store i8 0, ptr %tmp.8, align 1
  %tmp.9 = alloca i64, align 8
  store i64 0, ptr %tmp.9, align 8
  %address = alloca i64, align 8
  store i64 0, ptr %address, align 8
  %again = alloca ptr, align 8
  store ptr null, ptr %again, align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  store i64 506097522914230528, ptr %tmp.1, align 8
  %0 = call ptr @.lilac.offset(ptr %tmp.1, i64 8)
  store i64 0, ptr %0, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %pair, ptr align 8 %tmp.1, i64 16, i1 false)
  store ptr %pair, ptr %base, align 8
  %1 = load ptr, ptr %base, align 8
  store ptr %1, ptr %tmp.2, align 8
  store i64 2, ptr %tmp.3, align 8
  %2 = load ptr, ptr %tmp.2, align 8
  %3 = load i64, ptr %tmp.3, align 8
  %4 = call ptr @.lilac.ptr_add(ptr %2, i64 %3)
  store ptr %4, ptr %tmp.4, align 8
  %5 = load ptr, ptr %base, align 8
  store ptr %5, ptr %tmp.5, align 8
  store i64 8, ptr %tmp.6, align 8
  %6 = load ptr, ptr %tmp.4, align 8
  %7 = load ptr, ptr %tmp.5, align 8
  %8 = load i64, ptr %tmp.6, align 8
  call void @.lilac.memmove(ptr %6, ptr %7, i64 %8)
  %9 = load ptr, ptr %base, align 8
  store ptr %9, ptr %tmp.7, align 8
  store i8 0, ptr %tmp.8, align 1
  store i64 8, ptr %tmp.9, align 8
  %10 = load ptr, ptr %tmp.7, align 8
  %11 = load i8, ptr %tmp.8, align 1
  %12 = load i64, ptr %tmp.9, align 8
  call void @.lilac.memset(ptr %10, i8 %11, i64 %12)
  %13 = load ptr, ptr %base, align 8
  %14 = ptrtoint ptr %13 to i64
  store i64 %14, ptr %address, align 8
  %15 = load i64, ptr %address, align 8
  %16 = inttoptr i64 %15 to ptr
  store ptr %16, ptr %again, align 8
  %17 = load ptr, ptr %again, align 8
  %18 = ptrtoint ptr %17 to i64
  %19 = load i64, ptr %address, align 8
  %20 = icmp ult i64 %18, %19
  %21 = zext i1 %20 to i8
  %22 = icmp ne i8 %21, 0
  br i1 %22, label %l.1, label %l.2

l.1:                                              ; preds = %entry
  call void @.lilac.abort()
  unreachable

l.2:                                              ; preds = %entry
  br label %l.3

l.3:                                              ; preds = %l.2
  ret void
}

; Function Attrs: alwaysinline nounwind
define internal ptr @.lilac.offset(ptr %base, i64 %offset) #2 {
entry:
  %address = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %address
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #0

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #2 = { alwaysinline nounwind }
attributes #3 = { cold noreturn nounwind memory(inaccessiblemem: write) }
