; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%Number = type { i8 }

define void @.lilac.main() {
l.3:
  %number = alloca %Number, align 8
  store %Number zeroinitializer, ptr %number, align 1
  %tmp.1 = alloca %Number, align 8
  store %Number zeroinitializer, ptr %tmp.1, align 1
  %0 = getelementptr inbounds %Number, ptr %tmp.1, i32 0, i32 0
  store i8 0, ptr %0, align 1
  %1 = load %Number, ptr %tmp.1, align 1
  store %Number %1, ptr %number, align 1
  %2 = getelementptr inbounds %Number, ptr %number, i32 0, i32 0
  %3 = load i8, ptr %2, align 1
  %4 = icmp eq i8 %3, 0
  %5 = zext i1 %4 to i8
  %6 = icmp ne i8 %5, 0
  br i1 %6, label %l.4, label %l.5

l.4:                                              ; preds = %l.3
  br label %l.1

l.5:                                              ; preds = %l.3
  %7 = getelementptr inbounds %Number, ptr %number, i32 0, i32 0
  %8 = load i8, ptr %7, align 1
  %9 = icmp eq i8 %8, 1
  %10 = zext i1 %9 to i8
  %11 = icmp ne i8 %10, 0
  br i1 %11, label %l.6, label %l.2

l.6:                                              ; preds = %l.5
  br label %l.1

l.2:                                              ; preds = %l.5
  br label %l.1

l.1:                                              ; preds = %l.2, %l.6, %l.4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
