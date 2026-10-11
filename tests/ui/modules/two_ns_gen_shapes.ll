; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.root.b_use() {
entry:
  %x = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %x, i8 0, i64 16, i1 false)
  %tmp.1 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  %y = alloca i64, align 8
  store i64 0, ptr %y, align 8
  %m = alloca i32, align 4
  store i32 0, ptr %m, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca i64, align 8
  store i64 0, ptr %tmp.3, align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  store i64 2, ptr %tmp.1, align 8
  %0 = call ptr @.lilac.offset(ptr %tmp.1, i64 8)
  store i64 3, ptr %0, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %x, ptr align 8 %tmp.1, i64 16, i1 false)
  %1 = load i64, ptr %x, align 8
  store i64 %1, ptr %y, align 8
  store i64 0, ptr %tmp.3, align 8
  call void @b.tag.1..bit64(ptr %tmp.3, ptr %tmp.2)
  %2 = load i32, ptr %tmp.2, align 4
  store i32 %2, ptr %m, align 4
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
  %n = alloca i32, align 4
  store i32 0, ptr %n, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca i32, align 4
  store i32 0, ptr %tmp.3, align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 4, i1 false)
  store i32 1, ptr %tmp.1, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %x, ptr align 4 %tmp.1, i64 4, i1 false)
  %0 = load i32, ptr %x, align 4
  store i32 %0, ptr %y, align 4
  store i32 0, ptr %tmp.3, align 4
  call void @a.tag.1..bit32(ptr %tmp.3, ptr %tmp.2)
  %1 = load i32, ptr %tmp.2, align 4
  store i32 %1, ptr %n, align 4
  call void @.root.b_use()
  ret void
}

define void @b.tag.1..bit64(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 4 %out0, i8 0, i64 4, i1 false)
  store i32 2, ptr %out0, align 4
  ret void
}

define void @a.tag.1..bit32(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 4 %out0, i8 0, i64 4, i1 false)
  store i32 1, ptr %out0, align 4
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

; Function Attrs: alwaysinline nounwind
define internal ptr @.lilac.offset(ptr %base, i64 %offset) #1 {
entry:
  %address = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %address
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #2

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #1 = { alwaysinline nounwind }
attributes #2 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
