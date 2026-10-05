; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%Object = type { i32 }

define void @Object.get(ptr %in0, ptr %out0) {
entry:
  %0 = load ptr, ptr %in0, align 8
  %1 = load %Object, ptr %0, align 4
  store %Object %1, ptr %out0, align 4
  ret void
}

define void @gen.1.Object(ptr %in0, ptr %out0) {
entry:
  %tmp.1 = alloca %Object, align 8
  store %Object zeroinitializer, ptr %tmp.1, align 4
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  store ptr %in0, ptr %tmp.2, align 8
  call void @Object.get(ptr %tmp.2, ptr %tmp.1)
  %0 = load %Object, ptr %tmp.1, align 4
  store %Object %0, ptr %out0, align 4
  ret void
}

define void @.lilac.main() {
entry:
  %source = alloca %Object, align 8
  store %Object zeroinitializer, ptr %source, align 4
  %tmp.1 = alloca %Object, align 8
  store %Object zeroinitializer, ptr %tmp.1, align 4
  %result = alloca %Object, align 8
  store %Object zeroinitializer, ptr %result, align 4
  %tmp.2 = alloca %Object, align 8
  store %Object zeroinitializer, ptr %tmp.2, align 4
  %0 = getelementptr inbounds %Object, ptr %tmp.1, i32 0, i32 0
  store i32 2, ptr %0, align 4
  %1 = load %Object, ptr %tmp.1, align 4
  store %Object %1, ptr %source, align 4
  call void @gen.1.Object(ptr %source, ptr %tmp.2)
  %2 = load %Object, ptr %tmp.2, align 4
  store %Object %2, ptr %result, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
