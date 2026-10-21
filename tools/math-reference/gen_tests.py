"""Writes the reference tables the math tests read.

    python tools/math-reference/gen_tests.py [function ...]

For each function and each float width (16, 32, 64) it writes
`data/<function>.<width>.txt`, one row per line: the argument bit
patterns then the correctly rounded result's, in hexadecimal. The rows are

  * the special values (zeros, infinities, NaN, extremes, the points C
    Annex F names), with the expected result stated here by hand;
  * random inputs over the function's domain (fixed seed);
  * the random inputs whose exact result lies closest to a rounding
    boundary among a larger pool (the nearly hard cases).

Results come from reference.py, which uses neither the kernels' algorithms
nor any floating-point hardware. A table is a pure function of this script,
so regenerating changes no byte.
"""
import os
import random
import sys
from fractions import Fraction

import reference as R

HERE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(HERE, 'data')

RANDOM_ROWS = {16: 120, 32: 220, 64: 260}
POOL = {16: 300, 32: 500, 64: 700}
HARD_ROWS = {16: 10, 32: 30, 64: 40}


class Spec:
    def __init__(self, name, arity, domain, expected, specials=None, pool_scale=1.0,
                 kinds=None):
        self.name = name
        self.arity = arity
        self.domain = domain        # (rng, width) -> tuple of args (bit patterns)
        self.expected = expected    # (args as decoded values, width) -> exact/special
        self.specials = specials    # width -> list of argument tuples (arity 2)
        self.kinds = kinds or ('f', 'f')
        self.extra = ()
        self.pool_scale = pool_scale


# ----------------------------------------------------------------- inputs

def fmt(width):
    return R.FORMATS[width]


def val(bits, width):
    return R.decode(bits, width)


def max_finite(width):
    p, emin, emax, ebits = fmt(width)
    return (2 - Fraction(1, 2 ** (p - 1))) * Fraction(2) ** emax


def random_float(rng, width, lo_exp=None, hi_exp=None, sign=True):
    """A random finite float: exponent uniform in [lo_exp, hi_exp]
    (unbiased, subnormals below emin), mantissa uniform."""
    p, emin, emax, ebits = fmt(width)
    lo = emin - (p - 1) if lo_exp is None else lo_exp
    hi = emax if hi_exp is None else hi_exp
    e = rng.randint(lo, hi)
    if e < emin:
        # subnormal: the integer significand has fewer bits
        m = rng.getrandbits(max(1, e - (emin - (p - 1)) + 1)) if e >= emin - (p - 1) else 1
        v = Fraction(max(m, 1)) * Fraction(2) ** (emin - (p - 1))
    else:
        m = (1 << (p - 1)) | rng.getrandbits(p - 1)
        v = Fraction(m, 1 << (p - 1)) * Fraction(2) ** e
    if sign and rng.random() < 0.5:
        v = -v
    return R.encode(v, width)


def uniform_float(rng, width, lo, hi):
    """A random float in [lo, hi] (Fractions), rounded to the format."""
    t = Fraction(rng.getrandbits(80), 1 << 80)
    return R.encode(Fraction(lo) + (Fraction(hi) - Fraction(lo)) * t, width)


def dom_range(lo, hi, tiny=True):
    """Uniform on [lo, hi] mostly, sometimes log-uniform near zero."""
    def gen(rng, width):
        if tiny and rng.random() < 0.25:
            p, emin, emax, ebits = fmt(width)
            return (random_float(rng, width, -(p + 5), -1, True),)
        return (uniform_float(rng, width, lo, hi),)
    return gen


def with_edges(base, *ranges):
    """`base`, but a fifth of the draws come from the given edge ranges
    (lo, hi) (underflow, overflow and other boundaries)."""
    def gen(rng, width):
        if rng.random() < 0.2:
            lo, hi = ranges[rng.randrange(len(ranges))]
            return (uniform_float(rng, width, lo, hi),)
        return base(rng, width)
    return gen


def dom_positive(rng, width):
    r = rng.random()
    if r < 0.3:
        return (random_float(rng, width, None, None, False),)
    if r < 0.6:
        return (random_float(rng, width, -3, 3, False),)
    return (uniform_float(rng, width, Fraction(1, 100), 100),)


def dom_log_all(rng, width):
    return (random_float(rng, width),)


# ------------------------------------------------------------ expected values

def spec_unary(fn, handle):
    """Wraps a numeric reference with a special-value handler: `handle(x)`
    returns an expected special (str / Fraction) or None to compute."""
    def expected(args, width):
        x = args[0]
        s = handle(x)
        if s is not None:
            return s
        return fn(x)
    return expected


def is_nan(x):
    return x == 'nan'


def exp_like(fn, high=800, low=-800, zero_value=Fraction(1)):
    def handle(x):
        if not isinstance(x, str) and x > high:
            return 'inf'
        if not isinstance(x, str) and x < low:
            return '+0'
        if x == 'nan':
            return 'nan'
        if x == 'inf':
            return 'inf'
        if x == '-inf':
            return '+0'
        if x == 0:
            return zero_value
        return None
    return spec_unary(fn, handle)


def log_like(fn):
    def handle(x):
        if x == 'nan':
            return 'nan'
        if x == 'inf':
            return 'inf'
        if x == '-inf':
            return 'nan'
        if x == 0:
            return '-inf'
        if x < 0:
            return 'nan'
        if x == 1:
            return '+0'
        return None
    return spec_unary(fn, handle)


def expm1_expected(args, width):
    x = args[0]
    if x == 'nan':
        return 'nan'
    if x == 'inf':
        return 'inf'
    if x == '-inf' or x < -100:
        return Fraction(-1)
    if x > 800:
        return 'inf'
    if x == 0:
        return zero_like(x)
    return R.expm1_(x)


def ln1p_expected(args, width):
    x = args[0]
    if x == 'nan':
        return 'nan'
    if x == 'inf':
        return 'inf'
    if x == '-inf':
        return 'nan'
    if x == 0:
        return zero_like(x)
    if x < -1:
        return 'nan'
    if x == -1:
        return '-inf'
    return R.ln1p_(x)


class Bits:
    """An expected result already given as bits."""
    def __init__(self, value):
        self.value = value


class SignedZero(Fraction):
    """A zero that remembers its sign."""
    neg = False


def zero_like(x):
    """The zero with the sign of the argument `x` (a decoded zero)."""
    return '-0' if getattr(x, 'neg', False) else '+0'


def special_inputs(width, extra):
    """The shared special argument patterns of a width, plus `extra` floats."""
    p, emin, emax, ebits = fmt(width)
    inf = ((1 << ebits) - 1) << (p - 1)
    sign = 1 << (width - 1)
    out = [0, sign, inf, inf | sign, inf | (1 << (p - 2)), 1, sign | 1,
           R.encode(max_finite(width), width), R.encode(-max_finite(width), width),
           R.encode(Fraction(2) ** emin, width), R.encode(Fraction(1), width),
           R.encode(Fraction(-1), width), R.encode(Fraction(1, 2), width),
           R.encode(Fraction(2), width), R.encode(Fraction(-2), width)]
    for e in extra:
        out.append(R.encode(Fraction(e), width))
    return out


# ------------------------------------------------------------------ function list

def exp_specs():
    return [
        Spec('exp', 1, with_edges(dom_range(-745.5, 710), (-746, -700), (708, 710)), exp_like(R.exp_)),
        Spec('exp2', 1, with_edges(dom_range(-1080, 1030), (-1080, -1020), (1020, 1025)), exp_like(R.exp2_, 1200, -1200)),
        Spec('exp10', 1, with_edges(dom_range(-325, 310), (-325, -300), (307, 309)), exp_like(R.exp10_, 400, -400)),
        Spec('exp_m1', 1, dom_range(-40, 710), expm1_expected),
        Spec('ln', 1, dom_positive, log_like(R.ln_)),
        Spec('log2', 1, dom_positive, log_like(R.log2_)),
        Spec('log10', 1, dom_positive, log_like(R.log10_)),
        Spec('ln_1p', 1, lambda rng, w: (uniform_float(rng, w, -1, 4) if rng.random() < 0.5 else random_float(rng, w, -60, 60),), ln1p_expected),
    ]


# ------------------------------------------------------------------ power

import math as _math


def is_odd_int(y):
    return not isinstance(y, str) and y.denominator == 1 and y.numerator % 2 != 0


def is_int(y):
    return not isinstance(y, str) and y.denominator == 1


def signed(zero_neg, value):
    return value


def pow_expected(args, width):
    x, y = args
    if y == 0 and not isinstance(y, str):
        return Fraction(1)
    if not isinstance(x, str) and x == 1:
        return Fraction(1)
    if x == 'nan' or y == 'nan':
        return 'nan'
    neg_x = x == '-inf' or (not isinstance(x, str) and (x < 0 or getattr(x, 'neg', False)))
    if y in ('inf', '-inf'):
        ax = abs(x) if not isinstance(x, str) else None
        if ax is not None and ax == 1:
            return Fraction(1)
        big = (ax is None or ax > 1)
        if x == 'inf' or x == '-inf':
            big = True
        return 'inf' if (big == (y == 'inf')) else '+0'
    odd = is_odd_int(y)
    if x in ('inf', '-inf'):
        if y > 0:
            return '-inf' if (x == '-inf' and odd) else 'inf'
        return '-0' if (x == '-inf' and odd) else '+0'
    if x == 0:
        if y < 0:
            return '-inf' if (neg_x and odd) else 'inf'
        return '-0' if (neg_x and odd) else '+0'
    if x < 0 and not is_int(y):
        return 'nan'
    ax = abs(x)
    est = float(y) * _math.log(float(ax)) if float(ax) > 0 else 0
    if est > 800:
        return '-inf' if (x < 0 and odd) else 'inf'
    if est < -800:
        return '-0' if (x < 0 and odd) else '+0'
    r = R.pow_(ax, y)
    return -Fraction(r) if (x < 0 and odd) else r


def pow_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    lim = {16: 6, 32: 30, 64: 60}[width]
    if r < 0.5:
        x = random_float(rng, width, -lim, lim, False)
        y = uniform_float(rng, width, -30, 30)
    elif r < 0.7:
        x = random_float(rng, width, -8, 8, True)
        y = R.encode(Fraction(rng.randint(-40, 40)), width)
    elif r < 0.85:
        x = uniform_float(rng, width, Fraction(1, 2), 2)
        y = uniform_float(rng, width, -1100 if width == 64 else -130, 1100 if width == 64 else 130)
    else:
        x = uniform_float(rng, width, 0, 20)
        y = R.encode(Fraction(rng.randint(2, 20)), width)
    return (x, y)


def pow_specials(width):
    vals = special_inputs(width, (3, -3, Fraction(1, 2), Fraction(-1, 2), 10, 11, 7))
    out = []
    for a in vals:
        for b in vals:
            out.append((a, b))
    # exact results and ties
    for base, ex in ((3, 2), (11, 7), (2, 10), (10, 3), (4, Fraction(1, 2)), (-8, 3), (-8, 2), (0, 5), (5, -2)):
        out.append((R.encode(Fraction(base), width), R.encode(Fraction(ex), width)))
    if width == 64:
        out.append((R.encode(Fraction(94906267), 64), R.encode(Fraction(2), 64)))
    return out


def cbrt_expected(args, width):
    x = args[0]
    if x in ('nan', 'inf', '-inf'):
        return x
    if x == 0:
        return zero_like(x)
    return R.cbrt_(x)


def hypot_expected(args, width):
    x, y = args
    if x in ('inf', '-inf') or y in ('inf', '-inf'):
        return 'inf'
    if x == 'nan' or y == 'nan':
        return 'nan'
    p, emin, emax, ebits = fmt(width)
    b = R.sqrt_bits(R.hypot_squared(x, y), width)
    inf = ((1 << ebits) - 1) << (p - 1)
    if b >= inf:
        return 'inf'
    return Bits(b)


def hypot_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.5:
        e = rng.randint(-20, 20)
        return (random_float(rng, width, e - 3, e + 3), random_float(rng, width, e - 3, e + 3))
    if r < 0.8:
        return (random_float(rng, width), random_float(rng, width))
    if r < 0.9:
        a = rng.randint(1, 10 ** 6)
        k = rng.randint(1, 10 ** 3)
        m = R.encode(Fraction(2 * a * k), width)
        n = R.encode(Fraction((a * a - 1) * k), width)
        return (m, n) if rng.random() < 0.5 else (n, m)
    return (random_float(rng, width, emax - 3, emax), random_float(rng, width, emax - 6, emax))


def hypot_specials(width):
    vals = special_inputs(width, (3, 4, 5, 12))
    out = [(a, b) for a in vals for b in vals]
    out.append((R.encode(Fraction(3), width), R.encode(Fraction(4), width)))
    out.append((R.encode(Fraction(5), width), R.encode(Fraction(12), width)))
    return out


def root_expected(args, width):
    x, n = args
    if n == 0 or x == 'nan':
        return 'nan'
    odd = n % 2 != 0
    neg = x == '-inf' or (not isinstance(x, str) and (x < 0 or getattr(x, 'neg', False)))
    if neg and not odd and not (not isinstance(x, str) and x == 0):
        return 'nan'
    if x in ('inf', '-inf') or (not isinstance(x, str) and x == 0):
        big = (x in ('inf', '-inf')) == (n > 0)
        v = 'inf' if big else '+0'
        is_zero = not isinstance(x, str) and x == 0
        if neg and (odd or is_zero):
            v = '-inf' if big else '-0'
        return v
    if neg and not odd:
        return 'nan'
    est = _math.log(abs(float(x))) / n
    if est > 800:
        return '-inf' if neg else 'inf'
    if est < -800:
        return '-0' if neg else '+0'
    return R.root_(x, n)


def root_domain(rng, width):
    n = rng.choice([2, 3, 4, 5, 7, 10, 16, -1, -2, -3, 13, 100, 1000])
    return (random_float(rng, width, -30, 30, True), n & 0xFFFFFFFF)


def root_specials(width):
    out = []
    for a in special_inputs(width, (8, 27, 16, -8)):
        for n in (2, 3, 0, -1, 1):
            out.append((a, n & 0xFFFFFFFF))
    return out


def power_specs():
    return [
        Spec('pow', 2, pow_domain, pow_expected, pow_specials),
        Spec('pow_int', 2, root_domain, lambda a, w: pow_expected((a[0], Fraction(a[1])), w), root_specials, kinds=('f', 'i')),
        Spec('cbrt', 1, dom_log_all, cbrt_expected),
        Spec('hypot', 2, hypot_domain, hypot_expected, hypot_specials),
        Spec('root', 2, root_domain, root_expected, root_specials, kinds=('f', 'i')),
    ]


# ------------------------------------------------------------------ trigonometry

def neg_x(x):
    return x == '-inf' or (not isinstance(x, str) and (x < 0 or getattr(x, 'neg', False)))


def trig_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.3:
        return (uniform_float(rng, width, -12, 12),)
    if r < 0.5:
        return (random_float(rng, width, -(p + 4), 12),)
    if r < 0.65:
        return (random_float(rng, width, min(12, emax - 1), emax),)
    if r < 0.85:
        k = rng.randint(1, 10 ** 6)
        v = Fraction(k) * Fraction(R.pi(60)) / 2
        v = -v if rng.random() < 0.5 else v
        return (R.encode(v, width),)
    # a huge multiple of pi/2's neighbourhood
    return (random_float(rng, width, min(20, emax - 1), min(emax, 120)),)


def trig_specials(extra=()):
    def expected_from(fn, zero_value):
        pass
    return None


def sin_expected(args, width):
    x = args[0]
    if x in ('nan', 'inf', '-inf'):
        return 'nan'
    if x == 0:
        return zero_like(x)
    return R.sin_(x)


def cos_expected(args, width):
    x = args[0]
    if x in ('nan', 'inf', '-inf'):
        return 'nan'
    if x == 0:
        return Fraction(1)
    return R.cos_(x)


def tan_expected(args, width):
    x = args[0]
    if x in ('nan', 'inf', '-inf'):
        return 'nan'
    if x == 0:
        return zero_like(x)
    return R.tan_(x)


def pi_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.35:
        return (uniform_float(rng, width, -5, 5),)
    if r < 0.5:
        return (random_float(rng, width, -(p + 6), 4),)
    if r < 0.65:
        return (random_float(rng, width, 4, min(p + 2, emax)),)
    if r < 0.8:
        k = rng.randint(-40, 40)
        return (R.encode(Fraction(k, 2) + Fraction(rng.choice([0, 1, -1]), 1 << rng.randint(20, 50)), width),)
    return (random_float(rng, width, -(emax - 1), emax),)


def sin_pi_expected(args, width):
    x = args[0]
    if x in ('nan', 'inf', '-inf'):
        return 'nan'
    v = R.sinpi_(x)
    if v == 0:
        return '-0' if neg_x(x) else '+0'
    return v


def cos_pi_expected(args, width):
    x = args[0]
    if x in ('nan', 'inf', '-inf'):
        return 'nan'
    v = R.cospi_(x)
    if v == 0:
        return '+0'
    return v


def tan_pi_expected(args, width):
    x = args[0]
    if x in ('nan', 'inf', '-inf'):
        return 'nan'
    v = R.tanpi_(x)
    if v is None:
        return '-inf' if neg_x(x) else 'inf'
    if v == 0:
        return '-0' if neg_x(x) else '+0'
    return v


HARD_TRIG = (Fraction(1e22), Fraction(3.141592653589793), Fraction(1.5707963267948966),
             Fraction(3.141592653589793) * 2, Fraction(355), Fraction(103993), Fraction(1e300),
             Fraction(6381956970095103) * 2 ** 797, Fraction(2.0 ** 100), Fraction(2.0 ** 1023))


def trig_specs():
    specs = [
        Spec('sin', 1, trig_domain, sin_expected),
        Spec('cos', 1, trig_domain, cos_expected),
        Spec('tan', 1, trig_domain, tan_expected),
        Spec('sin_pi', 1, pi_domain, sin_pi_expected),
        Spec('cos_pi', 1, pi_domain, cos_pi_expected),
        Spec('tan_pi', 1, pi_domain, tan_pi_expected),
    ]
    for spec in specs[:3]:
        spec.extra = HARD_TRIG
    return specs


# ------------------------------------------------------------ inverse trigonometry

def pi_frac():
    return Fraction(R.pi(120))


def atan_expected(args, width):
    x = args[0]
    if x == 'nan':
        return 'nan'
    if x in ('inf', '-inf'):
        v = pi_frac() / 2
        return v if x == 'inf' else -v
    if x == 0:
        return zero_like(x)
    return R.atan_(x)


def asin_expected(args, width):
    x = args[0]
    if x in ('nan', 'inf', '-inf') or abs(x) > 1:
        return 'nan'
    if x == 0:
        return zero_like(x)
    if abs(x) == 1:
        v = pi_frac() / 2
        return v if x > 0 else -v
    return R.asin_(x)


def acos_expected(args, width):
    x = args[0]
    if x in ('nan', 'inf', '-inf') or abs(x) > 1:
        return 'nan'
    if x == 1:
        return '+0'
    if x == -1:
        return pi_frac()
    if x == 0:
        return pi_frac() / 2
    return R.acos_(x)


def atan2_expected(args, width):
    y, x = args
    if y == 'nan' or x == 'nan':
        return 'nan'
    P = pi_frac()
    ny = neg_x(y)
    nx = neg_x(x)
    sgn = -1 if ny else 1
    y_zero = not isinstance(y, str) and y == 0
    x_zero = not isinstance(x, str) and x == 0
    if y_zero:
        if nx:
            return sgn * P
        return '-0' if ny else '+0'
    if x_zero:
        return sgn * P / 2
    if y in ('inf', '-inf'):
        if x == 'inf':
            return sgn * P / 4
        if x == '-inf':
            return sgn * 3 * P / 4
        return sgn * P / 2
    if x == 'inf':
        return '-0' if ny else '+0'
    if x == '-inf':
        return sgn * P
    return R.atan2_(y, x)


def atan_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.4:
        return (uniform_float(rng, width, -3, 3),)
    if r < 0.6:
        return (random_float(rng, width, -(p + 4), 0),)
    return (random_float(rng, width, 0, emax),)


def unit_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.5:
        return (uniform_float(rng, width, -1, 1),)
    if r < 0.7:
        return (random_float(rng, width, -(p + 4), -1),)
    k = rng.randint(1, p - 1)
    v = 1 - Fraction(rng.randint(1, 8), 1 << k)
    return (R.encode(v if rng.random() < 0.5 else -v, width),)


def atan2_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.5:
        return (uniform_float(rng, width, -4, 4), uniform_float(rng, width, -4, 4))
    if r < 0.8:
        e = rng.randint(-20, 20)
        d = rng.randint(-p - 20, p + 20)
        return (random_float(rng, width, e + d - 1, e + d), random_float(rng, width, e - 1, e))
    return (random_float(rng, width), random_float(rng, width))


def atan2_specials(width):
    vals = special_inputs(width, (3, -3, Fraction(1, 2)))
    return [(a, b) for a in vals for b in vals]


def inverse_specs():
    return [
        Spec('atan', 1, atan_domain, atan_expected),
        Spec('asin', 1, unit_domain, asin_expected),
        Spec('acos', 1, unit_domain, acos_expected),
        Spec('atan2', 2, atan2_domain, atan2_expected, atan2_specials),
    ]


# ------------------------------------------------------------------ hyperbolic

def sinh_expected(args, width):
    x = args[0]
    if x == 'nan':
        return 'nan'
    if x in ('inf', '-inf'):
        return x
    if x == 0:
        return zero_like(x)
    if abs(x) > 800:
        return 'inf' if x > 0 else '-inf'
    return R.sinh_(x)


def cosh_expected(args, width):
    x = args[0]
    if x == 'nan':
        return 'nan'
    if x in ('inf', '-inf'):
        return 'inf'
    if x == 0:
        return Fraction(1)
    if abs(x) > 800:
        return 'inf'
    return R.cosh_(x)


def tanh_expected(args, width):
    x = args[0]
    if x == 'nan':
        return 'nan'
    if x in ('inf', '-inf'):
        return Fraction(1 if x == 'inf' else -1)
    if x == 0:
        return zero_like(x)
    return R.tanh_(x)


def asinh_expected(args, width):
    x = args[0]
    if x == 'nan':
        return 'nan'
    if x in ('inf', '-inf'):
        return x
    if x == 0:
        return zero_like(x)
    return R.asinh_(x)


def acosh_expected(args, width):
    x = args[0]
    if x == 'nan' or x == '-inf' or (x != 'inf' and x < 1):
        return 'nan'
    if x == 'inf':
        return 'inf'
    if x == 1:
        return '+0'
    return R.acosh_(x)


def atanh_expected(args, width):
    x = args[0]
    if x in ('nan', 'inf', '-inf') or abs(x) > 1:
        return 'nan'
    if x == 0:
        return zero_like(x)
    if abs(x) == 1:
        return 'inf' if x > 0 else '-inf'
    return R.atanh_(x)


def sinh_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    top = {16: 12, 32: 90, 64: 711}[width]
    if r < 0.4:
        return (uniform_float(rng, width, -30, 30),)
    if r < 0.6:
        return (random_float(rng, width, -(p + 4), 0),)
    return (uniform_float(rng, width, -top - 2, top + 2),)


def acosh_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.4:
        return (uniform_float(rng, width, 1, 10),)
    if r < 0.6:
        return (R.encode(1 + Fraction(rng.randint(1, 1 << 12), 1 << rng.randint(min(14, p - 3), p - 1)), width),)
    return (random_float(rng, width, 0, emax, False),)


def hyperbolic_specs():
    return [
        Spec('sinh', 1, sinh_domain, sinh_expected),
        Spec('cosh', 1, sinh_domain, cosh_expected),
        Spec('tanh', 1, sinh_domain, tanh_expected),
        Spec('asinh', 1, atan_domain, asinh_expected),
        Spec('acosh', 1, acosh_domain, acosh_expected),
        Spec('atanh', 1, unit_domain, atanh_expected),
    ]


# ------------------------------------------------------------------ special functions

def erf_expected(args, width):
    x = args[0]
    if x == 'nan':
        return 'nan'
    if x in ('inf', '-inf'):
        return Fraction(1 if x == 'inf' else -1)
    if x == 0:
        return zero_like(x)
    return R.erf_(x)


def erfc_expected(args, width):
    x = args[0]
    if x == 'nan':
        return 'nan'
    if x == 'inf':
        return '+0'
    if x == '-inf':
        return Fraction(2)
    return R.erfc_(x)


def gamma_expected(args, width):
    x = args[0]
    if x == 'nan' or x == '-inf':
        return 'nan'
    if x == 'inf':
        return 'inf'
    if x == 0:
        return '-inf' if getattr(x, 'neg', False) else 'inf'
    if x > 172:
        return 'inf'
    if x < 0 and x.denominator == 1:
        return 'nan'
    if x < -200:
        # Gamma(x) underflows: the sign is (-1)^ceil(-x)
        return '-0' if int(_math.floor(x)) % 2 != 0 else '+0'
    return R.gamma_(x)


def lgamma_expected(args, width):
    x = args[0]
    if x == 'nan':
        return 'nan'
    if x in ('inf', '-inf'):
        return 'inf'
    if x == 0:
        return 'inf'
    if x < 0 and x.denominator == 1:
        return 'inf'
    if x > Fraction(25563, 10) * 10 ** 304:
        return 'inf'
    return R.lgamma_(x)


def erf_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.5:
        return (uniform_float(rng, width, -3, 3),)
    if r < 0.7:
        return (random_float(rng, width, -(p + 4), 1),)
    return (uniform_float(rng, width, -7, 7),)


def erfc_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.4:
        return (uniform_float(rng, width, -3, 6),)
    if r < 0.55:
        return (random_float(rng, width, -(p + 4), 1),)
    if r < 0.7:
        return (uniform_float(rng, width, -7, 0),)
    return (uniform_float(rng, width, 2, 28),)


def gamma_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.35:
        return (uniform_float(rng, width, -6, 8),)
    if r < 0.5:
        return (random_float(rng, width, -(p + 4), 1),)
    if r < 0.7:
        return (uniform_float(rng, width, 0, 171.7 if width > 16 else 12),)
    if r < 0.8:
        k = rng.randint(-170, 0)
        return (R.encode(Fraction(k) + Fraction(rng.choice([1, -1]), 1 << rng.randint(5, p - 2)), width),)
    return (uniform_float(rng, width, -185, 0),)


def lgamma_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.3:
        return (uniform_float(rng, width, -8, 8),)
    if r < 0.45:
        return (R.encode(1 + Fraction(rng.randint(-1000, 1000), 1 << rng.randint(3, p - 1)), width),)
    if r < 0.55:
        return (R.encode(2 + Fraction(rng.randint(-1000, 1000), 1 << rng.randint(3, p - 1)), width),)
    if r < 0.7:
        return (uniform_float(rng, width, -100, 100),)
    if r < 0.85:
        return (random_float(rng, width, 0, emax, False),)
    return (random_float(rng, width, -(p + 4), 1),)


def special_specs():
    return [
        Spec('erf', 1, erf_domain, erf_expected),
        Spec('erfc', 1, erfc_domain, erfc_expected),
        Spec('gamma', 1, gamma_domain, gamma_expected, pool_scale=0.4),
        Spec('ln_gamma', 1, lgamma_domain, lgamma_expected, pool_scale=0.4),
    ]


# ------------------------------------------------------------------ log with a base

def ln_value(v):
    """ln as an exact value or special: ('nan'|'inf'|'-inf'|Fraction)."""
    if v == 'nan' or v == '-inf':
        return 'nan'
    if v == 'inf':
        return 'inf'
    if v == 0:
        return '-inf'
    if v < 0:
        return 'nan'
    if v == 1:
        return Fraction(0)
    return R.ln_(v)


def log_base_expected(args, width):
    x, b = args
    if b == '-inf' or (not isinstance(b, str) and b <= 1 and b in (0, 1)):
        return 'nan'
    if not isinstance(b, str) and b < 0:
        return 'nan'
    lx = ln_value(x)
    lb = ln_value(b)
    if lx == 'nan' or lb == 'nan':
        return 'nan'
    if lx in ('inf', '-inf') and lb in ('inf', '-inf'):
        return 'nan'
    if lb in ('inf', '-inf'):
        neg = (lx < 0) != (lb == '-inf')
        return '-0' if neg else '+0'
    if lx in ('inf', '-inf'):
        neg = (lx == '-inf') != (lb < 0)
        return '-inf' if neg else 'inf'
    if lb == 0:
        return 'nan'
    if lx == 0:
        return '-0' if lb < 0 else '+0'
    from decimal import localcontext
    with localcontext() as c:
        c.prec = 150
        return Fraction(R.dec(lx) / R.dec(lb))


def log_base_domain(rng, width):
    r = rng.random()
    p, emin, emax, ebits = fmt(width)
    if r < 0.6:
        return (dom_positive(rng, width)[0], uniform_float(rng, width, Fraction(1, 10), 100))
    if r < 0.8:
        b = rng.choice([2, 10, 3, 4, 8])
        k = rng.randint(-6, 10)
        return (R.encode(Fraction(b) ** k, width), R.encode(Fraction(b), width))
    return (random_float(rng, width, -20, 20, False), random_float(rng, width, -8, 8, False))


def log_base_specials(width):
    vals = special_inputs(width, (3, 10))
    return [(a, b) for a in vals for b in vals]


def log_specs():
    return [Spec('log', 2, log_base_domain, log_base_expected, log_base_specials)]


FAMILIES = {
    'log': log_specs,
    'special': special_specs,
    'hyperbolic': hyperbolic_specs,
    'inverse': inverse_specs,
    'trigonometry': trig_specs,
    'power': power_specs,
    'exponential': exp_specs,
}


def all_specs():
    out = []
    for fam in FAMILIES.values():
        out.extend(fam())
    return out


# ------------------------------------------------------------------ generation

spec_kinds_hint = None


def kinds_of(kinds, i):
    return kinds[i] if kinds else 'f'


def decode_args(args, width):
    out = []
    for i, a in enumerate(args):
        if kinds_of(spec_kinds_hint, i) == 'i':
            out.append(a - (1 << 32) if a >= (1 << 31) else a)
            continue
        v = val(a, width)
        if not isinstance(v, str) and v == 0:
            z = SignedZero(0)
            z.neg = bool(a >> (width - 1))
            v = z
        out.append(v)
    return tuple(out)


def make_rows(spec, width):
    global spec_kinds_hint
    spec_kinds_hint = spec.kinds
    rng = random.Random(f"{spec.name}-{width}")
    rows = []
    seen = set()

    def add(args):
        if args in seen:
            return
        seen.add(args)
        rows.append(args)

    if spec.arity == 1:
        for s in special_inputs(width, spec.extra if width == 64 else spec.extra[:6]):
            add((s,))
    elif spec.specials:
        for args in spec.specials(width):
            add(args)
    # random inputs
    for _ in range(RANDOM_ROWS[width]):
        add(spec.domain(rng, width))
    # the nearly hard cases from a pool
    pool = []
    for _ in range(int(POOL[width] * spec.pool_scale)):
        args = spec.domain(rng, width)
        dv = decode_args(args, width)
        if any(isinstance(d, str) for d in dv):
            continue
        try:
            exact = spec.expected(dv, width)
        except Exception:
            continue
        if isinstance(exact, str) or exact is None:
            continue
        try:
            dist = R.distance_to_midpoint(Fraction(exact), width)
        except Exception:
            continue
        pool.append((dist, args))
    pool.sort(key=lambda t: t[0])
    for dist, args in pool[:HARD_ROWS[width]]:
        add(args)
    return rows


def expected_bits(spec, args, width):
    global spec_kinds_hint
    spec_kinds_hint = spec.kinds
    dv = decode_args(args, width)
    # a zero argument keeps its sign for the odd functions: handled by the
    # expectation functions through the raw sign
    e = spec.expected(dv, width)
    if e is None:
        raise ValueError(f"{spec.name}{args}: no expected value")
    if isinstance(e, Bits):
        return e.value
    if isinstance(e, str):
        return R.encode(e, width)
    e = Fraction(e) if not isinstance(e, Fraction) else e
    if e == 0:
        return 0
    return R.encode(e, width)


def write(spec, width):
    rows = make_rows(spec, width)
    digits = width // 4
    lines = []
    for args in rows:
        res = expected_bits(spec, args, width)
        fields = []
        for i, a in enumerate(args):
            fields.append(f"{a:08x}" if kinds_of(spec.kinds, i) == 'i' else f"{a:0{digits}x}")
        lines.append(' '.join(fields) + ' ' + f"{res:0{digits}x}")
    os.makedirs(DATA, exist_ok=True)
    path = os.path.join(DATA, f"{spec.name}.{width}.txt")
    with open(path, 'w', newline='\n') as f:
        f.write('\n'.join(lines) + '\n')
    return len(lines)


def main(argv):
    names = set(argv)
    for spec in all_specs():
        if names and spec.name not in names:
            continue
        for width in (16, 32, 64):
            n = write(spec, width)
            print(f"{spec.name}.{width}: {n} rows", flush=True)


if __name__ == '__main__':
    main(sys.argv[1:])
