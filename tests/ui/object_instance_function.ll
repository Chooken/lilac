; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%Object = type { i32 }

define void @Object.get(ptr %in0, ptr %out0) {
entry:
  %0 = load ptr, ptr %in0, align 8
  %1 = getelementptr inbounds %Object, ptr %0, i32 0, i32 0
  %2 = load i32, ptr %1, align 4
  store i32 %2, ptr %out0, align 4
  ret void
}

define void @.lilac.main() {
entry:
  %variable = alloca %Object, align 8
  store %Object zeroinitializer, ptr %variable, align 4
  %tmp.1 = alloca %Object, align 8
  store %Object zeroinitializer, ptr %tmp.1, align 4
  %member = alloca i32, align 4
  store i32 0, ptr %member, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  %0 = getelementptr inbounds %Object, ptr %tmp.1, i32 0, i32 0
  store i32 1, ptr %0, align 4
  %1 = load %Object, ptr %tmp.1, align 4
  store %Object %1, ptr %variable, align 4
  store ptr %variable, ptr %tmp.3, align 8
  call void @Object.get(ptr %tmp.3, ptr %tmp.2)
  %2 = load i32, ptr %tmp.2, align 4
  store i32 %2, ptr %member, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
