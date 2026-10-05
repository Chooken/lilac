; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%GenObject.1..bit32 = type { i32 }

define void @GenObject.1..bit32.get(ptr %in0, ptr %out0) {
entry:
  %0 = load ptr, ptr %in0, align 8
  %1 = getelementptr inbounds %GenObject.1..bit32, ptr %0, i32 0, i32 0
  %2 = load i32, ptr %1, align 4
  store i32 %2, ptr %out0, align 4
  ret void
}

define void @.lilac.main() {
entry:
  %variable = alloca %GenObject.1..bit32, align 8
  store %GenObject.1..bit32 zeroinitializer, ptr %variable, align 4
  %tmp.1 = alloca %GenObject.1..bit32, align 8
  store %GenObject.1..bit32 zeroinitializer, ptr %tmp.1, align 4
  %value = alloca i32, align 4
  store i32 0, ptr %value, align 4
  %tmp.2 = alloca i32, align 4
  store i32 0, ptr %tmp.2, align 4
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  %0 = getelementptr inbounds %GenObject.1..bit32, ptr %tmp.1, i32 0, i32 0
  store i32 0, ptr %0, align 4
  %1 = load %GenObject.1..bit32, ptr %tmp.1, align 4
  store %GenObject.1..bit32 %1, ptr %variable, align 4
  store ptr %variable, ptr %tmp.3, align 8
  call void @GenObject.1..bit32.get(ptr %tmp.3, ptr %tmp.2)
  %2 = load i32, ptr %tmp.2, align 4
  store i32 %2, ptr %value, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
