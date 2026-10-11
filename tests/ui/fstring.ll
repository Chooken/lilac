; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

@c0 = private unnamed_addr constant [1 x i8] c"x"
@c1 = private unnamed_addr constant [1 x i8] c"a"
@c2 = private unnamed_addr constant [1 x i8] c"b"

; Function Attrs: alwaysinline nounwind
define ptr @.lilac.ptr_add(ptr %base, i64 %offset) #0 {
entry:
  %address = getelementptr i8, ptr %base, i64 %offset
  ret ptr %address
}

define void @.lilac.main() {
entry:
  %part = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %part, i8 0, i64 16, i1 false)
  %tmp.1 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.1, i8 0, i64 16, i1 false)
  %msg = alloca [264 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %msg, i8 0, i64 264, i1 false)
  %tmp.2 = alloca [264 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.2, i8 0, i64 264, i1 false)
  %tmp.3 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.3, i8 0, i64 16, i1 false)
  %tmp.4 = alloca ptr, align 8
  store ptr null, ptr %tmp.4, align 8
  %tmp.5 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.5, i8 0, i64 16, i1 false)
  %tmp.6 = alloca ptr, align 8
  store ptr null, ptr %tmp.6, align 8
  %tmp.7 = alloca [16 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp.7, i8 0, i64 16, i1 false)
  %tmp.8 = alloca ptr, align 8
  store ptr null, ptr %tmp.8, align 8
  %0 = call ptr @.lilac.offset(ptr %tmp.1, i64 8)
  store ptr @c0, ptr %0, align 8
  store i64 1, ptr %tmp.1, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %part, ptr align 8 %tmp.1, i64 16, i1 false)
  call void @.lilac.format_args_new(ptr %tmp.2)
  %1 = call ptr @.lilac.offset(ptr %tmp.3, i64 8)
  store ptr @c1, ptr %1, align 8
  store i64 1, ptr %tmp.3, align 8
  store ptr %tmp.2, ptr %tmp.4, align 8
  call void @.lilac.format_args_push(ptr %tmp.4, ptr %tmp.3)
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %tmp.5, ptr align 8 %part, i64 16, i1 false)
  store ptr %tmp.2, ptr %tmp.6, align 8
  call void @.lilac.format_args_push(ptr %tmp.6, ptr %tmp.5)
  %2 = call ptr @.lilac.offset(ptr %tmp.7, i64 8)
  store ptr @c2, ptr %2, align 8
  store i64 1, ptr %tmp.7, align 8
  store ptr %tmp.2, ptr %tmp.8, align 8
  call void @.lilac.format_args_push(ptr %tmp.8, ptr %tmp.7)
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %msg, ptr align 8 %tmp.2, i64 264, i1 false)
  ret void
}

define void @.lilac.format_args_new(ptr %out0) {
entry:
  %tmp = alloca [264 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %tmp, i8 0, i64 264, i1 false)
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %out0, ptr align 8 %tmp, i64 264, i1 false)
  ret void
}

define void @.lilac.format_args_push(ptr %in0, ptr %in1) {
entry:
  %0 = load ptr, ptr %in0, align 8
  %1 = load i64, ptr %0, align 8
  %2 = icmp ult i64 %1, 16
  %3 = zext i1 %2 to i8
  %4 = icmp ne i8 %3, 0
  br i1 %4, label %append, label %done

append:                                           ; preds = %entry
  %5 = load ptr, ptr %in0, align 8
  %6 = load i64, ptr %5, align 8
  %7 = load ptr, ptr %in0, align 8
  %8 = call ptr @.lilac.offset(ptr %7, i64 8)
  %base = alloca ptr, align 8
  store ptr null, ptr %base, align 8
  store ptr %8, ptr %base, align 8
  %9 = mul i64 %6, 16
  %offp = alloca i64, align 8
  store i64 0, ptr %offp, align 8
  store i64 %9, ptr %offp, align 8
  %slot = alloca ptr, align 8
  store ptr null, ptr %slot, align 8
  %10 = load ptr, ptr %base, align 8
  %11 = load i64, ptr %offp, align 8
  %12 = call ptr @.lilac.ptr_add(ptr %10, i64 %11)
  store ptr %12, ptr %slot, align 8
  %s = alloca ptr, align 8
  store ptr null, ptr %s, align 8
  %13 = load ptr, ptr %slot, align 8
  store ptr %13, ptr %s, align 8
  %14 = load ptr, ptr %s, align 8
  call void @llvm.memcpy.p0.p0.i64(ptr align 8 %14, ptr align 8 %in1, i64 16, i1 false)
  %15 = add i64 %6, 1
  %16 = load ptr, ptr %in0, align 8
  store i64 %15, ptr %16, align 8
  br label %done

done:                                             ; preds = %append, %entry
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr nocapture writeonly, i8, i64, i1 immarg) #1

; Function Attrs: alwaysinline nounwind
define internal ptr @.lilac.offset(ptr %base, i64 %offset) #0 {
entry:
  %address = getelementptr inbounds i8, ptr %base, i64 %offset
  ret ptr %address
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #2

attributes #0 = { alwaysinline nounwind }
attributes #1 = { nocallback nofree nounwind willreturn memory(argmem: write) }
attributes #2 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
