; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @"b.Thing.@on_drop(RN1b5ThingE)"(ptr %in0) {
entry:
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %tmp.1, align 8
  %1 = load ptr, ptr %tmp.1, align 8
  store i32 0, ptr %1, align 4
  ret void
}

define void @"a.Thing.@on_drop(RN1a5ThingE)"(ptr %in0) {
entry:
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %tmp.1, align 8
  %1 = load ptr, ptr %tmp.1, align 8
  store i32 0, ptr %1, align 4
  ret void
}

define void @.root.b_use() {
entry:
  %x = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %x, i8 0, i64 4, i1 false)
  %tmp.1 = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  %y = alloca i32, align 4
  store i32 0, ptr %y, align 4
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  store i32 2, ptr %tmp.1, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %x, ptr align 4 %tmp.1, i64 4, i1 false)
  %0 = load i32, ptr %x, align 4
  store i32 %0, ptr %y, align 4
  store ptr %x, ptr %tmp.2, align 8
  call void @"b.Thing.@on_drop(RN1b5ThingE)"(ptr %tmp.2)
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
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  store i32 1, ptr %tmp.1, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %x, ptr align 4 %tmp.1, i64 4, i1 false)
  %0 = load i32, ptr %x, align 4
  store i32 %0, ptr %y, align 4
  call void @.root.b_use()
  store ptr %x, ptr %tmp.2, align 8
  call void @"a.Thing.@on_drop(RN1a5ThingE)"(ptr %tmp.2)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #1

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
