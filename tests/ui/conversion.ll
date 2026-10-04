; ModuleID = 'lilac'
source_filename = "lilac"
target datalayout = "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
target triple = "x86_64-unknown-none"

define { i64, ptr } @byte_buffer_from(ptr %data, i64 %len) {
entry:
  %0 = insertvalue { i64, ptr } undef, i64 %len, 0
  %1 = insertvalue { i64, ptr } %0, ptr %data, 1
  ret { i64, ptr } %1
}

define ptr @byte_buffer_ptr({ i64, ptr } %buffer) {
entry:
  %0 = extractvalue { i64, ptr } %buffer, 1
  ret ptr %0
}

define i64 @byte_buffer_len({ i64, ptr } %buffer) {
entry:
  %0 = extractvalue { i64, ptr } %buffer, 0
  ret i64 %0
}

define i8 @load_u8(ptr %data, i64 %index) {
entry:
  %0 = ptrtoint ptr %data to i64
  %1 = add i64 %0, %index
  %2 = inttoptr i64 %1 to ptr
  %3 = load i8, ptr %2, align 1
  ret i8 %3
}

define void @store_u8(ptr %data, i64 %index, i8 %value) {
entry:
  %0 = ptrtoint ptr %data to i64
  %1 = add i64 %0, %index
  %2 = inttoptr i64 %1 to ptr
  store i8 %value, ptr %2, align 1
  ret void
}

define void @lilac_memcpy(ptr %dest, ptr %src, i64 %len) {
entry:
  call void @llvm.memcpy.p0.p0.i64(ptr align 1 %dest, ptr align 1 %src, i64 %len, i1 false)
  ret void
}

; Function Attrs: nocallback nofree nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias nocapture writeonly, ptr noalias nocapture readonly, i64, i1 immarg) #0

define { i64, [16 x { i64, ptr }] } @format_args_new() {
entry:
  ret { i64, [16 x { i64, ptr }] } zeroinitializer
}

define void @format_args_push(ptr %table, { i64, ptr } %piece) {
entry:
  %0 = getelementptr inbounds { i64, [16 x { i64, ptr }] }, ptr %table, i32 0, i32 0
  %1 = load i64, ptr %0, align 8
  %2 = icmp ult i64 %1, 16
  br i1 %2, label %append, label %done

append:                                           ; preds = %entry
  %3 = getelementptr inbounds { i64, [16 x { i64, ptr }] }, ptr %table, i32 0, i32 1
  %4 = mul i64 %1, 16
  %5 = ptrtoint ptr %3 to i64
  %6 = add i64 %5, %4
  %7 = inttoptr i64 %6 to ptr
  store { i64, ptr } %piece, ptr %7, align 8
  %8 = add i64 %1, 1
  store i64 %8, ptr %0, align 8
  br label %done

done:                                             ; preds = %append, %entry
  ret void
}

define i64 @format_args_count({ i64, [16 x { i64, ptr }] } %table) {
entry:
  %0 = extractvalue { i64, [16 x { i64, ptr }] } %table, 0
  ret i64 %0
}

define { i64, ptr } @format_args_piece({ i64, [16 x { i64, ptr }] } %table, i64 %index) {
entry:
  %0 = alloca { i64, [16 x { i64, ptr }] }, align 8
  store { i64, [16 x { i64, ptr }] } %table, ptr %0, align 8
  %1 = getelementptr inbounds { i64, [16 x { i64, ptr }] }, ptr %0, i32 0, i32 1
  %2 = mul i64 %index, 16
  %3 = ptrtoint ptr %1 to i64
  %4 = add i64 %3, %2
  %5 = inttoptr i64 %4 to ptr
  %6 = load { i64, ptr }, ptr %5, align 8
  ret { i64, ptr } %6
}

; Function Attrs: cold noreturn nounwind memory(inaccessiblemem: write)
declare void @llvm.trap() #1

define void @lilac_abort() {
entry:
  call void @llvm.trap()
  unreachable
}

define void @lilac_main() {
entry:
  %a = alloca i32, align 4
  store i32 0, ptr %a, align 4
  %b = alloca i8, align 1
  store i8 0, ptr %b, align 1
  %tmp.1 = alloca i8, align 1
  store i8 0, ptr %tmp.1, align 1
  store i32 1, ptr %a, align 4
  store i8 1, ptr %tmp.1, align 1
  %0 = load i8, ptr %tmp.1, align 1
  store i8 %0, ptr %b, align 1
  ret void
}

define i32 @main() {
entry:
  call void @lilac_main()
  ret i32 0
}

attributes #0 = { nocallback nofree nounwind willreturn memory(argmem: readwrite) }
attributes #1 = { cold noreturn nounwind memory(inaccessiblemem: write) }
