; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %add = alloca ptr, align 8
  store ptr null, ptr %add, align 8
  %value = alloca i32, align 4
  store i32 0, ptr %value, align 4
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca i32, align 4
  store i32 0, ptr %tmp.3, align 4
  %tmp.4 = alloca i32, align 4
  store i32 0, ptr %tmp.4, align 4
  store ptr @addOne, ptr %add, align 8
  store i32 0, ptr %value, align 4
  %0 = load i32, ptr %value, align 4
  store i32 %0, ptr %tmp.2, align 4
  %1 = load ptr, ptr %add, align 8
  call void %1(ptr %tmp.2, ptr %tmp.1)
  %2 = load i32, ptr %tmp.1, align 4
  store i32 %2, ptr %value, align 4
  store ptr @addTwo, ptr %add, align 8
  %3 = load i32, ptr %value, align 4
  store i32 %3, ptr %tmp.4, align 4
  %4 = load ptr, ptr %add, align 8
  call void %4(ptr %tmp.4, ptr %tmp.3)
  %5 = load i32, ptr %tmp.3, align 4
  store i32 %5, ptr %value, align 4
  ret void
}

define void @addOne(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (i32, ptr null, i32 1) to i64), i1 false)
  %0 = load i32, ptr %in0, align 4
  %1 = add i32 %0, 1
  store i32 %1, ptr %out0, align 4
  ret void
}

define void @addTwo(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (i32, ptr null, i32 1) to i64), i1 false)
  %0 = load i32, ptr %in0, align 4
  %1 = add i32 %0, 2
  store i32 %1, ptr %out0, align 4
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
