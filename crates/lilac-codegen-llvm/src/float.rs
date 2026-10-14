//! Float operations the backend writes itself rather than leave to the C
//! library.
//!
//! LLVM's `frem` becomes a call to the C library's `fmod`, which a native
//! program does not link (and a `--libc` one finds only in `libm`). So
//! `f.mod` calls `.lilac.fmod.32` or `.lilac.fmod.64`, internal functions
//! defined here as IR text the first time a module needs one; a 16-bit
//! remainder is the 32-bit one between conversions (the remainder is exact,
//! so nothing rounds twice).
//!
//! [`HALF`] is what x86-64 calls to convert half-precision floats, which
//! native mode defines (`native.rs`).

use inkwell::module::Linkage;
use inkwell::values::FunctionValue;

use lilac_error::bug;
use lilac_span::Span;

use crate::threads::append_ir;

/// `.lilac.fmod.<bits>(x, y)` for a 32- or 64-bit float, defined in
/// `module` the first time it is asked for.
pub(crate) fn remainder<'ctx>(
    module: &inkwell::module::Module<'ctx>,
    bits: u32,
) -> FunctionValue<'ctx> {
    let name = format!(".lilac.fmod.{bits}");
    if let Some(function) = module.get_function(&name) {
        return function;
    }
    let constants: &[(&str, &str)] = match bits {
        32 => &[
            ("FTY", "float"),
            ("ITY", "i32"),
            ("WIDTH", "32"),
            ("MBITS", "23"),
            ("GAPBITS", "8"),
            ("SIGNMASK", "-2147483648"),
            ("ABSMASK", "2147483647"),
            ("INFBITS", "2139095040"),
            ("MANTMASK", "8388607"),
            ("ONEBIT", "8388608"),
        ],
        64 => &[
            ("FTY", "double"),
            ("ITY", "i64"),
            ("WIDTH", "64"),
            ("MBITS", "52"),
            ("GAPBITS", "11"),
            ("SIGNMASK", "-9223372036854775808"),
            ("ABSMASK", "9223372036854775807"),
            ("INFBITS", "9218868437227405312"),
            ("MANTMASK", "4503599627370495"),
            ("ONEBIT", "4503599627370496"),
        ],
        _ => bug!(Span::dummy(), "no `.lilac.fmod` for {bits}-bit floats"),
    };
    let mut text = FMOD.to_owned();
    for (token, value) in constants {
        text = text.replace(token, value);
    }
    // Linked as an external definition (the linker drops an unreferenced
    // internal one), then made internal.
    append_ir(module, &name, &text);
    match module.get_function(&name) {
        Some(function) => {
            function.set_linkage(Linkage::Internal);
            function
        }
        None => bug!(Span::dummy(), "`{name}` was not defined"),
    }
}

/// C's `fmod` at one width, after musl's `fmod.c` (MIT licence, Copyright
/// © 2005-2020 Rich Felker, et al.): the remainder of `x / y` with the
/// quotient truncated, exact, by long division of the significands. NaN
/// when `y` is zero or NaN or `x` is infinite or NaN; `x` when `|x| < |y|`;
/// otherwise the sign is `x`'s. A subnormal is normalized with `ctlz` where
/// musl loops. The tokens (`FTY`, `ITY`, the masks) are replaced per width.
const FMOD: &str = r"
define FTY @.lilac.fmod.WIDTH(FTY %x, FTY %y) #0 {
entry:
  %ux = bitcast FTY %x to ITY
  %uy = bitcast FTY %y to ITY
  %sign = and ITY %ux, SIGNMASK
  %a = and ITY %ux, ABSMASK
  %b = and ITY %uy, ABSMASK
  %y.zero = icmp eq ITY %b, 0
  %y.nan = icmp ugt ITY %b, INFBITS
  %x.special = icmp uge ITY %a, INFBITS
  %bad.y = or i1 %y.zero, %y.nan
  %bad = or i1 %bad.y, %x.special
  br i1 %bad, label %invalid, label %finite
invalid:
  %product = fmul FTY %x, %y
  %nan = fdiv FTY %product, %product
  ret FTY %nan
finite:
  %below = icmp ult ITY %a, %b
  br i1 %below, label %small, label %compare
small:
  ret FTY %x
compare:
  %equal = icmp eq ITY %a, %b
  br i1 %equal, label %zero, label %divide
zero:
  %signed.zero = bitcast ITY %sign to FTY
  ret FTY %signed.zero
divide:
  %ex.field = lshr ITY %a, MBITS
  %ey.field = lshr ITY %b, MBITS
  %x.subnormal = icmp eq ITY %ex.field, 0
  %y.subnormal = icmp eq ITY %ey.field, 0
  %x.lead = call ITY @llvm.ctlz.ITY(ITY %a, i1 true)
  %y.lead = call ITY @llvm.ctlz.ITY(ITY %b, i1 true)
  %x.shift = sub ITY %x.lead, GAPBITS
  %y.shift = sub ITY %y.lead, GAPBITS
  %x.sub = shl ITY %a, %x.shift
  %y.sub = shl ITY %b, %y.shift
  %ex.sub = sub ITY 1, %x.shift
  %ey.sub = sub ITY 1, %y.shift
  %x.frac = and ITY %a, MANTMASK
  %y.frac = and ITY %b, MANTMASK
  %x.normal = or ITY %x.frac, ONEBIT
  %y.normal = or ITY %y.frac, ONEBIT
  %mx.start = select i1 %x.subnormal, ITY %x.sub, ITY %x.normal
  %my = select i1 %y.subnormal, ITY %y.sub, ITY %y.normal
  %ex.start = select i1 %x.subnormal, ITY %ex.sub, ITY %ex.field
  %ey = select i1 %y.subnormal, ITY %ey.sub, ITY %ey.field
  br label %loop
loop:
  %mx = phi ITY [ %mx.start, %divide ], [ %mx.next, %step ]
  %ex = phi ITY [ %ex.start, %divide ], [ %ex.next, %step ]
  %difference = sub ITY %mx, %my
  %exact = icmp eq ITY %difference, 0
  br i1 %exact, label %zero, label %reduce
reduce:
  %fits = icmp sge ITY %difference, 0
  %mx.reduced = select i1 %fits, ITY %difference, ITY %mx
  %more = icmp sgt ITY %ex, %ey
  br i1 %more, label %step, label %normalize
step:
  %mx.next = shl ITY %mx.reduced, 1
  %ex.next = sub ITY %ex, 1
  br label %loop
normalize:
  %lead = call ITY @llvm.ctlz.ITY(ITY %mx.reduced, i1 true)
  %shift = sub ITY %lead, GAPBITS
  %m = shl ITY %mx.reduced, %shift
  %e = sub ITY %ex, %shift
  %normal = icmp sgt ITY %e, 0
  br i1 %normal, label %pack.normal, label %pack.subnormal
pack.normal:
  %fraction = and ITY %m, MANTMASK
  %exponent = shl ITY %e, MBITS
  %normal.bits = or ITY %fraction, %exponent
  br label %done
pack.subnormal:
  %down = sub ITY 1, %e
  %subnormal.bits = lshr ITY %m, %down
  br label %done
done:
  %bits = phi ITY [ %normal.bits, %pack.normal ], [ %subnormal.bits, %pack.subnormal ]
  %result.bits = or ITY %bits, %sign
  %result = bitcast ITY %result.bits to FTY
  ret FTY %result
}

declare ITY @llvm.ctlz.ITY(ITY, i1)

attributes #0 = { nounwind }
";

/// `__extendhfsf2` and `__truncsfhf2`, the half-precision conversions LLVM
/// calls on x86-64 (which has no conversion instruction without F16C),
/// after compiler-rt's `fp_extend_impl.inc` and `fp_trunc_impl.inc`
/// (Apache-2.0 WITH LLVM-exception): integer code, rounding to nearest
/// with ties to even, NaNs kept quiet with their payload's top bits.
/// `no-builtins` keeps the optimizer from turning either back into a call
/// to itself.
pub(crate) const HALF: &str = r#"
define float @__extendhfsf2(half %value) #0 {
entry:
  %narrow = bitcast half %value to i16
  %h = zext i16 %narrow to i32
  %sign.bit = and i32 %h, 32768
  %sign = shl i32 %sign.bit, 16
  %abs = and i32 %h, 32767
  %exponent = lshr i32 %abs, 10
  %fraction = and i32 %abs, 1023
  %special = icmp eq i32 %exponent, 31
  br i1 %special, label %infinite, label %finite
infinite:
  %payload = shl i32 %fraction, 13
  %special.bits = or i32 %payload, 2139095040
  br label %done
finite:
  %subnormal = icmp eq i32 %exponent, 0
  br i1 %subnormal, label %small, label %normal
normal:
  %rebased = add i32 %abs, 114688
  %normal.bits = shl i32 %rebased, 13
  br label %done
small:
  %is.zero = icmp eq i32 %fraction, 0
  br i1 %is.zero, label %done, label %scale
scale:
  %lead = call i32 @llvm.ctlz.i32(i32 %fraction, i1 true)
  %shift = sub i32 %lead, 21
  %shifted = shl i32 %fraction, %shift
  %mantissa = and i32 %shifted, 1023
  %biased = sub i32 134, %lead
  %exponent.bits = shl i32 %biased, 23
  %mantissa.bits = shl i32 %mantissa, 13
  %subnormal.bits = or i32 %exponent.bits, %mantissa.bits
  br label %done
done:
  %bits = phi i32 [ %special.bits, %infinite ], [ %normal.bits, %normal ], [ 0, %small ], [ %subnormal.bits, %scale ]
  %signed = or i32 %bits, %sign
  %result = bitcast i32 %signed to float
  ret float %result
}

define half @__truncsfhf2(float %value) #0 {
entry:
  %f = bitcast float %value to i32
  %sign.wide = lshr i32 %f, 16
  %sign = and i32 %sign.wide, 32768
  %abs = and i32 %f, 2147483647
  %special = icmp uge i32 %abs, 2139095040
  br i1 %special, label %infinite, label %finite
infinite:
  %is.nan = icmp ugt i32 %abs, 2139095040
  %payload.wide = lshr i32 %abs, 13
  %payload = and i32 %payload.wide, 1023
  %quiet = or i32 %payload, 512
  %nan.bits = or i32 %quiet, 31744
  %special.bits = select i1 %is.nan, i32 %nan.bits, i32 31744
  br label %done
finite:
  %overflow = icmp uge i32 %abs, 1199566848
  br i1 %overflow, label %done, label %in.range
in.range:
  %normal = icmp uge i32 %abs, 947912704
  br i1 %normal, label %round.normal, label %round.subnormal
round.normal:
  %rebased = sub i32 %abs, 939524096
  %kept = lshr i32 %rebased, 13
  %rest = and i32 %rebased, 8191
  %odd = and i32 %kept, 1
  %bias = add i32 %rest, 4095
  %bias.even = add i32 %bias, %odd
  %carry = lshr i32 %bias.even, 13
  %normal.bits = add i32 %kept, %carry
  br label %done
round.subnormal:
  %exponent = lshr i32 %abs, 23
  %tiny = icmp ult i32 %exponent, 102
  br i1 %tiny, label %done, label %shift.subnormal
shift.subnormal:
  %fraction = and i32 %abs, 8388607
  %mantissa = or i32 %fraction, 8388608
  %drop = sub i32 126, %exponent
  %sub.kept = lshr i32 %mantissa, %drop
  %unit = shl i32 1, %drop
  %mask = sub i32 %unit, 1
  %sub.rest = and i32 %mantissa, %mask
  %halfway = lshr i32 %unit, 1
  %sub.odd = trunc i32 %sub.kept to i1
  %above = icmp ugt i32 %sub.rest, %halfway
  %tie = icmp eq i32 %sub.rest, %halfway
  %tie.up = and i1 %tie, %sub.odd
  %up = or i1 %above, %tie.up
  %up.bit = zext i1 %up to i32
  %subnormal.bits = add i32 %sub.kept, %up.bit
  br label %done
done:
  %bits = phi i32 [ %special.bits, %infinite ], [ 31744, %finite ], [ %normal.bits, %round.normal ], [ 0, %round.subnormal ], [ %subnormal.bits, %shift.subnormal ]
  %signed = or i32 %bits, %sign
  %narrow = trunc i32 %signed to i16
  %result = bitcast i16 %narrow to half
  ret half %result
}

declare i32 @llvm.ctlz.i32(i32, i1)

attributes #0 = { nounwind "no-builtins" }
"#;
