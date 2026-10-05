; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%GenBox.1..bit32 = type { i32 }

define void @.lilac.memcpy(ptr %dest, ptr %src, i64 %len) {
entry:
  call void @llvm.memcpy.p0.p0.i64(ptr align 1 %dest, ptr align 1 %src, i64 %len, i1 false)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #0

define ptr @.lilac.ptr_add(ptr %base, i64 %offset) {
entry:
  %0 = ptrtoint ptr %base to i64
  %1 = add i64 %0, %offset
  %2 = inttoptr i64 %1 to ptr
  ret ptr %2
}

; Function Attrs: cold noreturn nounwind memory(inaccessiblemem: write)
declare void @llvm.trap() #1

define void @.lilac.abort() {
entry:
  call void @llvm.trap()
  unreachable
}

define void @"GenBox.1..bit32.@on_drop"(ptr %in0) {
entry:
  %0 = load ptr, ptr %in0, align 8
  %1 = getelementptr inbounds %GenBox.1..bit32, ptr %0, i32 0, i32 0
  store i32 0, ptr %1, align 4
  ret void
}

define void @.lilac.main() {
entry:
  %box = alloca %GenBox.1..bit32, align 8
  store %GenBox.1..bit32 zeroinitializer, ptr %box, align 4
  %tmp.1 = alloca %GenBox.1..bit32, align 8
  store %GenBox.1..bit32 zeroinitializer, ptr %tmp.1, align 4
  %0 = getelementptr inbounds %GenBox.1..bit32, ptr %tmp.1, i32 0, i32 0
  store i32 0, ptr %0, align 4
  %1 = load %GenBox.1..bit32, ptr %tmp.1, align 4
  store %GenBox.1..bit32 %1, ptr %box, align 4
  %ref.arg = alloca ptr, align 8
  store ptr %box, ptr %ref.arg, align 8
  call void @"GenBox.1..bit32.@on_drop"(ptr %ref.arg)
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
attributes #1 = { cold noreturn nounwind memory(inaccessiblemem: write) }
