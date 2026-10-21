"""Sanity check of reference.py against Python's own float math."""
import math
import struct
from fractions import Fraction as F

import reference as R


def bits(x):
    return struct.unpack('<Q', struct.pack('<d', x))[0]


assert bits(1 / 3) == R.encode(F(1, 3), 64)
assert R.encode(F(1, 2**1074), 64) == 1
assert R.encode(F(1, 2**1075), 64) == 0
assert R.encode(F(3, 2**1075), 64) == 2     # tie to even
worst = 0
cases = [
    ('exp', R.exp_, math.exp, [0.5, 1.5, 10.25, -3.75, 700.5, -745.0]),
    ('exp2', R.exp2_, lambda x: 2.0 ** x, [0.5, 1.5, 10.25, -3.75, 1000.5]),
    ('expm1', R.expm1_, math.expm1, [0.5, 1e-10, -3.75, 20.5]),
    ('ln', R.ln_, math.log, [0.5, 1.5, 1e-300, 1e300, 1.0000001]),
    ('log2', R.log2_, math.log2, [0.5, 3.0, 1e-300, 1e300]),
    ('log10', R.log10_, math.log10, [0.5, 3.0, 1e-300, 1e300]),
    ('ln1p', R.ln1p_, math.log1p, [0.5, 1e-10, -0.75, 1e300]),
    ('sin', R.sin_, math.sin, [0.5, 1.5, 10.25, -3.75, 1e22, 1e300]),
    ('cos', R.cos_, math.cos, [0.5, 1.5, 10.25, -3.75, 1e22, 1e300]),
    ('tan', R.tan_, math.tan, [0.5, 1.5, 10.25, -3.75, 1e22]),
    ('atan', R.atan_, math.atan, [0.5, 1.5, 10.25, -3.75, 1e300, 1e-10]),
    ('asin', R.asin_, math.asin, [0.5, -0.75, 0.999999, 1e-10]),
    ('acos', R.acos_, math.acos, [0.5, -0.75, 0.999999, 1e-10]),
    ('sinh', R.sinh_, math.sinh, [0.5, 1e-10, -3.75, 700.5]),
    ('cosh', R.cosh_, math.cosh, [0.5, 1e-10, -3.75, 700.5]),
    ('tanh', R.tanh_, math.tanh, [0.5, 1e-10, -3.75, 20.5]),
    ('asinh', R.asinh_, math.asinh, [0.5, 1e-10, -3.75, 1e300]),
    ('acosh', R.acosh_, math.acosh, [1.5, 1.0000001, 3.75, 1e300]),
    ('atanh', R.atanh_, math.atanh, [0.5, 1e-10, -0.75, 0.9999999]),
    ('erf', R.erf_, math.erf, [0.5, 1e-10, -1.75, 3.5, 5.5]),
    ('erfc', R.erfc_, math.erfc, [0.5, 1e-10, -1.75, 3.5, 5.5, 20.5]),
    ('gamma', R.gamma_, math.gamma, [0.5, 1.5, 10.25, -3.75, 100.5, 171.5]),
    ('lgamma', R.lgamma_, math.lgamma, [0.5, 1.5, 10.25, -3.75, 100.5, 1e10, 1.0001, 2.0001]),
    ('sinpi', R.sinpi_, lambda x: math.sin(math.pi * x), [0.125, 0.3, 1.75]),
    ('cbrt', R.cbrt_, lambda x: math.copysign(abs(x) ** (1 / 3), x), [8.0, 0.5, -27.0, 1e300]),
]
for name, f, g, xs in cases:
    for x in xs:
        v = f(F(x))
        got = float(F(v)) if not isinstance(v, str) else v
        want = g(x)
        err = abs(got - want) / max(abs(want), 1e-320)
        if err > 1e-10:
            print('MISMATCH', name, x, got, want)
        worst = max(worst, err)
print('worst relative difference', worst)
