; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%GenBox.1..bit32 = type { i32 }

define void @.lilac.main() {
entry:
  %box = alloca %GenBox.1..bit32, align 8
  store %GenBox.1..bit32 zeroinitializer, ptr %box, align 4
  %tmp.1 = alloca %GenBox.1..bit32, align 8
  store %GenBox.1..bit32 zeroinitializer, ptr %tmp.1, align 4
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%GenBox.1..bit32, ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %GenBox.1..bit32, ptr %tmp.1, i32 0, i32 0
  store i32 0, ptr %0, align 4
  %1 = load %GenBox.1..bit32, ptr %tmp.1, align 4
  store %GenBox.1..bit32 %1, ptr %box, align 4
  store ptr %box, ptr %tmp.2, align 8
  call void @"GenBox.1..bit32.@on_drop:ref_GenBox[bit32]"(ptr %tmp.2)
  ret void
}

define void @"GenBox.1..bit32.@on_drop:ref_GenBox[bit32]"(ptr %in0) {
entry:
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %0 = load ptr, ptr %in0, align 8
  %1 = getelementptr inbounds %GenBox.1..bit32, ptr %0, i32 0, i32 0
  store ptr %1, ptr %tmp.1, align 8
  %2 = load ptr, ptr %tmp.1, align 8
  store i32 0, ptr %2, align 4
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
