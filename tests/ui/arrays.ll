; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%Holder = type { i8, [4 x i32] }

define void @.lilac.main() {
entry:
  %holder = alloca %Holder, align 8
  store %Holder zeroinitializer, ptr %holder, align 4
  %tmp.1 = alloca %Holder, align 8
  store %Holder zeroinitializer, ptr %tmp.1, align 4
  %tmp.2 = alloca [4 x i32], align 4
  store [4 x i32] zeroinitializer, ptr %tmp.2, align 4
  %slot = alloca ptr, align 8
  store ptr null, ptr %slot, align 8
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  %count = alloca i64, align 8
  store i64 0, ptr %count, align 8
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%Holder, ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %Holder, ptr %tmp.1, i32 0, i32 0
  store i8 1, ptr %0, align 1
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.2, i8 0, i64 ptrtoint (ptr getelementptr ([4 x i32], ptr null, i32 1) to i64), i1 false)
  %1 = getelementptr inbounds %Holder, ptr %tmp.1, i32 0, i32 1
  %2 = load [4 x i32], ptr %tmp.2, align 4
  store [4 x i32] %2, ptr %1, align 4
  %3 = load %Holder, ptr %tmp.1, align 4
  store %Holder %3, ptr %holder, align 4
  %4 = getelementptr inbounds %Holder, ptr %holder, i32 0, i32 1
  store ptr %4, ptr %tmp.3, align 8
  %5 = load ptr, ptr %tmp.3, align 8
  %6 = ptrtoint ptr %5 to i64
  %7 = add i64 %6, 8
  %8 = inttoptr i64 %7 to ptr
  store ptr %8, ptr %slot, align 8
  %9 = load ptr, ptr %slot, align 8
  store i32 7, ptr %9, align 4
  store i64 4, ptr %count, align 8
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
