; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

%b.Thing = type { i32 }
%a.Thing = type { i32 }

define void @"b.Thing.@on_drop:ref_b.Thing"(ptr %in0) {
entry:
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %0 = load ptr, ptr %in0, align 8
  %1 = getelementptr inbounds %b.Thing, ptr %0, i32 0, i32 0
  store ptr %1, ptr %tmp.1, align 8
  %2 = load ptr, ptr %tmp.1, align 8
  store i32 0, ptr %2, align 4
  ret void
}

define void @"a.Thing.@on_drop:ref_a.Thing"(ptr %in0) {
entry:
  %tmp.1 = alloca ptr, align 8
  store ptr null, ptr %tmp.1, align 8
  %0 = load ptr, ptr %in0, align 8
  %1 = getelementptr inbounds %a.Thing, ptr %0, i32 0, i32 0
  store ptr %1, ptr %tmp.1, align 8
  %2 = load ptr, ptr %tmp.1, align 8
  store i32 0, ptr %2, align 4
  ret void
}

define void @b_use() {
entry:
  %x = alloca %b.Thing, align 8
  store %b.Thing zeroinitializer, ptr %x, align 4
  %tmp.1 = alloca %b.Thing, align 8
  store %b.Thing zeroinitializer, ptr %tmp.1, align 4
  %y = alloca i32, align 4
  store i32 0, ptr %y, align 4
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%b.Thing, ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %b.Thing, ptr %tmp.1, i32 0, i32 0
  store i32 2, ptr %0, align 4
  %1 = load %b.Thing, ptr %tmp.1, align 4
  store %b.Thing %1, ptr %x, align 4
  %2 = getelementptr inbounds %b.Thing, ptr %x, i32 0, i32 0
  %3 = load i32, ptr %2, align 4
  store i32 %3, ptr %y, align 4
  store ptr %x, ptr %tmp.2, align 8
  call void @"b.Thing.@on_drop:ref_b.Thing"(ptr %tmp.2)
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
  %tmp.2 = alloca ptr, align 8
  store ptr null, ptr %tmp.2, align 8
  call void @llvm.memset.p0.i64(ptr align 1 %tmp.1, i8 0, i64 ptrtoint (ptr getelementptr (%a.Thing, ptr null, i32 1) to i64), i1 false)
  %0 = getelementptr inbounds %a.Thing, ptr %tmp.1, i32 0, i32 0
  store i32 1, ptr %0, align 4
  %1 = load %a.Thing, ptr %tmp.1, align 4
  store %a.Thing %1, ptr %x, align 4
  %2 = getelementptr inbounds %a.Thing, ptr %x, i32 0, i32 0
  %3 = load i32, ptr %2, align 4
  store i32 %3, ptr %y, align 4
  call void @b_use()
  store ptr %x, ptr %tmp.2, align 8
  call void @"a.Thing.@on_drop:ref_a.Thing"(ptr %tmp.2)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #0

define i32 @main() {
entry:
  call void @.lilac.main()
  ret i32 0
}

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: write) }
