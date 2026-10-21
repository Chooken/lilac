"""High-precision reference values for Standard's elementary functions.

Pure standard library (decimal, fractions): no mpmath. Every function takes
exact rational arguments (`Fraction`) and returns the result as a `Decimal`
carrying far more digits than any binary format needs (or an exact
`Fraction` when the result is rational); `encode` then rounds once to the
target format (ties to even), so the tables hold correctly rounded values.

Independent of the Lilac kernels on purpose: Taylor series and the
`decimal` module's own correctly rounded exp/ln/sqrt, never the
double-double algorithms the kernels use.
"""
from decimal import Decimal, getcontext, localcontext, MAX_EMAX, MIN_EMIN
from fractions import Fraction
import math

getcontext().prec = 160
getcontext().Emax = MAX_EMAX
getcontext().Emin = MIN_EMIN

DIGITS = 90

F16 = (11, -14, 15, 5)       # precision, emin, emax, exponent bits
F32 = (24, -126, 127, 8)
F64 = (53, -1022, 1023, 11)
FORMATS = {16: F16, 32: F32, 64: F64}


def dec(x):
    """A Fraction or int as a Decimal at the current context precision."""
    if isinstance(x, Decimal):
        return x
    x = Fraction(x)
    return Decimal(x.numerator) / Decimal(x.denominator)


# ---------------------------------------------------------------- formats

def decode(bits, width):
    """Bits -> 'nan' | 'inf' | '-inf' | Fraction (zero has no sign here)."""
    p, emin, emax, ebits = FORMATS[width]
    sign = bits >> (width - 1)
    e = (bits >> (p - 1)) & ((1 << ebits) - 1)
    m = bits & ((1 << (p - 1)) - 1)
    if e == (1 << ebits) - 1:
        if m:
            return 'nan'
        return '-inf' if sign else 'inf'
    if e == 0:
        v = Fraction(m, 1 << (p - 1)) * Fraction(2) ** emin
    else:
        v = Fraction((1 << (p - 1)) + m, 1 << (p - 1)) * Fraction(2) ** (e - emax)
    return -v if sign else v


def encode(v, width):
    """Round the exact value `v` (Fraction/Decimal, or 'inf', '-inf',
    'nan', '+0', '-0') to the format, ties to even; returns the bits."""
    p, emin, emax, ebits = FORMATS[width]
    inf = ((1 << ebits) - 1) << (p - 1)
    sign_bit = 1 << (width - 1)
    if v == 'nan':
        return inf | (1 << (p - 2))
    if v == 'inf':
        return inf
    if v == '-inf':
        return inf | sign_bit
    if v == '+0':
        return 0
    if v == '-0':
        return sign_bit
    v = Fraction(v)
    if v == 0:
        return 0
    sign = sign_bit if v < 0 else 0
    a = abs(v)
    e = a.numerator.bit_length() - a.denominator.bit_length()
    if Fraction(2) ** e > a:
        e -= 1
    e_eff = max(e, emin)
    q = Fraction(2) ** (e_eff - (p - 1))
    n_exact = a / q
    n = n_exact.numerator // n_exact.denominator
    r = n_exact - n
    if r > Fraction(1, 2) or (r == Fraction(1, 2) and n % 2 == 1):
        n += 1
    if n >= (1 << p):          # carried into the next binade
        n >>= 1
        e_eff += 1
    if e_eff > emax:
        return inf | sign
    if n < (1 << (p - 1)):     # subnormal (e_eff == emin)
        return sign | n
    field = e_eff + emax
    return sign | (field << (p - 1)) | (n - (1 << (p - 1)))


def distance_to_midpoint(v, width):
    """How close v is to a rounding boundary, in units of the result's
    spacing (0 = exactly a midpoint, 0.5 = exactly representable)."""
    p, emin, emax, ebits = FORMATS[width]
    v = Fraction(v)
    a = abs(v)
    if a == 0:
        return Fraction(1, 2)
    e = a.numerator.bit_length() - a.denominator.bit_length()
    if Fraction(2) ** e > a:
        e -= 1
    e_eff = max(e, emin)
    q = Fraction(2) ** (e_eff - (p - 1))
    n_exact = a / q
    frac = n_exact - (n_exact.numerator // n_exact.denominator)
    return abs(frac - Fraction(1, 2))


# ------------------------------------------------------------------ constants

_pi_cache = {}


def pi(prec):
    """pi to at least `prec` decimal digits (Machin, integer arithmetic)."""
    for k in sorted(_pi_cache):
        if k >= prec:
            return _pi_cache[k]
    guard = prec + 10
    one = 10 ** guard

    def arctan_inv(n):
        x = one // n
        total = x
        n2 = n * n
        k = 3
        sign = -1
        term = x
        while term:
            term //= n2
            total += sign * (term // k)
            sign = -sign
            k += 2
        return total

    p = 4 * (4 * arctan_inv(5) - arctan_inv(239))
    val = Decimal(p) / Decimal(one)
    _pi_cache[prec] = val
    return val


def ln2(prec):
    with localcontext() as c:
        c.prec = prec + 5
        return Decimal(2).ln()


def magnitude(x):
    """Rough base-10 exponent of |x| (0 for zero)."""
    x = abs(Fraction(x))
    if x == 0:
        return 0
    return int((x.numerator.bit_length() - x.denominator.bit_length()) * 0.30103)


def with_prec(extra, fn):
    with localcontext() as c:
        c.prec = DIGITS + int(max(0, extra))
        c.Emax = MAX_EMAX
        c.Emin = MIN_EMIN
        return +fn()


def tiny_extra(x):
    """Extra digits to survive a cancellation near x = 0."""
    return 2 * max(0, -magnitude(x)) + 10


# --------------------------------------------------------------- exp / log

def exp_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(1)
    return with_prec(max(0, magnitude(x)) + 10, lambda: dec(x).exp())


def exp2_(x):
    x = Fraction(x)
    if x.denominator == 1 and abs(x) < 5000:
        return Fraction(2) ** int(x)
    return with_prec(max(0, magnitude(x)) + 10, lambda: (dec(x) * ln2(DIGITS + 40)).exp())


def exp10_(x):
    x = Fraction(x)
    if x.denominator == 1 and abs(x) < 400:
        return Fraction(10) ** int(x)
    return with_prec(max(0, magnitude(x)) + 10, lambda: (dec(x) * Decimal(10).ln()).exp())


def expm1_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(0)
    return with_prec(tiny_extra(x) + max(0, magnitude(x)), lambda: dec(x).exp() - 1)


def ln_(x):
    x = Fraction(x)
    if x == 1:
        return Fraction(0)
    return with_prec(tiny_extra(x - 1), lambda: dec(x).ln())


def log2_(x):
    x = Fraction(x)
    if x > 0 and (x.numerator & (x.numerator - 1)) == 0 and (x.denominator & (x.denominator - 1)) == 0:
        return Fraction(x.numerator.bit_length() - x.denominator.bit_length())
    if x == 1:
        return Fraction(0)
    return with_prec(tiny_extra(x - 1), lambda: dec(x).ln() / ln2(DIGITS + tiny_extra(x - 1) + 10))


def log10_(x):
    x = Fraction(x)
    if x == 1:
        return Fraction(0)
    if x > 0:
        # exact powers of ten
        for k in range(-330, 310):
            if Fraction(10) ** k == x:
                return Fraction(k)
    return with_prec(tiny_extra(x - 1), lambda: dec(x).log10())


def ln1p_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(0)
    return with_prec(tiny_extra(x), lambda: dec(1 + x).ln())


def pow_(x, y):
    """x^y for x > 0, or x < 0 with y an integer. Exact when y is a small
    integer (so exact ties are decided exactly)."""
    x = Fraction(x)
    y = Fraction(y)
    if y.denominator == 1 and abs(y) <= 64 and x != 0:
        return x ** int(y)
    if x < 0:
        r = pow_(-x, y)
        return -r if y.numerator % 2 else r
    return with_prec(30 + max(0, magnitude(y)), lambda: (dec(y) * dec(x).ln()).exp())


def exact_root(a, n, r):
    """If the n-th root of a is rational with a small denominator, it."""
    cand = Fraction(r).limit_denominator(1 << 70)
    if cand ** n == a:
        return cand
    return None


def cbrt_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(0)
    a = abs(x)
    r = with_prec(30, lambda: (dec(a).ln() / 3).exp())
    e = exact_root(a, 3, r)
    if e is not None:
        r = e
    return -r if x < 0 else r


def root_(x, n):
    x = Fraction(x)
    if n < 0:
        return 1 / root_(x, -n)
    a = abs(x)
    r = with_prec(30, lambda: (dec(a).ln() / n).exp())
    e = exact_root(a, n, r)
    if e is not None:
        r = e
    return -r if x < 0 else r


def hypot_squared(x, y):
    """The exact radicand of hypot; round it with `sqrt_bits`."""
    return Fraction(x) ** 2 + Fraction(y) ** 2


def sqrt_bits(s, width):
    """Bits of the correctly rounded sqrt of the exact rational s >= 0."""
    if s == 0:
        return 0
    r = with_prec(30, lambda: dec(s).sqrt())
    bits = encode(Fraction(r), width)

    def val(b):
        v = decode(b, width)
        return Fraction(2) ** (FORMATS[width][2] + 1) if isinstance(v, str) else v

    for c in (bits, bits - 1, bits + 1):
        lo = (val(c) + val(c - 1)) / 2 if c > 0 else Fraction(0)
        hi = (val(c) + val(c + 1)) / 2
        ok_lo = s > lo * lo or (s == lo * lo and c % 2 == 0)
        ok_hi = s < hi * hi or (s == hi * hi and c % 2 == 0)
        if ok_lo and ok_hi:
            return c
    return bits


# ---------------------------------------------------------------- trig

def _sincos_series(r, prec):
    """sin and cos of the Decimal r (|r| <= ~0.8) by Taylor series."""
    with localcontext() as c:
        c.prec = prec
        r2 = r * r
        tol = Decimal(10) ** (-(prec + 5))
        term = r
        s = r
        n = 1
        while r != 0 and abs(term) > tol * abs(r):
            n += 2
            term = -term * r2 / (n * (n - 1))
            s += term
        term = Decimal(1)
        co = Decimal(1)
        n = 0
        while abs(term) > tol:
            n += 2
            term = -term * r2 / (n * (n - 1))
            co += term
        return s, co


def sincos_(x):
    """(sin x, cos x) for the exact rational x, as Decimals."""
    x = Fraction(x)
    if x == 0:
        return Decimal(0), Decimal(1)
    prec = DIGITS + 40 + max(0, magnitude(x)) + max(0, -magnitude(x))
    with localcontext() as c:
        c.prec = prec
        c.Emax = MAX_EMAX
        c.Emin = MIN_EMIN
        half = pi(prec + 10) / 2
        xd = dec(x)
        k = int((xd / half).to_integral_value())
        r = xd - k * half
        s, co = _sincos_series(r, prec)
        k %= 4
        if k == 0:
            return +s, +co
        if k == 1:
            return +co, -s
        if k == 2:
            return -s, -co
        return -co, +s


def sin_(x):
    return sincos_(x)[0]


def cos_(x):
    return sincos_(x)[1]


def tan_(x):
    s, c = sincos_(x)
    with localcontext() as ctx:
        ctx.prec = DIGITS
        return s / c


def _pi_reduce(x):
    """x -> (n mod 4, r) with x = n/2 + r exactly, |r| <= 1/4."""
    n = round(x * 2)
    r = x - Fraction(n, 2)
    return n % 4, r


def _sincos_pi(r):
    prec = DIGITS + 40 + tiny_extra(r)
    with localcontext() as c:
        c.prec = prec
        c.Emax = MAX_EMAX
        c.Emin = MIN_EMIN
        arg = dec(r) * pi(prec + 10)
        return _sincos_series(arg, prec)


def sinpi_(x):
    k, r = _pi_reduce(Fraction(x))
    if r == 0:
        return Fraction(1 if k == 1 else (-1 if k == 3 else 0))
    s, c = _sincos_pi(r)
    return [s, c, -s, -c][k]


def cospi_(x):
    k, r = _pi_reduce(Fraction(x))
    if r == 0:
        return Fraction({0: 1, 2: -1}.get(k, 0))
    s, c = _sincos_pi(r)
    return [c, -s, -c, s][k]


def tanpi_(x):
    k, r = _pi_reduce(Fraction(x))
    if r == 0:
        return Fraction(0) if k % 2 == 0 else None   # pole
    s, c = _sincos_pi(r)
    with localcontext() as ctx:
        ctx.prec = DIGITS
        t = s / c
        return t if k % 2 == 0 else -1 / t


def atan_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(0)
    with localcontext() as c:
        c.prec = DIGITS + 20 + tiny_extra(x)
        c.Emax = MAX_EMAX
        c.Emin = MIN_EMIN
        v = dec(x)
        neg = v < 0
        v = abs(v)
        inv = False
        if v > 1:
            v = 1 / v
            inv = True
        halvings = 0
        while v > Decimal('0.05'):
            v = v / (1 + (1 + v * v).sqrt())
            halvings += 1
        v2 = v * v
        term = v
        total = v
        n = 1
        tol = Decimal(10) ** (-(c.prec + 3)) * abs(v)
        while abs(term) > tol:
            n += 2
            term = -term * v2
            total += term / n
        total *= 2 ** halvings
        if inv:
            total = pi(c.prec + 5) / 2 - total
        return +(-total if neg else total)


def asin_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(0)
    with localcontext() as c:
        c.prec = DIGITS + 40 + tiny_extra(1 - abs(x))
        if abs(x) == 1:
            return pi(DIGITS + 20) / 2 * (1 if x > 0 else -1)
        q = dec(x) / (1 - dec(x) * dec(x)).sqrt()
        qf = Fraction(q)
    return atan_(qf)


def acos_(x):
    x = Fraction(x)
    if x == 1:
        return Fraction(0)
    with localcontext() as c:
        c.prec = DIGITS + 40 + tiny_extra(1 - abs(x))
        P = pi(c.prec + 10)
        if x == -1:
            return +P
        if x == 0:
            return +(P / 2)
        s = ((1 - dec(x)) * (1 + dec(x))).sqrt()
        if x > 0:
            return atan_(Fraction(s / dec(x)))
        return +(P - atan_(Fraction(s / dec(-x))))


def atan2_(y, x):
    """atan2 for y != 0 and x != 0 (the callers handle the axes)."""
    y = Fraction(y)
    x = Fraction(x)
    with localcontext() as c:
        c.prec = DIGITS + 30
        P = pi(DIGITS + 40)
        a = atan_(y / x)
        if x > 0:
            return a
        return +(a + P) if y > 0 else +(a - P)


# ---------------------------------------------------------------- hyperbolic

def sinh_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(0)
    extra = 20 + tiny_extra(x) + abs(magnitude(x)) + int(abs(float(x)) / 2.3)
    return with_prec(extra, lambda: (dec(x).exp() - (-dec(x)).exp()) / 2)


def cosh_(x):
    x = Fraction(x)
    extra = 20 + int(abs(float(x)) / 2.3)
    return with_prec(extra, lambda: (dec(x).exp() + (-dec(x)).exp()) / 2)


def tanh_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(0)
    if abs(x) > 400:
        return Fraction(1 if x > 0 else -1)

    def f():
        e = (2 * dec(x)).exp()
        return (e - 1) / (e + 1)
    return with_prec(30 + tiny_extra(x), f)


def asinh_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(0)

    def f():
        a = abs(dec(x))
        t = (a * a + 1).sqrt()
        if a > 1:
            return (a + t).ln()
        return (1 + a + a * a / (1 + t)).ln()
    r = with_prec(30 + 2 * tiny_extra(x), f)
    return -r if x < 0 else r


def acosh_(x):
    x = Fraction(x)
    if x == 1:
        return Fraction(0)
    return with_prec(30 + 2 * tiny_extra(x - 1),
                     lambda: (dec(x) + ((dec(x) - 1) * (dec(x) + 1)).sqrt()).ln())


def atanh_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(0)
    return with_prec(30 + 2 * tiny_extra(x),
                     lambda: ((1 + dec(x)) / (1 - dec(x))).ln() / 2)


# ---------------------------------------------------------------- special

def _erf_series(a):
    """erf of the Decimal a > 0 by the positive-term series."""
    term = a
    total = a
    n = 0
    a2 = 2 * a * a
    tol = Decimal(10) ** (-(getcontext().prec + 3))
    while abs(term) > tol * total:
        n += 1
        term = term * a2 / (2 * n + 1)
        total += term
    return 2 / pi(getcontext().prec + 10).sqrt() * (-(a * a)).exp() * total


def erf_(x):
    x = Fraction(x)
    if x == 0:
        return Fraction(0)
    ax = abs(x)
    if ax > 9:
        return Fraction(1) if x > 0 else Fraction(-1)
    r = with_prec(30 + tiny_extra(x) + int(float(ax) ** 2 / 2.3), lambda: _erf_series(dec(ax)))
    return -r if x < 0 else r


def erfc_(x):
    x = Fraction(x)
    fx = float(x)
    if fx > 27.3:
        return Fraction(0)
    if fx < -9:
        return Fraction(2)

    def f():
        e = _erf_series(dec(abs(x)))
        return (1 - e) if x > 0 else (1 + e)
    return with_prec(40 + int(fx * fx / 2.3 * 1.0) + 20, f)


_bern_cache = {}


def bernoulli(n):
    """B_n as a Fraction (B_1 = -1/2)."""
    if n in _bern_cache:
        return _bern_cache[n]
    A = [Fraction(0)] * (n + 1)
    for m in range(n + 1):
        A[m] = Fraction(1, m + 1)
        for j in range(m, 0, -1):
            A[j - 1] = j * (A[j - 1] - A[j])
        _bern_cache[m] = A[0]
    return _bern_cache[n]


def _lgamma_pos(z, prec):
    """ln Gamma(z) for the Decimal z > 0 at the context precision: shift z
    above a bound, then the Stirling series."""
    zz = z
    prod = Decimal(1)
    target = 40 + prec // 2
    while zz < target:
        prod *= zz
        zz += 1
    lg = (zz - Decimal('0.5')) * zz.ln() - zz + (2 * pi(prec + 10)).ln() / 2
    z2 = zz * zz
    zp = zz
    k = 1
    tol = Decimal(10) ** (-(prec + 3))
    while True:
        term = dec(bernoulli(2 * k)) / (2 * k * (2 * k - 1)) / zp
        lg += term
        if abs(term) < tol or k > 600:
            break
        zp *= z2
        k += 1
    return lg - prod.ln()


def gamma_(x):
    x = Fraction(x)
    if x.denominator == 1 and 0 < x < 40:
        return Fraction(math.factorial(int(x) - 1))
    extra = 40 + abs(magnitude(x)) * 2

    def f():
        prec = getcontext().prec
        if x > 0:
            return _lgamma_pos(dec(x), prec).exp()
        s = dec(sinpi_(x))
        return pi(prec + 10) / (s * _lgamma_pos(dec(1 - x), prec).exp())
    return with_prec(extra + 20, f)


def lgamma_(x):
    x = Fraction(x)
    if x in (1, 2):
        return Fraction(0)
    extra = 80 + abs(magnitude(x)) * 2

    def f():
        prec = getcontext().prec
        if x > 0:
            return _lgamma_pos(dec(x), prec)
        s = abs(dec(sinpi_(x)))
        return (pi(prec + 10) / s).ln() - _lgamma_pos(dec(1 - x), prec)
    return with_prec(extra + 40, f)
