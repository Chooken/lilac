; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%Number = type { i8 }

define void @.lilac.main() {
entry:
  %number = alloca %Number, align 8
  store %Number zeroinitializer, ptr %number, align 1
  %tmp.1 = alloca %Number, align 8
  store %Number zeroinitializer, ptr %tmp.1, align 1
  %tmp.2 = alloca %Number, align 8
  store %Number zeroinitializer, ptr %tmp.2, align 1
  %tmp.3 = alloca %Number, align 8
  store %Number zeroinitializer, ptr %tmp.3, align 1
  %0 = getelementptr inbounds %Number, ptr %tmp.1, i32 0, i32 0
  store i8 0, ptr %0, align 1
  %1 = load %Number, ptr %tmp.1, align 1
  store %Number %1, ptr %number, align 1
  %2 = getelementptr inbounds %Number, ptr %tmp.2, i32 0, i32 0
  store i8 1, ptr %2, align 1
  %3 = load %Number, ptr %tmp.2, align 1
  store %Number %3, ptr %number, align 1
  %4 = getelementptr inbounds %Number, ptr %tmp.3, i32 0, i32 0
  store i8 2, ptr %4, align 1
  %5 = load %Number, ptr %tmp.3, align 1
  store %Number %5, ptr %number, align 1
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
