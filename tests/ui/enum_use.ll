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
  %0 = getelementptr inbounds %Number, ptr %tmp.1, i32 0, i32 0
  store i8 0, ptr %0, align 1
  %1 = load %Number, ptr %tmp.1, align 1
  store %Number %1, ptr %number, align 1
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
