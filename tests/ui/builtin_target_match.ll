; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%"@builtin.Os" = type { i8 }

define void @.root.on_bare() {
entry:
  ret void
}

define void @.root.with_libc() {
entry:
  ret void
}

define void @.lilac.main() {
entry:
  %os = alloca %"@builtin.Os", align 8
  store %"@builtin.Os" zeroinitializer, ptr %os, align 1
  %tmp.1 = alloca %"@builtin.Os", align 8
  store %"@builtin.Os" zeroinitializer, ptr %tmp.1, align 1
  call void @.root.on_bare()
  call void @.root.with_libc()
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%"@builtin.Os", ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %"@builtin.Os", ptr %tmp.1, i32 0, i32 0
  store i8 3, ptr %0, align 1
  %1 = load %"@builtin.Os", ptr %tmp.1, align 1
  store %"@builtin.Os" %1, ptr %os, align 1
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
