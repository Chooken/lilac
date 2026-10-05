; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define void @.lilac.main(ptr %in0, ptr %in1, ptr %in2) {
entry:
  %one = alloca i32, align 4
  store i32 0, ptr %one, align 4
  %divisor = alloca i32, align 4
  store i32 0, ptr %divisor, align 4
  %quotient = alloca i32, align 4
  store i32 0, ptr %quotient, align 4
  %remainder = alloca i32, align 4
  store i32 0, ptr %remainder, align 4
  %native_one = alloca i64, align 8
  store i64 0, ptr %native_one, align 8
  %wide_divisor = alloca i64, align 8
  store i64 0, ptr %wide_divisor, align 8
  %wide_quotient = alloca i64, align 8
  store i64 0, ptr %wide_quotient, align 8
  %wide_remainder = alloca i64, align 8
  store i64 0, ptr %wide_remainder, align 8
  store i32 1, ptr %one, align 4
  %0 = load i32, ptr %in1, align 4
  %1 = load i32, ptr %one, align 4
  %2 = add i32 %0, %1
  store i32 %2, ptr %divisor, align 4
  %3 = load i32, ptr %in0, align 4
  %4 = load i32, ptr %divisor, align 4
  %5 = udiv i32 %3, %4
  store i32 %5, ptr %quotient, align 4
  %6 = load i32, ptr %in0, align 4
  %7 = load i32, ptr %divisor, align 4
  %8 = urem i32 %6, %7
  store i32 %8, ptr %remainder, align 4
  %9 = load i32, ptr %in0, align 4
  %10 = load i32, ptr %divisor, align 4
  %11 = icmp ult i32 %9, %10
  %12 = zext i1 %11 to i8
  %13 = icmp ne i8 %12, 0
  br i1 %13, label %l.1, label %l.2

l.1:                                              ; preds = %entry
  store i32 1, ptr %quotient, align 4
  br label %l.3

l.2:                                              ; preds = %entry
  br label %l.3

l.3:                                              ; preds = %l.2, %l.1
  %14 = load i32, ptr %in0, align 4
  %15 = load i32, ptr %divisor, align 4
  %16 = icmp ugt i32 %14, %15
  %17 = zext i1 %16 to i8
  %18 = icmp ne i8 %17, 0
  br i1 %18, label %l.4, label %l.5

l.4:                                              ; preds = %l.3
  store i32 1, ptr %remainder, align 4
  br label %l.6

l.5:                                              ; preds = %l.3
  br label %l.6

l.6:                                              ; preds = %l.5, %l.4
  store i64 1, ptr %native_one, align 8
  %19 = load i64, ptr %in2, align 8
  %20 = load i64, ptr %native_one, align 8
  %21 = add i64 %19, %20
  store i64 %21, ptr %wide_divisor, align 8
  %22 = load i64, ptr %in2, align 8
  %23 = load i64, ptr %wide_divisor, align 8
  %24 = udiv i64 %22, %23
  store i64 %24, ptr %wide_quotient, align 8
  %25 = load i64, ptr %in2, align 8
  %26 = load i64, ptr %wide_divisor, align 8
  %27 = urem i64 %25, %26
  store i64 %27, ptr %wide_remainder, align 8
  ret void
}

define i32 @main() {
entry:
  %a = alloca i32, align 4
  store i32 0, ptr %a, align 4
  %b = alloca i32, align 4
  store i32 0, ptr %b, align 4
  %w = alloca i64, align 8
  store i64 0, ptr %w, align 8
  call void @.lilac.main(ptr %a, ptr %b, ptr %w)
  ret i32 0
}
