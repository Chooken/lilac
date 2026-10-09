; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%b.GenBox.1..bit64 = type { i64, i64 }
%a.GenBox.1..bit32 = type { i32 }

define void @.root.b_use() {
entry:
  %x = alloca %b.GenBox.1..bit64, align 8
  store %b.GenBox.1..bit64 zeroinitializer, ptr %x, align 8
  %tmp.1 = alloca %b.GenBox.1..bit64, align 8
  store %b.GenBox.1..bit64 zeroinitializer, ptr %tmp.1, align 8
  %y = alloca i64, align 8
  store i64 0, ptr %y, align 8
  %m = alloca i32, align 4
  store i32 0, ptr %m, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca i64, align 8
  store i64 0, ptr %tmp.3, align 8
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%b.GenBox.1..bit64, ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %b.GenBox.1..bit64, ptr %tmp.1, i32 0, i32 0
  store i64 2, ptr %0, align 8
  %1 = getelementptr inbounds %b.GenBox.1..bit64, ptr %tmp.1, i32 0, i32 1
  store i64 3, ptr %1, align 8
  %2 = load %b.GenBox.1..bit64, ptr %tmp.1, align 8
  store %b.GenBox.1..bit64 %2, ptr %x, align 8
  %3 = getelementptr inbounds %b.GenBox.1..bit64, ptr %x, i32 0, i32 0
  %4 = load i64, ptr %3, align 8
  store i64 %4, ptr %y, align 8
  store i64 0, ptr %tmp.3, align 8
  call void @b.tag.1..bit64(ptr %tmp.3, ptr %tmp.2)
  %5 = load i32, ptr %tmp.2, align 4
  store i32 %5, ptr %m, align 4
  ret void
}

define void @.lilac.main() {
entry:
  %x = alloca %a.GenBox.1..bit32, align 8
  store %a.GenBox.1..bit32 zeroinitializer, ptr %x, align 4
  %tmp.1 = alloca %a.GenBox.1..bit32, align 8
  store %a.GenBox.1..bit32 zeroinitializer, ptr %tmp.1, align 4
  %y = alloca i32, align 4
  store i32 0, ptr %y, align 4
  %n = alloca i32, align 4
  store i32 0, ptr %n, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca i32, align 4
  store i32 0, ptr %tmp.3, align 4
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%a.GenBox.1..bit32, ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %a.GenBox.1..bit32, ptr %tmp.1, i32 0, i32 0
  store i32 1, ptr %0, align 4
  %1 = load %a.GenBox.1..bit32, ptr %tmp.1, align 4
  store %a.GenBox.1..bit32 %1, ptr %x, align 4
  %2 = getelementptr inbounds %a.GenBox.1..bit32, ptr %x, i32 0, i32 0
  %3 = load i32, ptr %2, align 4
  store i32 %3, ptr %y, align 4
  store i32 0, ptr %tmp.3, align 4
  call void @a.tag.1..bit32(ptr %tmp.3, ptr %tmp.2)
  %4 = load i32, ptr %tmp.2, align 4
  store i32 %4, ptr %n, align 4
  call void @.root.b_use()
  ret void
}

define void @b.tag.1..bit64(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (i32, ptr null, i32 1) to i64), i1 false)
  store i32 2, ptr %out0, align 4
  ret void
}

define void @a.tag.1..bit32(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %out0, i8 0, i64 ptrtoint (ptr getelementptr (i32, ptr null, i32 1) to i64), i1 false)
  store i32 1, ptr %out0, align 4
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
