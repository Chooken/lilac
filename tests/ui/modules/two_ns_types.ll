; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%b.Thing = type { i32 }
%a.Thing = type { i32 }

define void @b_use() {
entry:
  %x = alloca %b.Thing, align 8
  store %b.Thing zeroinitializer, ptr %x, align 4
  %tmp.1 = alloca %b.Thing, align 8
  store %b.Thing zeroinitializer, ptr %tmp.1, align 4
  %y = alloca i32, align 4
  store i32 0, ptr %y, align 4
  %0 = getelementptr inbounds %b.Thing, ptr %tmp.1, i32 0, i32 0
  store i32 2, ptr %0, align 4
  %1 = load %b.Thing, ptr %tmp.1, align 4
  store %b.Thing %1, ptr %x, align 4
  %2 = getelementptr inbounds %b.Thing, ptr %x, i32 0, i32 0
  %3 = load i32, ptr %2, align 4
  store i32 %3, ptr %y, align 4
  %ref.arg = alloca ptr, align 8
  store ptr %x, ptr %ref.arg, align 8
  call void @"b.Thing.@on_drop:ref_b.Thing"(ptr %ref.arg)
  ret void
}

define void @.lilac.main() {
entry:
  %x = alloca %a.Thing, align 8
  store %a.Thing zeroinitializer, ptr %x, align 4
  %tmp.1 = alloca %a.Thing, align 8
  store %a.Thing zeroinitializer, ptr %tmp.1, align 4
  %y = alloca i32, align 4
  store i32 0, ptr %y, align 4
  %0 = getelementptr inbounds %a.Thing, ptr %tmp.1, i32 0, i32 0
  store i32 1, ptr %0, align 4
  %1 = load %a.Thing, ptr %tmp.1, align 4
  store %a.Thing %1, ptr %x, align 4
  %2 = getelementptr inbounds %a.Thing, ptr %x, i32 0, i32 0
  %3 = load i32, ptr %2, align 4
  store i32 %3, ptr %y, align 4
  call void @b_use()
  %ref.arg = alloca ptr, align 8
  store ptr %x, ptr %ref.arg, align 8
  call void @"a.Thing.@on_drop:ref_a.Thing"(ptr %ref.arg)
  ret void
}

define void @"b.Thing.@on_drop:ref_b.Thing"(ptr %in0) {
entry:
  %0 = load ptr, ptr %in0, align 8
  %1 = getelementptr inbounds %b.Thing, ptr %0, i32 0, i32 0
  store i32 0, ptr %1, align 4
  ret void
}

define void @"a.Thing.@on_drop:ref_a.Thing"(ptr %in0) {
entry:
  %0 = load ptr, ptr %in0, align 8
  %1 = getelementptr inbounds %a.Thing, ptr %0, i32 0, i32 0
  store i32 0, ptr %1, align 4
  ret void
}

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}
