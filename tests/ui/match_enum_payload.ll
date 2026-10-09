; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%OptionalBit32 = type { i8, i32 }

define void @.lilac.main() {
entry:
  %number = alloca %OptionalBit32, align 8
  store %OptionalBit32 zeroinitializer, ptr %number, align 4
  %tmp.1 = alloca %OptionalBit32, align 8
  store %OptionalBit32 zeroinitializer, ptr %tmp.1, align 4
  %some = alloca i32, align 4
  store i32 0, ptr %some, align 4
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%OptionalBit32, ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %OptionalBit32, ptr %tmp.1, i32 0, i32 0
  store i8 0, ptr %0, align 1
  %1 = getelementptr inbounds %OptionalBit32, ptr %tmp.1, i32 0, i32 1
  store i32 1, ptr %1, align 4
  %2 = load %OptionalBit32, ptr %tmp.1, align 4
  store %OptionalBit32 %2, ptr %number, align 4
  br label %l.3

l.3:                                              ; preds = %entry
  %3 = getelementptr inbounds %OptionalBit32, ptr %number, i32 0, i32 0
  %4 = load i8, ptr %3, align 1
  %5 = icmp eq i8 %4, 0
  %6 = zext i1 %5 to i8
  %7 = icmp ne i8 %6, 0
  br i1 %7, label %l.4, label %l.5

l.4:                                              ; preds = %l.3
  %8 = getelementptr inbounds %OptionalBit32, ptr %number, i32 0, i32 1
  store ptr %8, ptr %tmp.2, align 8
  %9 = load ptr, ptr %tmp.2, align 8
  %10 = load i32, ptr %9, align 4
  store i32 %10, ptr %some, align 4
  br label %l.1

l.5:                                              ; preds = %l.3
  %11 = getelementptr inbounds %OptionalBit32, ptr %number, i32 0, i32 0
  %12 = load i8, ptr %11, align 1
  %13 = icmp eq i8 %12, 1
  %14 = zext i1 %13 to i8
  %15 = icmp ne i8 %14, 0
  br i1 %15, label %l.6, label %l.2

l.6:                                              ; preds = %l.5
  ret void

l.2:                                              ; preds = %l.5
  br label %l.1

l.1:                                              ; preds = %l.2, %l.4
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
