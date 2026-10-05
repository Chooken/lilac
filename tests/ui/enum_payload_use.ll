; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%OptionalBit32 = type { i8, i32 }

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

define void @.lilac.main() {
entry:
  %variable = alloca %OptionalBit32, align 8
  store %OptionalBit32 zeroinitializer, ptr %variable, align 4
  %tmp.1 = alloca %OptionalBit32, align 8
  store %OptionalBit32 zeroinitializer, ptr %tmp.1, align 4
  %tmp.2 = alloca %OptionalBit32, align 8
  store %OptionalBit32 zeroinitializer, ptr %tmp.2, align 4
  %0 = getelementptr inbounds %OptionalBit32, ptr %tmp.1, i32 0, i32 0
  store i8 0, ptr %0, align 1
  %1 = getelementptr inbounds %OptionalBit32, ptr %tmp.1, i32 0, i32 1
  store i32 0, ptr %1, align 4
  %2 = load %OptionalBit32, ptr %tmp.1, align 4
  store %OptionalBit32 %2, ptr %variable, align 4
  %3 = getelementptr inbounds %OptionalBit32, ptr %tmp.2, i32 0, i32 0
  store i8 1, ptr %3, align 1
  %4 = load %OptionalBit32, ptr %tmp.2, align 4
  store %OptionalBit32 %4, ptr %variable, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
attributes #1 = { cold noreturn nounwind memory(inaccessiblemem: write) }
