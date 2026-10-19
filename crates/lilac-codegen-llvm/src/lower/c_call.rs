//! C ABI signatures, calls and `export` entries: each slot classified by
//! the [`crate::abi`] rules for the target, so an object passes by value
//! as the target's C compiler passes the struct.
//!
//! A scalar slot is its own LLVM type. An object travels as the parts the
//! classifier picks, loaded from (or stored to) its bytes, as the address
//! of a copy (`byval`, or a plain pointer to a caller's copy), and a result
//! that does not come back in registers is written through a hidden first
//! parameter (`sret`). Every signature is built once by
//! [`Lower::c_signature`], which declarations, calls and entries share.

use std::num::NonZeroU32;

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::builder::Builder;
use inkwell::types::{AnyType, BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, CallSiteValue, FunctionValue, PointerValue,
    ValueKind,
};

use lilac_error::bug;
use lilac_ir::{builtin_layout, LirExport, LirLayout, LirLayoutKind, LirTy, PlaceName};

use super::{FnState, Lower};
use crate::abi::{self, Pass, Piece, Return, Shape};

/// A C signature as LLVM sees it: the function type, how each slot
/// travels, and where each slot's LLVM parameters start.
pub(super) struct CSignature<'ctx> {
    pub(super) fn_type: FunctionType<'ctx>,
    /// The slot types, one per declared parameter.
    params: Vec<LirTy>,
    ret: Option<LirTy>,
    abi: abi::Signature,
    /// The first LLVM parameter of each slot (after an `sret`).
    starts: Vec<u32>,
}

/// What a C call calls.
#[derive(Clone, Copy)]
pub(super) enum CTarget<'ctx> {
    Func(FunctionValue<'ctx>),
    Pointer(PointerValue<'ctx>),
}

/// The alignment an access at `offset` into an object aligned to `align`
/// may claim.
fn align_at(align: u64, offset: u64) -> u32 {
    let at = if offset == 0 {
        align
    } else {
        align.min(1 << offset.trailing_zeros())
    };
    u32::try_from(at).unwrap_or(1)
}

impl<'ctx> Lower<'ctx, '_> {
    /// The C type of a LIR slot type, for the classifier.
    fn shape_of(&self, ty: &LirTy) -> Shape {
        let target = self.lir.target;
        match ty {
            LirTy::Bit(bits) => Shape::Int(u32::from(*bits)),
            LirTy::BitNative => Shape::Int(target.bit_native_width()),
            LirTy::FBit(bits) => Shape::Float(u32::from(*bits)),
            LirTy::Ref(_) => Shape::Ptr,
            LirTy::Array(elem, len) => Shape::Array {
                elem: Box::new(self.shape_of(elem)),
                len: *len,
            },
            LirTy::Named(name) => {
                let layout = match self.lir.layouts.get(name.as_str()) {
                    Some(layout) => layout.clone(),
                    None => builtin_layout(ty, &target)
                        .unwrap_or_else(|| bug!(self.at(), "`{ty}` has no layout")),
                };
                self.layout_shape(&layout)
            }
            #[allow(unreachable_patterns, reason = "future LIR types")]
            other => bug!(self.at(), "`{other}` is not a C type"),
        }
    }

    /// A struct as its fields; a tagged union as its tag and the union of
    /// its payloads, the C `struct { tag; union { … } payload; }`.
    fn layout_shape(&self, layout: &LirLayout) -> Shape {
        let target = self.lir.target;
        match layout.kind {
            LirLayoutKind::Struct => Shape::Struct {
                size: layout.size,
                align: layout.align,
                fields: layout
                    .fields
                    .iter()
                    .map(|field| (field.offset, self.shape_of(&field.ty)))
                    .collect(),
            },
            LirLayoutKind::Tagged => {
                let mut fields = Vec::new();
                let mut payloads = layout.fields.iter();
                if let Some(tag) = payloads.next() {
                    fields.push((tag.offset, self.shape_of(&tag.ty)));
                }
                let members: Vec<Shape> = payloads.map(|f| self.shape_of(&f.ty)).collect();
                if let Some(offset) = layout.fields.get(1).map(|field| field.offset) {
                    let align = members.iter().map(|m| m.align(target)).max().unwrap_or(1);
                    let size = members.iter().map(|m| m.size(target)).max().unwrap_or(0);
                    fields.push((
                        offset,
                        Shape::Union {
                            size: size.div_ceil(align) * align,
                            align,
                            members,
                        },
                    ));
                }
                Shape::Struct {
                    size: layout.size,
                    align: layout.align,
                    fields,
                }
            }
        }
    }

    fn piece_type(&self, piece: &Piece) -> BasicTypeEnum<'ctx> {
        match piece {
            Piece::Int(bits) => {
                let bits = NonZeroU32::new(*bits)
                    .unwrap_or_else(|| bug!(self.at(), "a zero-width C integer part"));
                match self.context.custom_width_int_type(bits) {
                    Ok(int) => int.into(),
                    Err(err) => bug!(self.at(), "no {bits}-bit integer: {err}"),
                }
            }
            Piece::Float(16) => self.context.f16_type().into(),
            Piece::Float(32) => self.context.f32_type().into(),
            Piece::Float(64) => self.context.f64_type().into(),
            Piece::Float(_) => self.context.f128_type().into(),
            Piece::Ptr => self.ptr_type().into(),
            Piece::Vector { bits, count } => {
                let float = if *bits == 16 {
                    self.context.f16_type()
                } else if *bits == 32 {
                    self.context.f32_type()
                } else {
                    self.context.f64_type()
                };
                float.vec_type(*count).into()
            }
            Piece::Array(elem, count) => self.piece_type(elem).array_type(*count).into(),
        }
    }

    /// The C signature of slots `params` returning `ret` on the target.
    pub(super) fn c_signature(
        &self,
        params: &[LirTy],
        ret: Option<&LirTy>,
        variadic: bool,
    ) -> CSignature<'ctx> {
        let shapes: Vec<Shape> = params.iter().map(|ty| self.shape_of(ty)).collect();
        let ret_shape = ret.map(|ty| self.shape_of(ty));
        let abi = abi::classify(self.lir.target, &shapes, ret_shape.as_ref());
        let mut types: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        if matches!(abi.ret, Return::Indirect { .. }) {
            types.push(self.ptr_type().into());
        }
        let mut starts = Vec::with_capacity(params.len());
        for (ty, pass) in params.iter().zip(&abi.params) {
            starts.push(u32::try_from(types.len()).unwrap_or(u32::MAX));
            match pass {
                Pass::Scalar => types.push(self.lir_type(ty).into()),
                Pass::Direct { parts, .. } => {
                    types.extend(
                        parts
                            .iter()
                            .map(|part| BasicMetadataTypeEnum::from(self.piece_type(&part.piece))),
                    );
                }
                Pass::Indirect { .. } => types.push(self.ptr_type().into()),
            }
        }
        let fn_type = match (&abi.ret, ret) {
            (Return::Scalar, Some(ty)) => self.lir_type(ty).fn_type(&types, variadic),
            (Return::Direct(parts), _) => match parts.as_slice() {
                [part] => self.piece_type(&part.piece).fn_type(&types, variadic),
                parts => {
                    let fields: Vec<BasicTypeEnum<'ctx>> = parts
                        .iter()
                        .map(|part| self.piece_type(&part.piece))
                        .collect();
                    self.context
                        .struct_type(&fields, false)
                        .fn_type(&types, variadic)
                }
            },
            _ => self.context.void_type().fn_type(&types, variadic),
        };
        CSignature {
            fn_type,
            params: params.to_vec(),
            ret: ret.cloned(),
            abi,
            starts,
        }
    }

    /// The parameter attributes the C ABI needs (`sret`, `byval`, `align`,
    /// `alignstack`), which a declaration and each call both carry.
    fn c_attributes(&self, sig: &CSignature<'ctx>) -> Vec<(AttributeLoc, Attribute)> {
        let kind = Attribute::get_named_enum_kind_id;
        let mut attributes = Vec::new();
        let memory = |at: u32, attribute: &str, ty: &LirTy, align: u64| {
            let bytes = self.bytes_type(self.size_align(ty).size);
            [
                (
                    AttributeLoc::Param(at),
                    self.context
                        .create_type_attribute(kind(attribute), bytes.as_any_type_enum()),
                ),
                (
                    AttributeLoc::Param(at),
                    self.context.create_enum_attribute(kind("align"), align),
                ),
            ]
        };
        if let (Return::Indirect { align }, Some(ty)) = (&sig.abi.ret, &sig.ret) {
            attributes.extend(memory(0, "sret", ty, *align));
        }
        for ((pass, ty), start) in sig.abi.params.iter().zip(&sig.params).zip(&sig.starts) {
            match pass {
                Pass::Indirect { byval: true, align } => {
                    attributes.extend(memory(*start, "byval", ty, *align));
                }
                Pass::Direct {
                    align_stack: Some(align),
                    ..
                } => attributes.push((
                    AttributeLoc::Param(*start),
                    self.context
                        .create_enum_attribute(kind("alignstack"), *align),
                )),
                _ => {}
            }
        }
        attributes
    }

    /// Adds the ABI attributes to a C function's declaration.
    pub(super) fn add_c_attributes(&self, function: FunctionValue<'ctx>, sig: &CSignature<'ctx>) {
        for (loc, attribute) in self.c_attributes(sig) {
            function.add_attribute(loc, attribute);
        }
    }

    /// The signature of an `extern`'s C function.
    pub(super) fn extern_signature(&self, func: &lilac_ir::LirFunc) -> CSignature<'ctx> {
        let params: Vec<LirTy> = func.ins.iter().map(|(_, ty)| ty.clone()).collect();
        self.c_signature(&params, func.outs.first().map(|(_, ty)| ty), func.variadic)
    }

    /// The signature of an `export`'s C entry.
    pub(super) fn export_signature(&self, export: &LirExport) -> CSignature<'ctx> {
        self.c_signature(&export.ins, export.outs.first(), false)
    }

    /// A stack slot of `size` bytes aligned to `align`, in the entry block.
    fn c_slot(&self, builder: &Builder<'ctx>, size: u64, align: u64) -> PointerValue<'ctx> {
        let slot = self.entry_alloca(builder, self.bytes_type(size));
        self.aligned(
            slot.as_instruction(),
            u32::try_from(align).unwrap_or(1),
            "C slot",
        );
        slot
    }

    /// Loads the parts of the object at `ptr` (of type `ty`). Parts that
    /// reach past the object (an `i24` read as part of an eightbyte, an
    /// ARM `[2 x i32]` of a 6-byte object) read a padded copy instead.
    fn load_parts(
        &self,
        builder: &Builder<'ctx>,
        ptr: PointerValue<'ctx>,
        ty: &LirTy,
        parts: &[abi::Part],
    ) -> Vec<BasicValueEnum<'ctx>> {
        let sized = self.size_align(ty);
        let extent = abi::extent(parts, self.lir.target);
        let base = if extent > sized.size {
            let copy = self.c_slot(builder, extent, sized.align);
            self.copy_bytes(builder, copy, ptr, ty);
            copy
        } else {
            ptr
        };
        parts
            .iter()
            .map(|part| {
                let address = self.part_address(builder, base, part.offset);
                let value = self.checked(
                    builder.build_load(self.piece_type(&part.piece), address, ""),
                    "C part load",
                );
                self.aligned(
                    value.as_instruction_value(),
                    align_at(sized.align, part.offset),
                    "C part load",
                );
                value
            })
            .collect()
    }

    /// Stores the parts `values` into the object at `ptr` (of type `ty`),
    /// through a padded copy when they reach past it.
    fn store_parts(
        &self,
        builder: &Builder<'ctx>,
        ptr: PointerValue<'ctx>,
        ty: &LirTy,
        parts: &[abi::Part],
        values: &[BasicValueEnum<'ctx>],
    ) {
        let sized = self.size_align(ty);
        let extent = abi::extent(parts, self.lir.target);
        let base = if extent > sized.size {
            self.c_slot(builder, extent, sized.align)
        } else {
            ptr
        };
        for (part, value) in parts.iter().zip(values) {
            let address = self.part_address(builder, base, part.offset);
            let store = self.checked(builder.build_store(address, *value), "C part store");
            self.aligned(
                Some(store),
                align_at(sized.align, part.offset),
                "C part store",
            );
        }
        if base != ptr {
            self.copy_bytes(builder, ptr, base, ty);
        }
    }

    fn part_address(
        &self,
        builder: &Builder<'ctx>,
        base: PointerValue<'ctx>,
        offset: u64,
    ) -> PointerValue<'ctx> {
        if offset == 0 {
            base
        } else {
            self.offset_ptr(builder, base, self.native.const_int(offset, false))
        }
    }

    /// The address of the object an argument place holds: the place
    /// itself, or what a `ref` place points to.
    fn object_ptr(
        &self,
        place: &PlaceName,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> PointerValue<'ctx> {
        if state.place_types.get(place).is_some_and(LirTy::is_ref) {
            self.load_place(place, builder, state).into_pointer_value()
        } else {
            self.place_ptr(place, state)
        }
    }

    /// A C call of `target` with signature `sig`: each `in` place passed
    /// as its slot travels (a variadic call's extra places loaded at their
    /// own types), and the result stored into the `out` place.
    pub(super) fn c_call(
        &self,
        target: CTarget<'ctx>,
        sig: &CSignature<'ctx>,
        ins: &[PlaceName],
        outs: &[PlaceName],
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) {
        let mut args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        let ret_slot = match (&sig.abi.ret, &sig.ret) {
            (Return::Indirect { align }, Some(ty)) => {
                let slot = self.c_slot(builder, self.size_align(ty).size, *align);
                args.push(slot.into());
                Some(slot)
            }
            _ => None,
        };
        for ((place, ty), pass) in ins.iter().zip(&sig.params).zip(&sig.abi.params) {
            match pass {
                Pass::Scalar if ty.is_ref() => {
                    let arg_is_ref = state.place_types.get(place).is_some_and(LirTy::is_ref);
                    args.push(if arg_is_ref {
                        self.load_place(place, builder, state).into()
                    } else {
                        self.place_ptr(place, state).into()
                    });
                }
                Pass::Scalar => args.push(self.load_as(place, ty, builder, state).into()),
                Pass::Direct { parts, .. } => {
                    let ptr = self.object_ptr(place, builder, state);
                    let values = self.load_parts(builder, ptr, ty, parts);
                    args.extend(values.into_iter().map(BasicMetadataValueEnum::from));
                }
                Pass::Indirect { byval, align } => {
                    let ptr = self.object_ptr(place, builder, state);
                    let sized = self.size_align(ty);
                    // A `byval` argument is copied by the call itself, from
                    // memory aligned as the attribute says; any other is the
                    // address of a copy the callee may change.
                    let arg = if *byval && sized.align >= *align {
                        ptr
                    } else {
                        let copy = self.c_slot(builder, sized.size, sized.align.max(*align));
                        self.copy_bytes(builder, copy, ptr, ty);
                        copy
                    };
                    args.push(arg.into());
                }
            }
        }
        args.extend(
            ins.iter()
                .skip(sig.params.len())
                .map(|place| BasicMetadataValueEnum::from(self.load_place(place, builder, state))),
        );
        let call: CallSiteValue<'ctx> = match target {
            CTarget::Func(function) => {
                self.checked(builder.build_call(function, &args, ""), "C call")
            }
            CTarget::Pointer(ptr) => self.checked(
                builder.build_indirect_call(sig.fn_type, ptr, &args, ""),
                "C indirect call",
            ),
        };
        for (loc, attribute) in self.c_attributes(sig) {
            call.add_attribute(loc, attribute);
        }
        let (Some(out), Some(ty)) = (outs.first(), &sig.ret) else {
            return;
        };
        let out_ptr = self.place_ptr(out, state);
        let result = || match call.try_as_basic_value() {
            ValueKind::Basic(value) => value,
            ValueKind::Instruction(_) => bug!(self.at(), "a C call returned nothing"),
        };
        match &sig.abi.ret {
            Return::Void => {}
            Return::Scalar => self.store(builder, out_ptr, result(), "C result store"),
            Return::Direct(parts) => {
                let values = self.split_result(builder, result(), parts.len());
                self.store_parts(builder, out_ptr, ty, parts, &values);
            }
            Return::Indirect { .. } => {
                if let Some(slot) = ret_slot {
                    self.copy_bytes(builder, out_ptr, slot, ty);
                }
            }
        }
    }

    /// The parts of a returned value: itself, or the fields of a pair.
    fn split_result(
        &self,
        builder: &Builder<'ctx>,
        value: BasicValueEnum<'ctx>,
        count: usize,
    ) -> Vec<BasicValueEnum<'ctx>> {
        if count == 1 {
            return vec![value];
        }
        let BasicValueEnum::StructValue(pair) = value else {
            bug!(self.at(), "a C pair result that is not a struct");
        };
        (0..u32::try_from(count).unwrap_or(0))
            .map(|index| {
                self.checked(
                    builder.build_extract_value(pair, index, ""),
                    "C pair result",
                )
            })
            .collect()
    }

    /// The C entry of an `export`: `symbol` with its C signature puts each
    /// argument in a place (a scalar or the parts in a stack slot; a copy
    /// passed by address is its own place), calls the function with the
    /// place ABI, and returns the out (in registers, or written to the
    /// caller's `sret` memory, which is then the out place).
    pub(super) fn emit_export(&self, func: &lilac_ir::LirFunc, export: &LirExport) {
        let symbol = export.symbol.as_str();
        let Some(entry) = self.module.get_function(symbol) else {
            bug!(self.at(), "export `{symbol}` was not declared");
        };
        let Some(body) = self.module.get_function(super::symbol(func.name.as_str())) else {
            bug!(self.at(), "export of undeclared `{}`", func.name);
        };
        let sig = self.export_signature(export);
        let builder = self.context.create_builder();
        builder.position_at_end(self.context.append_basic_block(entry, "entry"));
        let param = |index: u32| {
            entry
                .get_nth_param(index)
                .unwrap_or_else(|| bug!(self.at(), "export `{symbol}` lost a parameter"))
        };
        let mut slots: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
        for ((ty, pass), start) in sig.params.iter().zip(&sig.abi.params).zip(&sig.starts) {
            let slot = match pass {
                Pass::Scalar => {
                    let slot = self.checked(
                        builder.build_alloca(self.lir_type(ty), "arg"),
                        "export argument",
                    );
                    self.store(&builder, slot, param(*start), "export argument store");
                    slot
                }
                Pass::Direct { parts, .. } => {
                    let sized = self.size_align(ty);
                    let size = sized.size.max(abi::extent(parts, self.lir.target));
                    let slot = self.c_slot(&builder, size, sized.align);
                    let values: Vec<BasicValueEnum<'ctx>> = (0..parts.len())
                        .map(|index| param(*start + u32::try_from(index).unwrap_or(0)))
                        .collect();
                    self.store_parts(&builder, slot, ty, parts, &values);
                    slot
                }
                Pass::Indirect { .. } => param(*start).into_pointer_value(),
            };
            slots.push(slot.into());
        }
        let out = sig.ret.as_ref().map(|ty| match &sig.abi.ret {
            Return::Indirect { .. } => param(0).into_pointer_value(),
            Return::Direct(parts) => {
                let sized = self.size_align(ty);
                let size = sized.size.max(abi::extent(parts, self.lir.target));
                self.c_slot(&builder, size, sized.align)
            }
            _ => self.checked(builder.build_alloca(self.lir_type(ty), "out"), "export out"),
        });
        slots.extend(out.map(BasicMetadataValueEnum::from));
        self.checked(builder.build_call(body, &slots, ""), "export call");
        let value = match (&sig.abi.ret, &sig.ret, out) {
            (Return::Scalar, Some(ty), Some(out)) => {
                Some(self.load(&builder, self.lir_type(ty), out, "export result"))
            }
            (Return::Direct(parts), Some(ty), Some(out)) => {
                let values = self.load_parts(&builder, out, ty, parts);
                Some(self.join_result(&builder, sig.fn_type, values))
            }
            _ => None,
        };
        let ret = value.as_ref().map(|value| value as &dyn BasicValue);
        self.checked(builder.build_return(ret), "export return");
    }

    /// One part as itself, or a pair as the literal struct the signature
    /// returns.
    fn join_result(
        &self,
        builder: &Builder<'ctx>,
        fn_type: FunctionType<'ctx>,
        values: Vec<BasicValueEnum<'ctx>>,
    ) -> BasicValueEnum<'ctx> {
        if let [value] = values.as_slice() {
            return *value;
        }
        let Some(BasicTypeEnum::StructType(pair)) = fn_type.get_return_type() else {
            bug!(self.at(), "a C pair result without a struct type");
        };
        let mut joined = pair.get_undef();
        for (index, value) in values.into_iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(0);
            let built = self.checked(
                builder.build_insert_value(joined, value, index, ""),
                "C pair result",
            );
            joined = built.into_struct_value();
        }
        joined.into()
    }
}
