//! The hardware's numeric operations (math plan phase 1): `rotate.*`,
//! the overflow tests, saturating and high multiplies, the one-operand
//! bit and float operations, `f.mul.add`, `f.min`/`f.max`/`f.copy.sign`
//! and the number conversions.
//!
//! Integer operations are LLVM intrinsics, which every target expands
//! inline. Float ones must not become C library calls (`floor`, `fma`,
//! `fmin`), which a native program does not link: aarch64 has an
//! instruction for each, and elsewhere (x86-64's baseline has no SSE4.1 or
//! FMA) the backend calls its own internal routines, IR text in [`MATH`]
//! and [`FUSED`]. A 16-bit float operation runs at 32 bits between exact
//! conversions, except the fused multiply-add, which rounds to odd at 53
//! and then 24 bits so the last rounding is the only one that counts.
//! Half conversions go through `float` too: LLVM's direct ones call
//! `__truncdfhf2`/`__extendhfdf2`, which native mode does not define.

use inkwell::builder::Builder;
use inkwell::intrinsics::Intrinsic;
use inkwell::module::Linkage;
use inkwell::types::{BasicTypeEnum, IntType};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, FloatValue, FunctionValue, IntValue, ValueKind,
};

use lilac_error::bug;
use lilac_ir::{ArithOp, ConvertOp, LirTy, Reg, UnaryOp};
use lilac_target::Arch;

use super::{FnState, Lower};
use crate::threads::append_ir;

impl<'ctx> Lower<'ctx, '_> {
    /// Whether the target's baseline has the float rounding, fused
    /// multiply-add and IEEE `minimum`/`maximum` instructions.
    fn float_instructions(&self) -> bool {
        self.arch == Some(Arch::Aarch64)
    }

    /// Calls LLVM intrinsic `name` overloaded at `types`.
    fn intrinsic(
        &self,
        name: &str,
        types: &[BasicTypeEnum<'ctx>],
        args: &[BasicMetadataValueEnum<'ctx>],
        builder: &Builder<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let Some(function) = Intrinsic::find(name)
            .and_then(|intrinsic| intrinsic.get_declaration(&self.module, types))
        else {
            bug!(self.at(), "no LLVM intrinsic `{name}`");
        };
        self.call_value(function, args, builder, name)
    }

    /// Calls `function` for its value.
    fn call_value(
        &self,
        function: FunctionValue<'ctx>,
        args: &[BasicMetadataValueEnum<'ctx>],
        builder: &Builder<'ctx>,
        what: &str,
    ) -> BasicValueEnum<'ctx> {
        let call = self.checked(builder.build_call(function, args, ""), what);
        match call.try_as_basic_value() {
            ValueKind::Basic(value) => value,
            ValueKind::Instruction(_) => bug!(self.at(), "`{what}` returned nothing"),
        }
    }

    /// The integer type of `bits` bits (8 to 128).
    fn int_of(&self, bits: u32) -> IntType<'ctx> {
        match bits {
            8 => self.context.i8_type(),
            16 => self.context.i16_type(),
            32 => self.context.i32_type(),
            64 => self.context.i64_type(),
            128 => self.context.i128_type(),
            _ => bug!(self.at(), "no {bits}-bit integer type"),
        }
    }

    /// The width of float `value`'s type.
    fn float_width(&self, value: FloatValue<'ctx>) -> u32 {
        let ty = value.get_type();
        if ty == self.context.f16_type() {
            16
        } else if ty == self.context.f32_type() {
            32
        } else if ty == self.context.f64_type() {
            64
        } else {
            bug!(
                self.at(),
                "no numeric operation on a `{}`",
                ty.print_to_string()
            )
        }
    }

    /// `value` converted (exactly) to a float of `bits` bits, through
    /// `float` from or to a half.
    fn float_to(
        &self,
        value: FloatValue<'ctx>,
        bits: u32,
        builder: &Builder<'ctx>,
    ) -> FloatValue<'ctx> {
        let from = self.float_width(value);
        if from == bits {
            return value;
        }
        let target = match bits {
            16 => self.context.f16_type(),
            32 => self.context.f32_type(),
            _ => self.context.f64_type(),
        };
        if from < bits {
            let value = if from == 16 && bits == 64 {
                self.float_to(value, 32, builder)
            } else {
                value
            };
            self.checked(builder.build_float_ext(value, target, ""), "fpext")
        } else {
            self.checked(builder.build_float_trunc(value, target, ""), "fptrunc")
        }
    }

    /// One of the routines in [`MATH`] (`floor`, ... at 32 or 64 bits) or
    /// [`FUSED`], defined the first time a module needs its group.
    fn math_routine(&self, name: &str) -> FunctionValue<'ctx> {
        if let Some(function) = self.module.get_function(name) {
            return function;
        }
        let (group, text) = if name.ends_with(".32") {
            (
                "math.32",
                MATH.replace("FTY", "float").replace("ITY", "i32"),
            )
        } else if name.ends_with(".64") && name != ".lilac.fma.64" {
            (
                "math.64",
                MATH.replace("FTY", "double").replace("ITY", "i64"),
            )
        } else {
            ("fused", FUSED.to_owned())
        };
        let constants: &[(&str, &str)] = match group {
            "math.32" => &[
                ("WIDTH", "32"),
                ("MBITS", "23"),
                ("EXPMASK", "255"),
                ("BIAS", "127"),
                ("SIGNMASK", "-2147483648"),
                ("ABSMASK", "2147483647"),
                ("MANTMASK", "8388607"),
                ("ONEBITS", "1065353216"),
            ],
            "math.64" => &[
                ("WIDTH", "64"),
                ("MBITS", "52"),
                ("EXPMASK", "2047"),
                ("BIAS", "1023"),
                ("SIGNMASK", "-9223372036854775808"),
                ("ABSMASK", "9223372036854775807"),
                ("MANTMASK", "4503599627370495"),
                ("ONEBITS", "4607182418800017408"),
            ],
            _ => &[],
        };
        let mut text = text;
        for (token, value) in constants {
            text = text.replace(token, value);
        }
        let before: Vec<_> = self.module.get_functions().collect();
        // Linked as external definitions (the linker drops unreferenced
        // internal ones), then made internal.
        append_ir(&self.module, group, &text);
        for function in self.module.get_functions() {
            if !before.contains(&function) && function.count_basic_blocks() > 0 {
                function.set_linkage(Linkage::Internal);
            }
        }
        match self.module.get_function(name) {
            Some(function) => function,
            None => bug!(self.at(), "`{name}` was not defined"),
        }
    }

    /// Reinterprets float `value` as its bits, applies `op`, and back.
    fn float_bits_op(
        &self,
        value: FloatValue<'ctx>,
        builder: &Builder<'ctx>,
        op: impl FnOnce(IntValue<'ctx>, u32) -> IntValue<'ctx>,
    ) -> FloatValue<'ctx> {
        let bits = self.float_width(value);
        let int = self.int_of(bits);
        let BasicValueEnum::IntValue(raw) =
            self.checked(builder.build_bit_cast(value, int, ""), "bitcast")
        else {
            bug!(self.at(), "a float's bits are no integer");
        };
        let result = op(raw, bits);
        let BasicValueEnum::FloatValue(result) = self.checked(
            builder.build_bit_cast(result, value.get_type(), ""),
            "bitcast",
        ) else {
            bug!(self.at(), "an integer's float is no float");
        };
        result
    }

    /// The sign bit of a `bits`-bit float, as an integer of that width.
    fn sign_mask(&self, bits: u32) -> IntValue<'ctx> {
        self.int_of(bits).const_int(1 << (bits - 1), false)
    }

    /// Lowers `unary`.
    pub(super) fn lower_unary(
        &self,
        op: UnaryOp,
        src: &Reg,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        if !op.is_float() {
            let value = self.reg_int(src, state);
            let types = [value.get_type().into()];
            let mut args: Vec<BasicMetadataValueEnum<'ctx>> = vec![value.into()];
            let name = match op {
                UnaryOp::CountOnes => "llvm.ctpop",
                UnaryOp::LeadingZeros | UnaryOp::TrailingZeros => {
                    // A zero operand is defined: the width.
                    args.push(self.context.bool_type().const_zero().into());
                    if op == UnaryOp::LeadingZeros {
                        "llvm.ctlz"
                    } else {
                        "llvm.cttz"
                    }
                }
                UnaryOp::SwapBytes => "llvm.bswap",
                _ => "llvm.bitreverse",
            };
            return self.intrinsic(name, &types, &args, builder);
        }
        let value = self.reg_float(src, state);
        if op == UnaryOp::FAbs {
            return self
                .float_bits_op(value, builder, |raw, bits| {
                    let mask = self.sign_mask(bits).const_not();
                    self.checked(builder.build_and(raw, mask, ""), "and")
                })
                .into();
        }
        let bits = self.float_width(value);
        let wide = self.float_to(value, bits.max(32), builder);
        let routine = match op {
            UnaryOp::FSqrt => "sqrt",
            UnaryOp::FFloor => "floor",
            UnaryOp::FCeil => "ceil",
            UnaryOp::FTrunc => "trunc",
            UnaryOp::FRound => "round",
            _ => "roundeven",
        };
        let result = if op == UnaryOp::FSqrt || self.float_instructions() {
            self.intrinsic(
                &format!("llvm.{routine}"),
                &[wide.get_type().into()],
                &[wide.into()],
                builder,
            )
        } else {
            let name = format!(".lilac.{routine}.{}", bits.max(32));
            self.call_value(self.math_routine(&name), &[wide.into()], builder, routine)
        };
        let BasicValueEnum::FloatValue(result) = result else {
            bug!(self.at(), "`{routine}` returned no float");
        };
        self.float_to(result, bits, builder).into()
    }

    /// Lowers `f.mul.add`: `a * b + c` rounded once.
    pub(super) fn lower_mul_add(
        &self,
        [a, b, c]: [&Reg; 3],
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let [a, b, c] = [a, b, c].map(|reg| self.reg_float(reg, state));
        let bits = self.float_width(a);
        if bits > 16 && self.float_instructions() {
            return self.intrinsic(
                "llvm.fma",
                &[a.get_type().into()],
                &[a.into(), b.into(), c.into()],
                builder,
            );
        }
        if bits == 64 {
            let fma = self.math_routine(".lilac.fma.64");
            return self.call_value(fma, &[a.into(), b.into(), c.into()], builder, "fma");
        }
        // The product of two 24-bit significands is exact in a `double`;
        // the sum rounded to odd at 53 bits then rounds correctly to 24
        // (and, through 24 bits rounded to odd, to 11).
        let [a, b, c] = [a, b, c].map(|value| self.float_to(value, 64, builder));
        let product = self.checked(builder.build_float_mul(a, b, ""), "fmul");
        let BasicValueEnum::FloatValue(sum) = self.call_value(
            self.math_routine(".lilac.add.odd"),
            &[product.into(), c.into()],
            builder,
            "add.odd",
        ) else {
            bug!(self.at(), "`.lilac.add.odd` returned no float");
        };
        self.narrow(sum, bits, builder).into()
    }

    /// `value` (a `double`) rounded correctly to `bits` bits: a half
    /// through `float` rounded to odd.
    fn narrow(
        &self,
        value: FloatValue<'ctx>,
        bits: u32,
        builder: &Builder<'ctx>,
    ) -> FloatValue<'ctx> {
        if bits != 16 {
            return self.float_to(value, bits, builder);
        }
        let BasicValueEnum::FloatValue(single) = self.call_value(
            self.math_routine(".lilac.narrow.odd"),
            &[value.into()],
            builder,
            "narrow.odd",
        ) else {
            bug!(self.at(), "`.lilac.narrow.odd` returned no float");
        };
        self.float_to(single, 16, builder)
    }

    /// The two-operand operations of phase 1, which `arith` leaves here:
    /// [`float_pair`](Self::float_pair)'s and [`int_pair`](Self::int_pair)'s,
    /// an overflow test's flag widened to `ty`.
    pub(super) fn numeric_pair(
        &self,
        op: &ArithOp,
        lhs: &Reg,
        rhs: &Reg,
        ty: &LirTy,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        match op {
            ArithOp::FMin | ArithOp::FMax | ArithOp::FCopySign => {
                self.float_pair(op, lhs, rhs, builder, state)
            }
            ArithOp::IAddOverflows
            | ArithOp::IAddOverflowsU
            | ArithOp::ISubOverflows
            | ArithOp::ISubOverflowsU
            | ArithOp::IMulOverflows
            | ArithOp::IMulOverflowsU => {
                let BasicValueEnum::IntValue(flag) = self.int_pair(op, lhs, rhs, builder, state)
                else {
                    bug!(self.at(), "an overflow test gave no flag");
                };
                self.widen_bool(flag, ty, builder)
            }
            _ => self.int_pair(op, lhs, rhs, builder, state),
        }
    }

    /// The float operations `arith` leaves here: `f.min`, `f.max` (IEEE
    /// `minimum`/`maximum`: `-0.0` below `+0.0`, a NaN if either is) and
    /// `f.copy.sign`.
    fn float_pair(
        &self,
        op: &ArithOp,
        lhs: &Reg,
        rhs: &Reg,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let left = self.reg_float(lhs, state);
        let right = self.reg_float(rhs, state);
        let bits = self.float_width(left);
        if *op == ArithOp::FCopySign {
            let sign = self.sign_mask(bits);
            let int = self.int_of(bits);
            let BasicValueEnum::IntValue(right) =
                self.checked(builder.build_bit_cast(right, int, ""), "bitcast")
            else {
                bug!(self.at(), "a float's bits are no integer");
            };
            let sign_of = self.checked(builder.build_and(right, sign, ""), "and");
            return self
                .float_bits_op(left, builder, |raw, _| {
                    let magnitude =
                        self.checked(builder.build_and(raw, sign.const_not(), ""), "and");
                    self.checked(builder.build_or(magnitude, sign_of, ""), "or")
                })
                .into();
        }
        let min = *op == ArithOp::FMin;
        if self.float_instructions() {
            let name = if min { "llvm.minimum" } else { "llvm.maximum" };
            return self.intrinsic(
                name,
                &[left.get_type().into()],
                &[left.into(), right.into()],
                builder,
            );
        }
        let wide = bits.max(32);
        let (left, right) = (
            self.float_to(left, wide, builder),
            self.float_to(right, wide, builder),
        );
        let name = format!(".lilac.{}.{wide}", if min { "fmin" } else { "fmax" });
        let BasicValueEnum::FloatValue(result) = self.call_value(
            self.math_routine(&name),
            &[left.into(), right.into()],
            builder,
            &name,
        ) else {
            bug!(self.at(), "`{name}` returned no float");
        };
        self.float_to(result, bits, builder).into()
    }

    /// The integer operations `arith` leaves here: rotates, the overflow
    /// tests (an `i1`, which `arith` widens), saturating and high
    /// multiplies.
    fn int_pair(
        &self,
        op: &ArithOp,
        lhs: &Reg,
        rhs: &Reg,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let left = self.reg_int(lhs, state);
        let right = self.reg_int(rhs, state);
        let ty = left.get_type();
        let name = match op {
            ArithOp::RotL => "llvm.fshl",
            ArithOp::RotR => "llvm.fshr",
            ArithOp::IAddSat => "llvm.sadd.sat",
            ArithOp::IAddSatU => "llvm.uadd.sat",
            ArithOp::ISubSat => "llvm.ssub.sat",
            ArithOp::ISubSatU => "llvm.usub.sat",
            ArithOp::IAddOverflows => "llvm.sadd.with.overflow",
            ArithOp::IAddOverflowsU => "llvm.uadd.with.overflow",
            ArithOp::ISubOverflows => "llvm.ssub.with.overflow",
            ArithOp::ISubOverflowsU => "llvm.usub.with.overflow",
            ArithOp::IMulOverflows => "llvm.smul.with.overflow",
            ArithOp::IMulOverflowsU => "llvm.umul.with.overflow",
            ArithOp::IMulHigh | ArithOp::IMulHighU => {
                let width = ty.get_bit_width();
                let double = self.int_of(width * 2);
                let signed = *op == ArithOp::IMulHigh;
                let extend = |value| {
                    if signed {
                        self.checked(builder.build_int_s_extend(value, double, ""), "sext")
                    } else {
                        self.checked(builder.build_int_z_extend(value, double, ""), "zext")
                    }
                };
                let product = self.checked(
                    builder.build_int_mul(extend(left), extend(right), ""),
                    "mul",
                );
                let high = self.checked(
                    builder.build_right_shift(
                        product,
                        double.const_int(u64::from(width), false),
                        false,
                        "",
                    ),
                    "lshr",
                );
                return self
                    .checked(builder.build_int_truncate(high, ty, ""), "trunc")
                    .into();
            }
            _ => bug!(self.at(), "`{op:?}` is no integer pair operation"),
        };
        if matches!(op, ArithOp::RotL | ArithOp::RotR) {
            return self.intrinsic(
                name,
                &[ty.into()],
                &[left.into(), left.into(), right.into()],
                builder,
            );
        }
        let value = self.intrinsic(name, &[ty.into()], &[left.into(), right.into()], builder);
        if !name.ends_with("with.overflow") {
            return value;
        }
        let BasicValueEnum::StructValue(pair) = value else {
            bug!(self.at(), "`{name}` returned no pair");
        };
        self.checked(builder.build_extract_value(pair, 1, ""), "extractvalue")
    }

    /// The conversions between integers and floats and between float
    /// widths. Float to integer saturates and makes a NaN zero.
    pub(super) fn number_convert(
        &self,
        op: &ConvertOp,
        src: &Reg,
        ty: &LirTy,
        builder: &Builder<'ctx>,
        state: &FnState<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let result_ty = self.lir_type(ty);
        match op {
            ConvertOp::IntToFloat | ConvertOp::UintToFloat => {
                let BasicTypeEnum::FloatType(float) = result_ty else {
                    bug!(self.at(), "`{}` to a `{ty}`", op.name());
                };
                let signed = *op == ConvertOp::IntToFloat;
                let mut value = self.reg_int(src, state);
                let half = float == self.context.f16_type();
                let float = if half { self.context.f32_type() } else { float };
                // An integer past 2^24 would round twice on its way to a
                // half through `float`; every one past 2^20 is infinite
                // as a half, so it is clamped first.
                if half && value.get_type().get_bit_width() > 24 {
                    let int = value.get_type();
                    let limit = int.const_int(1 << 20, false);
                    value = if signed {
                        let low = int.const_int(1 << 20, false).const_neg();
                        let high = self.int_minmax("llvm.smin", value, limit, builder);
                        self.int_minmax("llvm.smax", high, low, builder)
                    } else {
                        self.int_minmax("llvm.umin", value, limit, builder)
                    };
                }
                let converted = if signed {
                    builder.build_signed_int_to_float(value, float, "")
                } else {
                    builder.build_unsigned_int_to_float(value, float, "")
                };
                let converted = self.checked(converted, "int to float");
                if half {
                    self.float_to(converted, 16, builder).into()
                } else {
                    converted.into()
                }
            }
            ConvertOp::FloatToInt | ConvertOp::FloatToUint => {
                let BasicTypeEnum::IntType(int) = result_ty else {
                    bug!(self.at(), "`{}` to a `{ty}`", op.name());
                };
                let value = self.reg_float(src, state);
                let value = self.float_to(value, self.float_width(value).max(32), builder);
                let name = if *op == ConvertOp::FloatToInt {
                    "llvm.fptosi.sat"
                } else {
                    "llvm.fptoui.sat"
                };
                self.intrinsic(
                    name,
                    &[int.into(), value.get_type().into()],
                    &[value.into()],
                    builder,
                )
            }
            _ => {
                let value = self.reg_float(src, state);
                let bits = match ty {
                    LirTy::FBit(bits) => u32::from(*bits),
                    _ => bug!(self.at(), "`f.convert` to a `{ty}`"),
                };
                if self.float_width(value) == 64 {
                    self.narrow(value, bits, builder).into()
                } else {
                    self.float_to(value, bits, builder).into()
                }
            }
        }
    }

    /// `llvm.smin`/`smax`/`umin` of two integers.
    fn int_minmax(
        &self,
        name: &str,
        left: IntValue<'ctx>,
        right: IntValue<'ctx>,
        builder: &Builder<'ctx>,
    ) -> IntValue<'ctx> {
        let BasicValueEnum::IntValue(value) = self.intrinsic(
            name,
            &[left.get_type().into()],
            &[left.into(), right.into()],
            builder,
        ) else {
            bug!(self.at(), "`{name}` returned no integer");
        };
        value
    }
}

/// The float routines at one width (`FTY` `float` or `double`, `ITY` its
/// bits; the other tokens are its masks). `trunc` clears the fraction
/// bits below the exponent (`|x| < 1` keeps only the sign, an integral
/// value, an infinity or a NaN stays); `floor`, `ceil`, `round` (half
/// away from zero) and `roundeven` step from it by one, which is exact.
/// `fmin`/`fmax` are IEEE `minimum`/`maximum`: equal operands (`-0.0`
/// and `+0.0`) merge their bits, so the minimum is negative, and a NaN
/// operand gives a quiet NaN.
const MATH: &str = r"
define FTY @.lilac.trunc.WIDTH(FTY %x) #0 {
entry:
  %b = bitcast FTY %x to ITY
  %field = lshr ITY %b, MBITS
  %biased = and ITY %field, EXPMASK
  %e = sub ITY %biased, BIAS
  %integral = icmp sge ITY %e, MBITS
  %fraction = icmp slt ITY %e, 0
  %whole = or i1 %integral, %fraction
  %amount = select i1 %whole, ITY 0, ITY %e
  %drop = lshr ITY MANTMASK, %amount
  %keep = xor ITY %drop, -1
  %cleared = and ITY %b, %keep
  %sign = and ITY %b, SIGNMASK
  %small = select i1 %fraction, ITY %sign, ITY %cleared
  %bits = select i1 %integral, ITY %b, ITY %small
  %result = bitcast ITY %bits to FTY
  ret FTY %result
}

define FTY @.lilac.floor.WIDTH(FTY %x) #0 {
entry:
  %t = call FTY @.lilac.trunc.WIDTH(FTY %x)
  %below = fcmp olt FTY %x, %t
  %less = fsub FTY %t, 0x3FF0000000000000
  %result = select i1 %below, FTY %less, FTY %t
  ret FTY %result
}

define FTY @.lilac.ceil.WIDTH(FTY %x) #0 {
entry:
  %t = call FTY @.lilac.trunc.WIDTH(FTY %x)
  %above = fcmp ogt FTY %x, %t
  %more = fadd FTY %t, 0x3FF0000000000000
  %result = select i1 %above, FTY %more, FTY %t
  ret FTY %result
}

define FTY @.lilac.round.WIDTH(FTY %x) #0 {
entry:
  %t = call FTY @.lilac.trunc.WIDTH(FTY %x)
  %d = fsub FTY %x, %t
  %up = fcmp oge FTY %d, 0x3FE0000000000000
  %down = fcmp ole FTY %d, 0xBFE0000000000000
  %away = or i1 %up, %down
  %b = bitcast FTY %x to ITY
  %sign = and ITY %b, SIGNMASK
  %one.bits = or ITY %sign, ONEBITS
  %one = bitcast ITY %one.bits to FTY
  %moved = fadd FTY %t, %one
  %result = select i1 %away, FTY %moved, FTY %t
  ret FTY %result
}

define FTY @.lilac.roundeven.WIDTH(FTY %x) #0 {
entry:
  %t = call FTY @.lilac.trunc.WIDTH(FTY %x)
  %d = fsub FTY %x, %t
  %d.bits = bitcast FTY %d to ITY
  %ad.bits = and ITY %d.bits, ABSMASK
  %ad = bitcast ITY %ad.bits to FTY
  %more = fcmp ogt FTY %ad, 0x3FE0000000000000
  %tie = fcmp oeq FTY %ad, 0x3FE0000000000000
  %h = fmul FTY %t, 0x3FE0000000000000
  %ht = call FTY @.lilac.trunc.WIDTH(FTY %h)
  %odd = fcmp une FTY %ht, %h
  %tie.odd = and i1 %tie, %odd
  %away = or i1 %more, %tie.odd
  %b = bitcast FTY %x to ITY
  %sign = and ITY %b, SIGNMASK
  %one.bits = or ITY %sign, ONEBITS
  %one = bitcast ITY %one.bits to FTY
  %moved = fadd FTY %t, %one
  %result = select i1 %away, FTY %moved, FTY %t
  ret FTY %result
}

define FTY @.lilac.fmin.WIDTH(FTY %a, FTY %b) #0 {
entry:
  %lt = fcmp olt FTY %a, %b
  %gt = fcmp ogt FTY %a, %b
  %uno = fcmp uno FTY %a, %b
  %a.bits = bitcast FTY %a to ITY
  %b.bits = bitcast FTY %b to ITY
  %tied.bits = or ITY %a.bits, %b.bits
  %tied = bitcast ITY %tied.bits to FTY
  %nan = fadd FTY %a, %b
  %r1 = select i1 %gt, FTY %b, FTY %tied
  %r2 = select i1 %lt, FTY %a, FTY %r1
  %result = select i1 %uno, FTY %nan, FTY %r2
  ret FTY %result
}

define FTY @.lilac.fmax.WIDTH(FTY %a, FTY %b) #0 {
entry:
  %lt = fcmp olt FTY %a, %b
  %gt = fcmp ogt FTY %a, %b
  %uno = fcmp uno FTY %a, %b
  %a.bits = bitcast FTY %a to ITY
  %b.bits = bitcast FTY %b to ITY
  %tied.bits = and ITY %a.bits, %b.bits
  %tied = bitcast ITY %tied.bits to FTY
  %nan = fadd FTY %a, %b
  %r1 = select i1 %lt, FTY %b, FTY %tied
  %r2 = select i1 %gt, FTY %a, FTY %r1
  %result = select i1 %uno, FTY %nan, FTY %r2
  ret FTY %result
}

attributes #0 = { nounwind alwaysinline }
";

/// The fused multiply-add without an instruction.
///
/// `.lilac.add.odd(p, c)` is `p + c` rounded to odd at 53 bits: the sum
/// and its exact error by `TwoSum`, then, when the error is not zero and
/// the sum is even, the neighbour toward the error. `.lilac.narrow.odd`
/// rounds a `double` to odd at 24 bits the same way (the error is the
/// difference from the `float` widened back). Rounding to odd at
/// `p + 2` bits or more and then to nearest at `p` is rounding once.
///
/// `.lilac.fma.64` is exact integer arithmetic in `i128`: specials (an
/// infinite or NaN operand, a zero factor) are `x * y + z`, which is then
/// already right, but an infinite `z` with finite factors is `z` and a
/// zero `z` is the rounded product (whose sign stays). Otherwise the
/// product's 106-bit significand and `z`'s are each normalized to bit
/// 125, the smaller aligned to the larger with a sticky bit, added or
/// subtracted, normalized to bit 127 and rounded to nearest, ties to
/// even, at the normal or subnormal position. The bits are composed as
/// `((lsb exponent + 1074) << 52) + significand`, so a rounding carry
/// moves into the exponent, and clamped to infinity.
const FUSED: &str = r"
define double @.lilac.add.odd(double %p, double %c) #0 {
entry:
  %s = fadd double %p, %c
  %bb = fsub double %s, %p
  %t1 = fsub double %s, %bb
  %e1 = fsub double %p, %t1
  %e2 = fsub double %c, %bb
  %err = fadd double %e1, %e2
  %inexact = fcmp one double %err, 0.0
  %s.bits = bitcast double %s to i64
  %low = trunc i64 %s.bits to i1
  %even = xor i1 %low, true
  %adjust = and i1 %inexact, %even
  %err.bits = bitcast double %err to i64
  %signs = xor i64 %s.bits, %err.bits
  %differ = icmp slt i64 %signs, 0
  %step = select i1 %differ, i64 -1, i64 1
  %stepped = add i64 %s.bits, %step
  %bits = select i1 %adjust, i64 %stepped, i64 %s.bits
  %result = bitcast i64 %bits to double
  ret double %result
}

define float @.lilac.narrow.odd(double %v) #0 {
entry:
  %f = fptrunc double %v to float
  %back = fpext float %f to double
  %inexact = fcmp one double %back, %v
  %f.bits = bitcast float %f to i32
  %low = trunc i32 %f.bits to i1
  %even = xor i1 %low, true
  %adjust = and i1 %inexact, %even
  %up = fcmp ogt double %v, %back
  %negative = icmp slt i32 %f.bits, 0
  %grow = xor i1 %up, %negative
  %step = select i1 %grow, i32 1, i32 -1
  %stepped = add i32 %f.bits, %step
  %bits = select i1 %adjust, i32 %stepped, i32 %f.bits
  %result = bitcast i32 %bits to float
  ret float %result
}

define double @.lilac.fma.64(double %x, double %y, double %z) #1 {
entry:
  %bx = bitcast double %x to i64
  %by = bitcast double %y to i64
  %bz = bitcast double %z to i64
  %ax = and i64 %bx, 9223372036854775807
  %ay = and i64 %by, 9223372036854775807
  %az = and i64 %bz, 9223372036854775807
  %x.special = icmp uge i64 %ax, 9218868437227405312
  %y.special = icmp uge i64 %ay, 9218868437227405312
  %z.special = icmp uge i64 %az, 9218868437227405312
  %x.zero = icmp eq i64 %ax, 0
  %y.zero = icmp eq i64 %ay, 0
  %xy.special = or i1 %x.special, %y.special
  %xy.zero = or i1 %x.zero, %y.zero
  %plain = or i1 %xy.special, %xy.zero
  br i1 %plain, label %naive, label %check.z
naive:
  %naive.product = fmul double %x, %y
  %naive.sum = fadd double %naive.product, %z
  ret double %naive.sum
check.z:
  br i1 %z.special, label %z.infinite, label %check.zero
z.infinite:
  %quiet = fadd double %z, %z
  ret double %quiet
check.zero:
  %z.zero = icmp eq i64 %az, 0
  br i1 %z.zero, label %product.only, label %exact
product.only:
  %rounded = fmul double %x, %y
  ret double %rounded
exact:
  %fx = and i64 %bx, 4503599627370495
  %fy = and i64 %by, 4503599627370495
  %fz = and i64 %bz, 4503599627370495
  %Ex = lshr i64 %ax, 52
  %Ey = lshr i64 %ay, 52
  %Ez = lshr i64 %az, 52
  %x.sub = icmp eq i64 %Ex, 0
  %y.sub = icmp eq i64 %Ey, 0
  %z.sub = icmp eq i64 %Ez, 0
  %mx.n = or i64 %fx, 4503599627370496
  %my.n = or i64 %fy, 4503599627370496
  %mz.n = or i64 %fz, 4503599627370496
  %mx = select i1 %x.sub, i64 %fx, i64 %mx.n
  %my = select i1 %y.sub, i64 %fy, i64 %my.n
  %mz = select i1 %z.sub, i64 %fz, i64 %mz.n
  %Ex1 = select i1 %x.sub, i64 1, i64 %Ex
  %Ey1 = select i1 %y.sub, i64 1, i64 %Ey
  %Ez1 = select i1 %z.sub, i64 1, i64 %Ez
  %ex = sub i64 %Ex1, 1075
  %ey = sub i64 %Ey1, 1075
  %ez0 = sub i64 %Ez1, 1075
  %mx.w = zext i64 %mx to i128
  %my.w = zext i64 %my to i128
  %mz.w = zext i64 %mz to i128
  %P = mul i128 %mx.w, %my.w
  %ep0 = add i64 %ex, %ey
  %P.lead = call i128 @llvm.ctlz.i128(i128 %P, i1 true)
  %P.shift = sub i128 %P.lead, 2
  %Pn = shl i128 %P, %P.shift
  %P.shift64 = trunc i128 %P.shift to i64
  %ep = sub i64 %ep0, %P.shift64
  %Z.lead = call i128 @llvm.ctlz.i128(i128 %mz.w, i1 true)
  %Z.shift = sub i128 %Z.lead, 2
  %Zn = shl i128 %mz.w, %Z.shift
  %Z.shift64 = trunc i128 %Z.shift to i64
  %ez = sub i64 %ez0, %Z.shift64
  %sp0 = xor i64 %bx, %by
  %sp = and i64 %sp0, -9223372036854775808
  %sz = and i64 %bz, -9223372036854775808
  %e.greater = icmp sgt i64 %ep, %ez
  %e.equal = icmp eq i64 %ep, %ez
  %m.ge = icmp uge i128 %Pn, %Zn
  %equal.ge = and i1 %e.equal, %m.ge
  %p.big = or i1 %e.greater, %equal.ge
  %big = select i1 %p.big, i128 %Pn, i128 %Zn
  %small = select i1 %p.big, i128 %Zn, i128 %Pn
  %eb = select i1 %p.big, i64 %ep, i64 %ez
  %es = select i1 %p.big, i64 %ez, i64 %ep
  %sb = select i1 %p.big, i64 %sp, i64 %sz
  %d = sub i64 %eb, %es
  %far = icmp ugt i64 %d, 127
  %d.c = select i1 %far, i64 127, i64 %d
  %d.w = zext i64 %d.c to i128
  %shifted = lshr i128 %small, %d.w
  %back = shl i128 %shifted, %d.w
  %lost = icmp ne i128 %back, %small
  %sticky = zext i1 %lost to i128
  %aligned = or i128 %shifted, %sticky
  %same = icmp eq i64 %sp, %sz
  %sum = add i128 %big, %aligned
  %diff = sub i128 %big, %aligned
  %R = select i1 %same, i128 %sum, i128 %diff
  %R.zero = icmp eq i128 %R, 0
  br i1 %R.zero, label %cancel, label %round
cancel:
  ret double 0.0
round:
  %R.lead = call i128 @llvm.ctlz.i128(i128 %R, i1 true)
  %Rn = shl i128 %R, %R.lead
  %R.lead64 = trunc i128 %R.lead to i64
  %e = sub i64 %eb, %R.lead64
  %sub.s = sub i64 -1074, %e
  %subnormal = icmp sgt i64 %sub.s, 75
  %s0 = select i1 %subnormal, i64 %sub.s, i64 75
  %tiny = icmp sgt i64 %s0, 128
  %s = select i1 %tiny, i64 128, i64 %s0
  %s.w = zext i64 %s to i128
  %s.less = sub i128 %s.w, 1
  %R1 = lshr i128 %Rn, 1
  %q0 = lshr i128 %R1, %s.less
  %half = shl i128 1, %s.less
  %unit = shl i128 %half, 1
  %mask = sub i128 %unit, 1
  %rem = and i128 %Rn, %mask
  %above = icmp ugt i128 %rem, %half
  %tie = icmp eq i128 %rem, %half
  %q.odd = trunc i128 %q0 to i1
  %tie.up = and i1 %tie, %q.odd
  %up0 = or i1 %above, %tie.up
  %not.tiny = xor i1 %tiny, true
  %up = and i1 %up0, %not.tiny
  %up.w = zext i1 %up to i128
  %q = add i128 %q0, %up.w
  %q64 = trunc i128 %q to i64
  %e.lsb = add i64 %e, %s
  %biased = add i64 %e.lsb, 1074
  %high = shl i64 %biased, 52
  %bits0 = add i64 %high, %q64
  %bits1 = select i1 %tiny, i64 0, i64 %bits0
  %bits2 = call i64 @llvm.umin.i64(i64 %bits1, i64 9218868437227405312)
  %bits = or i64 %bits2, %sb
  %result = bitcast i64 %bits to double
  ret double %result
}

declare i128 @llvm.ctlz.i128(i128, i1)
declare i64 @llvm.umin.i64(i64, i64)

attributes #0 = { nounwind alwaysinline }
attributes #1 = { nounwind }
";
