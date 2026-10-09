; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%GenObject.1..bit32 = type { i32 }

define void @.lilac.main() {
entry:
  %variable = alloca %GenObject.1..bit32, align 8
  store %GenObject.1..bit32 zeroinitializer, ptr %variable, align 4
  %tmp.1 = alloca %GenObject.1..bit32, align 8
  store %GenObject.1..bit32 zeroinitializer, ptr %tmp.1, align 4
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%GenObject.1..bit32, ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %GenObject.1..bit32, ptr %tmp.1, i32 0, i32 0
  store i32 0, ptr %0, align 4
  %1 = load %GenObject.1..bit32, ptr %tmp.1, align 4
  store %GenObject.1..bit32 %1, ptr %variable, align 4
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
