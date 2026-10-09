; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%Object = type { i32 }

define void @Object.get(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (%Object, ptr null, i32 1) to i64), i1 false)
  %0 = load ptr, ptr %in0, align 8
  %1 = load %Object, ptr %0, align 4
  store %Object %1, ptr %out0, align 4
  ret void
}

define void @.lilac.main() {
entry:
  %source = alloca %Object, align 8
  store %Object zeroinitializer, ptr %source, align 4
  %tmp.1 = alloca %Object, align 8
  store %Object zeroinitializer, ptr %tmp.1, align 4
  %result = alloca %Object, align 8
  store %Object zeroinitializer, ptr %result, align 4
  %tmp.2 = alloca %Object, align 8
  store %Object zeroinitializer, ptr %tmp.2, align 4
  %tmp.3 = alloca %Object, align 8
  store %Object zeroinitializer, ptr %tmp.3, align 4
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%Object, ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %Object, ptr %tmp.1, i32 0, i32 0
  store i32 2, ptr %0, align 4
  %1 = load %Object, ptr %tmp.1, align 4
  store %Object %1, ptr %source, align 4
  %2 = load %Object, ptr %source, align 4
  store %Object %2, ptr %tmp.3, align 4
  call void @.root.gen.1.Object(ptr %tmp.3, ptr %tmp.2)
  %3 = load %Object, ptr %tmp.2, align 4
  store %Object %3, ptr %result, align 4
  ret void
}

define void @.root.gen.1.Object(ptr %in0, ptr %out0) {
entry:
  %tmp.1 = alloca %Object, align 8
  store %Object zeroinitializer, ptr %tmp.1, align 4
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (%Object, ptr null, i32 1) to i64), i1 false)
  store ptr %in0, ptr %tmp.2, align 8
  call void @Object.get(ptr %tmp.2, ptr %tmp.1)
  %0 = load %Object, ptr %tmp.1, align 4
  store %Object %0, ptr %out0, align 4
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
