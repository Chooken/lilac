; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @"@conversion:bit32:bit16"(ptr %in0, ptr %out0) {
entry:
  %0 = load i32, ptr %in0, align 4
  %1 = trunc i32 %0 to i16
  store i16 %1, ptr %out0, align 2
  ret void
}

define void @.lilac.main(ptr %in0) {
entry:
  %truncated = alloca i16, align 2
  store i16 0, ptr %truncated, align 2
  %tmp.1 = alloca i16, align 2
  store i16 0, ptr %tmp.1, align 2
  %pinned = alloca i16, align 2
  store i16 0, ptr %pinned, align 2
  %same = alloca i16, align 2
  store i16 0, ptr %same, align 2
  %v_ref = alloca ptr, align 8
  store ptr null, ptr %v_ref, align 8
  %erased = alloca ptr, align 8
  store ptr null, ptr %erased, align 8
  %round_trip = alloca ptr, align 8
  store ptr null, ptr %round_trip, align 8
  call void @"@conversion:bit32:bit16"(ptr %in0, ptr %tmp.1)
  %0 = load i16, ptr %tmp.1, align 2
  store i16 %0, ptr %truncated, align 2
  store i16 1, ptr %pinned, align 2
  %1 = load i16, ptr %truncated, align 2
  store i16 %1, ptr %same, align 2
  store ptr %in0, ptr %v_ref, align 8
  %2 = load ptr, ptr %v_ref, align 8
  store ptr %2, ptr %erased, align 8
  %3 = load ptr, ptr %erased, align 8
  store ptr %3, ptr %round_trip, align 8
  ret void
}

define i32 @main() {
entry:
  %v = alloca i32, align 4
  store i32 0, ptr %v, align 4
  call void @.lilac.main(ptr %v)
  ret i32 0
}
