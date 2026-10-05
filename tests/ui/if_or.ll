; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %conditional = alloca i32, align 4
  store i32 0, ptr %conditional, align 4
  %t.1 = alloca i8, align 1
  store i8 0, ptr %t.1, align 1
  store i32 0, ptr %conditional, align 4
  br label %l.2

l.2:                                              ; preds = %entry
  store i8 1, ptr %t.1, align 1
  br label %l.3

l.3:                                              ; preds = %l.2
  %0 = load i8, ptr %t.1, align 1
  br label %l.4

l.4:                                              ; preds = %l.3
  store i32 1, ptr %conditional, align 4
  br label %l.6

l.6:                                              ; preds = %l.4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
