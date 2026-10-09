; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%Pair = type { i64, i64 }

define void @.lilac.memmove(ptr %0, ptr %1, i64 %2) {
entry:
  call void @llvm.memmove.p0.p0.i64(ptr align 1 %0, ptr align 1 %1, i64 %2, i1 false)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memmove.p0.p0.i64(ptr nocapture writeonly, ptr nocapture readonly, i64, i1 immarg) #0

define void @.lilac.memset(ptr %0, i8 %1, i64 %2) {
entry:
  call void @llvm.memset.p0.i64(ptr align 1 %0, i8 %1, i64 %2, i1 false)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #1

; Function Attrs: cold noreturn nounwind memory(inaccessiblemem: write)
declare void @llvm.trap() #2

define void @.lilac.abort() {
entry:
  call void @llvm.trap()
  unreachable
}

define void @.lilac.main() {
entry:
  %pair = alloca %Pair, align 8
  store %Pair zeroinitializer, ptr %pair, align 8
  %tmp.1 = alloca %Pair, align 8
  store %Pair zeroinitializer, ptr %tmp.1, align 8
  %base = alloca ptr, align 8
  store ptr null, ptr %base, align 8
  %bytes = alloca ptr, align 8
  store ptr null, ptr %bytes, align 8
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  %tmp.4 = alloca i64, align 8
  store i64 0, ptr %tmp.4, align 8
  %tmp.5 = alloca ptr, align 8
  store ptr null, ptr %tmp.5, align 8
  %tmp.6 = alloca i8, align 1
  store i8 0, ptr %tmp.6, align 1
  %tmp.7 = alloca i64, align 8
  store i64 0, ptr %tmp.7, align 8
  %address = alloca i64, align 8
  store i64 0, ptr %address, align 8
  %again = alloca ptr, align 8
  store ptr null, ptr %again, align 8
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%Pair, ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %Pair, ptr %tmp.1, i32 0, i32 0
  store i64 506097522914230528, ptr %0, align 8
  %1 = getelementptr inbounds %Pair, ptr %tmp.1, i32 0, i32 1
  store i64 0, ptr %1, align 8
  %2 = load %Pair, ptr %tmp.1, align 8
  store %Pair %2, ptr %pair, align 8
  store ptr %pair, ptr %base, align 8
  %3 = load ptr, ptr %base, align 8
  store ptr %3, ptr %bytes, align 8
  %4 = load ptr, ptr %base, align 8
  %5 = ptrtoint ptr %4 to i64
  %6 = add i64 %5, 2
  %7 = inttoptr i64 %6 to ptr
  store ptr %7, ptr %tmp.2, align 8
  %8 = load ptr, ptr %bytes, align 8
  store ptr %8, ptr %tmp.3, align 8
  store i64 8, ptr %tmp.4, align 8
  %9 = load ptr, ptr %tmp.2, align 8
  %10 = load ptr, ptr %tmp.3, align 8
  %11 = load i64, ptr %tmp.4, align 8
  call void @.lilac.memmove(ptr %9, ptr %10, i64 %11)
  %12 = load ptr, ptr %bytes, align 8
  store ptr %12, ptr %tmp.5, align 8
  store i8 0, ptr %tmp.6, align 1
  store i64 8, ptr %tmp.7, align 8
  %13 = load ptr, ptr %tmp.5, align 8
  %14 = load i8, ptr %tmp.6, align 1
  %15 = load i64, ptr %tmp.7, align 8
  call void @.lilac.memset(ptr %13, i8 %14, i64 %15)
  %16 = load ptr, ptr %base, align 8
  %17 = ptrtoint ptr %16 to i64
  store i64 %17, ptr %address, align 8
  %18 = load i64, ptr %address, align 8
  %19 = inttoptr i64 %18 to ptr
  store ptr %19, ptr %again, align 8
  %20 = load ptr, ptr %again, align 8
  %21 = ptrtoint ptr %20 to i64
  %22 = load i64, ptr %address, align 8
  %23 = icmp ult i64 %21, %22
  %24 = zext i1 %23 to i8
  %25 = icmp ne i8 %24, 0
  br i1 %25, label %l.1, label %l.2

l.1:                                              ; preds = %entry
  call void @.lilac.abort()
  unreachable

l.2:                                              ; preds = %entry
  br label %l.3

l.3:                                              ; preds = %l.2
  ret void
}

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #2 = { cold noreturn nounwind memory(inaccessiblemem: write) }
