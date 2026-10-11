; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %number = alloca [8 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %number, i8 0, i64 8, i1 false)
  %tmp.1 = alloca [8 x i8], align 4
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 8, i1 false)
  %some = alloca i32, align 4
  store i32 0, ptr %some, align 4
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  call void @llvm.memset.p0.i64(ptr align 4 %tmp.1, i8 0, i64 8, i1 false)
  store i8 0, ptr %tmp.1, align 1
  %0 = call ptr @.lilac.offset(ptr %tmp.1, i64 4)
  store i32 1, ptr %0, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr align 4 %number, ptr align 4 %tmp.1, i64 8, i1 false)
  br label %l.3

l.3:                                              ; preds = %entry
  %1 = load i8, ptr %number, align 1
  %2 = icmp eq i8 %1, 0
  %3 = zext i1 %2 to i8
  %4 = icmp ne i8 %3, 0
  br i1 %4, label %l.4, label %l.5

l.4:                                              ; preds = %l.3
  %5 = call ptr @.lilac.offset(ptr %number, i64 4)
  store ptr %5, ptr %tmp.2, align 8
  %6 = load ptr, ptr %tmp.2, align 8
  %7 = load i32, ptr %6, align 4
  store i32 %7, ptr %some, align 4
  br label %l.1

l.5:                                              ; preds = %l.3
  %8 = load i8, ptr %number, align 1
  %9 = icmp eq i8 %8, 1
  %10 = zext i1 %9 to i8
  %11 = icmp ne i8 %10, 0
  br i1 %11, label %l.6, label %l.2

l.6:                                              ; preds = %l.5
  ret void

l.2:                                              ; preds = %l.5
  br label %l.1

l.1:                                              ; preds = %l.2, %l.4
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

; Function Attrs: alwaysinline nounwind
define internal ptr @.lilac.offset(ptr %base, i64 %offset) #1 {
entry:
  %address = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %address
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #2

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #1 = { alwaysinline nounwind }
attributes #2 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
