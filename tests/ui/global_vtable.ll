; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@.root.count = internal global [4 x i8] c"\02\00\00\00", align 4
@.root.TABLE = internal global <{ ptr, ptr }> <{ ptr @.root.add, ptr @.root.total }>, align 8
@.root.sink = internal global <{ ptr, ptr }> <{ ptr @.root.count, ptr @.root.TABLE }>, align 8

; Function Attrs: cold noreturn nounwind memory(inaccessiblemem: write)
declare void @llvm.trap() #0

define void @.lilac.abort() {
entry:
  call void @llvm.trap()
  unreachable
}

define void @.root.add(ptr %in0, ptr %in1) {
entry:
  %count = alloca ptr, align 8
  store ptr null, ptr %count, align 8
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %count, align 8
  %1 = load ptr, ptr %count, align 8
  %2 = load i32, ptr %1, align 4
  %3 = load i32, ptr %in1, align 4
  %4 = add i32 %2, %3
  %5 = load ptr, ptr %count, align 8
  store i32 %4, ptr %5, align 4
  ret void
}

define void @.root.total(ptr %in0, ptr %out0) {
entry:
  %count = alloca ptr, align 8
  store ptr null, ptr %count, align 8
  call void @llvm.memset.p0.i64(ptr align 4 %out0, i8 0, i64 4, i1 false)
  %0 = load ptr, ptr %in0, align 8
  store ptr %0, ptr %count, align 8
  %1 = load ptr, ptr %count, align 8
  %2 = load i32, ptr %1, align 4
  store i32 %2, ptr %out0, align 4
  ret void
}

define void @.lilac.main() {
entry:
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  %tmp.4 = alloca ptr, align 8
  store ptr null, ptr %tmp.4, align 8
  %tmp.5 = alloca ptr, align 8
  store ptr null, ptr %tmp.5, align 8
  %tmp.6 = alloca i32, align 4
  store i32 0, ptr %tmp.6, align 4
  %tmp.7 = alloca i32, align 4
  store i32 0, ptr %tmp.7, align 4
  %tmp.8 = alloca ptr, align 8
  store ptr null, ptr %tmp.8, align 8
  %tmp.9 = alloca ptr, align 8
  store ptr null, ptr %tmp.9, align 8
  %tmp.10 = alloca ptr, align 8
  store ptr null, ptr %tmp.10, align 8
  %tmp.11 = alloca ptr, align 8
  store ptr null, ptr %tmp.11, align 8
  %tmp.12 = alloca ptr, align 8
  store ptr null, ptr %tmp.12, align 8
  store ptr @.root.sink, ptr %tmp.1, align 8
  %0 = load ptr, ptr %tmp.1, align 8
  %1 = call ptr @.lilac.offset(ptr %0, i64 8)
  %2 = load ptr, ptr %1, align 8
  store ptr %2, ptr %tmp.2, align 8
  %3 = load ptr, ptr %tmp.2, align 8
  %4 = load ptr, ptr %3, align 8
  store ptr %4, ptr %tmp.3, align 8
  store ptr @.root.sink, ptr %tmp.4, align 8
  %5 = load ptr, ptr %tmp.4, align 8
  %6 = load ptr, ptr %5, align 8
  store ptr %6, ptr %tmp.5, align 8
  store i32 3, ptr %tmp.6, align 4
  %7 = load ptr, ptr %tmp.3, align 8
  call void %7(ptr %tmp.5, ptr %tmp.6)
  store ptr @.root.sink, ptr %tmp.8, align 8
  %8 = load ptr, ptr %tmp.8, align 8
  %9 = call ptr @.lilac.offset(ptr %8, i64 8)
  %10 = load ptr, ptr %9, align 8
  store ptr %10, ptr %tmp.9, align 8
  %11 = load ptr, ptr %tmp.9, align 8
  %12 = call ptr @.lilac.offset(ptr %11, i64 8)
  %13 = load ptr, ptr %12, align 8
  store ptr %13, ptr %tmp.10, align 8
  store ptr @.root.sink, ptr %tmp.11, align 8
  %14 = load ptr, ptr %tmp.11, align 8
  %15 = load ptr, ptr %14, align 8
  store ptr %15, ptr %tmp.12, align 8
  %16 = load ptr, ptr %tmp.10, align 8
  call void %16(ptr %tmp.12, ptr %tmp.7)
  %17 = load i32, ptr %tmp.7, align 4
  %18 = icmp eq i32 %17, 5
  %19 = zext i1 %18 to i8
  %20 = icmp eq i8 %19, 0
  %21 = zext i1 %20 to i8
  %22 = icmp ne i8 %21, 0
  br i1 %22, label %l.1, label %l.2

l.1:                                              ; preds = %entry
  call void @.lilac.abort()
  unreachable

l.2:                                              ; preds = %entry
  br label %l.3

l.3:                                              ; preds = %l.2
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #1

; Function Attrs: alwaysinline nounwind
define internal ptr @.lilac.offset(ptr %base, i64 %offset) #2 {
entry:
  %address = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %address
}

attributes #0 = { cold noreturn nounwind memory(inaccessiblemem: write) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #2 = { alwaysinline nounwind }
