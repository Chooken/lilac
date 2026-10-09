; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [1 x i8] c"x"
@c1 = private unnamed_addr constant [1 x i8] c"a"
@c2 = private unnamed_addr constant [1 x i8] c"b"

define ptr @.lilac.ptr_add(ptr %base, i64 %offset) {
entry:
  %0 = ptrtoint ptr %base to i64
  %1 = add i64 %0, %offset
  %2 = inttoptr i64 %1 to ptr
  ret ptr %2
}

define void @.lilac.main() {
entry:
  %part = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %part, align 8
  %msg = alloca { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } }, align 8
  store { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } } zeroinitializer, ptr %msg, align 8
  %tmp.1 = alloca { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } }, align 8
  store { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } } zeroinitializer, ptr %tmp.1, align 8
  %tmp.2 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.2, align 8
  %tmp.3 = alloca ptr, align 8
  store ptr null, ptr %tmp.3, align 8
  %tmp.4 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.4, align 8
  %tmp.5 = alloca ptr, align 8
  store ptr null, ptr %tmp.5, align 8
  %tmp.6 = alloca { i64, ptr }, align 8
  store { i64, ptr } zeroinitializer, ptr %tmp.6, align 8
  %tmp.7 = alloca ptr, align 8
  store ptr null, ptr %tmp.7, align 8
  store { i64, ptr } { i64 1, ptr @c0 }, ptr %part, align 8
  call void @.lilac.format_args_new(ptr %tmp.1)
  store { i64, ptr } { i64 1, ptr @c1 }, ptr %tmp.2, align 8
  store ptr %tmp.1, ptr %tmp.3, align 8
  call void @.lilac.format_args_push(ptr %tmp.3, ptr %tmp.2)
  %0 = load { i64, ptr }, ptr %part, align 8
  store { i64, ptr } %0, ptr %tmp.4, align 8
  store ptr %tmp.1, ptr %tmp.5, align 8
  call void @.lilac.format_args_push(ptr %tmp.5, ptr %tmp.4)
  store { i64, ptr } { i64 1, ptr @c2 }, ptr %tmp.6, align 8
  store ptr %tmp.1, ptr %tmp.7, align 8
  call void @.lilac.format_args_push(ptr %tmp.7, ptr %tmp.6)
  %1 = load { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } }, ptr %tmp.1, align 8
  store { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } } %1, ptr %msg, align 8
  ret void
}

define void @.lilac.format_args_new(ptr %out0) {
entry:
  %tmp = alloca { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } }, align 8
  store { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } } zeroinitializer, ptr %tmp, align 8
  %0 = load { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } }, ptr %tmp, align 8
  store { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } } %0, ptr %out0, align 8
  ret void
}

define void @.lilac.format_args_push(ptr %in0, ptr %in1) {
entry:
  %0 = load ptr, ptr %in0, align 8
  %1 = getelementptr inbounds { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } }, ptr %0, i32 0, i32 0
  %2 = load i64, ptr %1, align 8
  %3 = icmp ult i64 %2, 16
  %4 = zext i1 %3 to i8
  %5 = icmp ne i8 %4, 0
  br i1 %5, label %append, label %done

append:                                           ; preds = %entry
  %6 = load ptr, ptr %in0, align 8
  %7 = getelementptr inbounds { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } }, ptr %6, i32 0, i32 0
  %8 = load i64, ptr %7, align 8
  %9 = load ptr, ptr %in0, align 8
  %10 = getelementptr inbounds { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } }, ptr %9, i32 0, i32 1
  %base = alloca ptr, align 8
  store ptr null, ptr %base, align 8
  store ptr %10, ptr %base, align 8
  %11 = mul i64 %8, 16
  %offp = alloca i64, align 8
  store i64 0, ptr %offp, align 8
  store i64 %11, ptr %offp, align 8
  %slot = alloca ptr, align 8
  store ptr null, ptr %slot, align 8
  %12 = load ptr, ptr %base, align 8
  %13 = load i64, ptr %offp, align 8
  %14 = call ptr @.lilac.ptr_add(ptr %12, i64 %13)
  store ptr %14, ptr %slot, align 8
  %s = alloca ptr, align 8
  store ptr null, ptr %s, align 8
  %15 = load ptr, ptr %slot, align 8
  store ptr %15, ptr %s, align 8
  %16 = load ptr, ptr %s, align 8
  %17 = load { i64, ptr }, ptr %in1, align 8
  store { i64, ptr } %17, ptr %16, align 8
  %18 = add i64 %8, 1
  %19 = load ptr, ptr %in0, align 8
  %20 = getelementptr inbounds { i64, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr }, { i64, ptr } }, ptr %19, i32 0, i32 0
  store i64 %18, ptr %20, align 8
  br label %done

done:                                             ; preds = %append, %entry
  ret void
}
