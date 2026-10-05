; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%GenObject.1..bit32 = type { i32 }

define void @.lilac.main() {
entry:
  %variable = alloca %GenObject.1..bit32, align 8
  store %GenObject.1..bit32 zeroinitializer, ptr %variable, align 4
  %tmp.1 = alloca %GenObject.1..bit32, align 8
  store %GenObject.1..bit32 zeroinitializer, ptr %tmp.1, align 4
  %0 = getelementptr inbounds %GenObject.1..bit32, ptr %tmp.1, i32 0, i32 0
  store i32 0, ptr %0, align 4
  %1 = load %GenObject.1..bit32, ptr %tmp.1, align 4
  store %GenObject.1..bit32 %1, ptr %variable, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
