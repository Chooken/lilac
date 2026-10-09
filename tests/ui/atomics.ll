; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main() {
entry:
  %value = alloca i32, align 4
  store i32 0, ptr %value, align 4
  %old = alloca i32, align 4
  store i32 0, ptr %old, align 4
  %wide = alloca i64, align 8
  store i64 0, ptr %wide, align 8
  %count = alloca i64, align 8
  store i64 0, ptr %count, align 8
  store i32 5, ptr %value, align 4
  %0 = load atomic i32, ptr %value acquire, align 4
  store i32 %0, ptr %old, align 4
  store atomic i32 7, ptr %value release, align 4
  %1 = atomicrmw add ptr %value, i32 1 seq_cst, align 4
  store i32 %1, ptr %old, align 4
  %2 = atomicrmw umax ptr %value, i32 9 monotonic, align 4
  store i32 %2, ptr %old, align 4
  %3 = cmpxchg ptr %value, i32 9, i32 2 acq_rel acquire, align 4
  %4 = extractvalue { i32, i1 } %3, 0
  store i32 %4, ptr %old, align 4
  fence seq_cst
  store i64 0, ptr %wide, align 8
  %5 = atomicrmw xchg ptr %wide, i64 1 seq_cst, align 8
  store i64 %5, ptr %count, align 8
  ret void
}
