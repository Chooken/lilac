; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @Object.get(ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (i32, ptr null, i32 1) to i64), i1 false)
  store i32 2, ptr %out0, align 4
  ret void
}

define void @.lilac.main() {
entry:
  %result = alloca i32, align 4
  store i32 0, ptr %result, align 4
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  call void @gen.1.Object(ptr %tmp.1)
  %0 = load i32, ptr %tmp.1, align 4
  store i32 %0, ptr %result, align 4
  ret void
}

define void @gen.1.Object(ptr %out0) {
entry:
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (i32, ptr null, i32 1) to i64), i1 false)
  call void @Object.get(ptr %tmp.1)
  %0 = load i32, ptr %tmp.1, align 4
  store i32 %0, ptr %out0, align 4
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
