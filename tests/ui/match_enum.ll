; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %number = alloca [1 x i8], align 1
  call void @llvm.memset.p0.i64(ptr align 1 %number, i8 0, i64 1, i1 false)
  %tmp.1 = alloca [1 x i8], align 1
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 1, i1 false)
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 1, i1 false)
  store i8 0, ptr %tmp.1, align 1
  call void @llvm.memcpy.p0.p0.i64(ptr align 1 %number, ptr align 1 %tmp.1, i64 1, i1 false)
  br label %l.3

l.3:                                              ; preds = %entry
  %0 = load i8, ptr %number, align 1
  %1 = icmp eq i8 %0, 0
  %2 = zext i1 %1 to i8
  %3 = icmp ne i8 %2, 0
  br i1 %3, label %l.4, label %l.5

l.4:                                              ; preds = %l.3
  br label %l.1

l.5:                                              ; preds = %l.3
  %4 = load i8, ptr %number, align 1
  %5 = icmp eq i8 %4, 1
  %6 = zext i1 %5 to i8
  %7 = icmp ne i8 %6, 0
  br i1 %7, label %l.6, label %l.2

l.6:                                              ; preds = %l.5
  br label %l.1

l.2:                                              ; preds = %l.5
  br label %l.1

l.1:                                              ; preds = %l.2, %l.6, %l.4
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #1

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
