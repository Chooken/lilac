; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

; Function Attrs: cold noreturn nounwind memory(inaccessiblemem: write)
declare void @llvm.trap() #0

define void @.lilac.abort() {
entry:
  call void @llvm.trap()
  unreachable
}

define void @.root.one(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 4 %out0, i8 0, i64 4, i1 false)
  %0 = load i32, ptr %in0, align 4
  %1 = add i32 %0, 1
  store i32 %1, ptr %out0, align 4
  ret void
}

define void @.root.same(ptr %in0, ptr %out0) {
entry:
  call void @llvm.memset.p0.i64(ptr align 4 %out0, i8 0, i64 4, i1 false)
  %0 = load i32, ptr %in0, align 4
  %1 = add i32 %0, 1
  store i32 %1, ptr %out0, align 4
  ret void
}

define void @.lilac.main() {
entry:
  %a = alloca ptr, align 8
  store ptr null, ptr %a, align 8
  %b = alloca ptr, align 8
  store ptr null, ptr %b, align 8
  %tmp.1 = alloca i32, align 4
  store i32 0, ptr %tmp.1, align 4
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  %tmp.3 = alloca i32, align 4
  store i32 0, ptr %tmp.3, align 4
  %tmp.4 = alloca i32, align 4
  store i32 0, ptr %tmp.4, align 4
  %tmp.5 = alloca ptr, align 8
  store ptr null, ptr %tmp.5, align 8
  %tmp.6 = alloca i32, align 4
  store i32 0, ptr %tmp.6, align 4
  store ptr @.root.one, ptr %a, align 8
  store ptr @.root.same, ptr %b, align 8
  %0 = load ptr, ptr %a, align 8
  %1 = load ptr, ptr %b, align 8
  %2 = ptrtoint ptr %0 to i64
  %3 = ptrtoint ptr %1 to i64
  %4 = icmp eq i64 %2, %3
  %5 = zext i1 %4 to i8
  %6 = icmp ne i8 %5, 0
  br i1 %6, label %l.1, label %l.2

l.1:                                              ; preds = %entry
  call void @.lilac.abort()
  unreachable

l.2:                                              ; preds = %entry
  br label %l.3

l.3:                                              ; preds = %l.2
  %7 = load ptr, ptr %a, align 8
  %8 = ptrtoint ptr %7 to i64
  %9 = icmp eq i64 %8, ptrtoint (ptr @.root.one to i64)
  %10 = zext i1 %9 to i8
  %11 = icmp eq i8 %10, 0
  %12 = zext i1 %11 to i8
  %13 = icmp ne i8 %12, 0
  br i1 %13, label %l.4, label %l.5

l.4:                                              ; preds = %l.3
  call void @.lilac.abort()
  unreachable

l.5:                                              ; preds = %l.3
  br label %l.6

l.6:                                              ; preds = %l.5
  %14 = load ptr, ptr %a, align 8
  store ptr %14, ptr %tmp.2, align 8
  store i32 1, ptr %tmp.3, align 4
  %15 = load ptr, ptr %tmp.2, align 8
  call void %15(ptr %tmp.3, ptr %tmp.1)
  %16 = load ptr, ptr %b, align 8
  store ptr %16, ptr %tmp.5, align 8
  store i32 1, ptr %tmp.6, align 4
  %17 = load ptr, ptr %tmp.5, align 8
  call void %17(ptr %tmp.6, ptr %tmp.4)
  %18 = load i32, ptr %tmp.1, align 4
  %19 = load i32, ptr %tmp.4, align 4
  %20 = icmp eq i32 %18, %19
  %21 = zext i1 %20 to i8
  %22 = icmp eq i8 %21, 0
  %23 = zext i1 %22 to i8
  %24 = icmp ne i8 %23, 0
  br i1 %24, label %l.7, label %l.8

l.7:                                              ; preds = %l.6
  call void @.lilac.abort()
  unreachable

l.8:                                              ; preds = %l.6
  br label %l.9

l.9:                                              ; preds = %l.8
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #1

attributes #0 = { cold noreturn nounwind memory(inaccessiblemem: write) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: write) }
