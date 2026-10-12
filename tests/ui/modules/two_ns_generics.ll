; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.root.b_use() {
entry:
  %x = alloca [8 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %x, i8 0, i64 8, i1 false)
  %tmp.1 = alloca [8 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 8, i1 false)
  %y = alloca i64, align 8
  store i64 0, ptr %y, align 8
  %tmp.2 = alloca i64, align 8
  store i64 0, ptr %tmp.2, align 8
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 8, i1 false)
  store i64 2, ptr %tmp.1, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %x, ptr align 8 %tmp.1, i64 8, i1 false)
  store ptr %x, ptr %tmp.3, align 8
  call void @"b.GenBox[y].get"(ptr %tmp.3, ptr %tmp.2)
  %0 = load i64, ptr %tmp.2, align 8
  store i64 %0, ptr %y, align 8
  ret void
}

define void @.lilac.main() {
entry:
  %x = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %x, i8 0, i64 4, i1 false)
  %tmp.1 = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  %y = alloca i32, align 4
  store i32 0, ptr %y, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  store i32 1, ptr %tmp.1, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %x, ptr align 4 %tmp.1, i64 4, i1 false)
  store ptr %x, ptr %tmp.3, align 8
  call void @"a.GenBox[m].get"(ptr %tmp.3, ptr %tmp.2)
  %0 = load i32, ptr %tmp.2, align 4
  store i32 %0, ptr %y, align 4
  call void @.root.b_use()
  ret void
}

define void @"b.GenBox[y].get"(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 8 %out0, i8 0, i64 8, i1 false)
  %0 = load ptr, ptr %in0, align 8
  %1 = load i64, ptr %0, align 8
  store i64 %1, ptr %out0, align 8
  ret void
}

define void @"a.GenBox[m].get"(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 4 %out0, i8 0, i64 4, i1 false)
  %0 = load ptr, ptr %in0, align 8
  %1 = load i32, ptr %0, align 4
  store i32 %1, ptr %out0, align 4
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #1

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
