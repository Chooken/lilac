"""Prototype of the Payne-Hanek reduction the trig kernels use, with the
same limb arithmetic as the Lilac code, checked against exact rationals."""
import math
import random
from fractions import Fraction

import reference as R

MASK = (1 << 64) - 1
WORDS = 24


def two_over_pi_words():
    prec = 600
    from decimal import localcontext, Decimal
    with localcontext() as c:
        c.prec = prec
        v = 2 / R.pi(prec)
    f = Fraction(v)
    n = int(f * (1 << (64 * WORDS)))
    return [(n >> (64 * (WORDS - 1 - i))) & MASK for i in range(WORDS)]


W = two_over_pi_words()
L = 6


def add_at(D, idx, value):
    """D[idx] += value (u64), carrying towards index 0."""
    s = D[idx] + value
    D[idx] = s & MASK
    carry = s >> 64
    while carry:
        idx -= 1
        s = D[idx] + carry
        D[idx] = s & MASK
        carry = s >> 64


def reduce(x):
    """x >= pi/4 -> (N mod 4, negative, fraction words F[0..3) as 192-bit
    magnitude in [0, 1/2], so that x*2/pi = N' + sign*F)."""
    m, e = math.frexp(x)
    M = int(m * 2 ** 53)
    E = e - 53
    i0 = max(0, (E - 2) // 64)
    b = E - 64 * (i0 + 1)
    D = [0] * (L + 2)
    for j in range(L):
        prod = M * W[i0 + j]
        add_at(D, j + 1, prod >> 64)
        add_at(D, j + 2, prod & MASK)
    # shift left by b0 = b mod 64, b1 = b // 64
    b1, b0 = divmod(b, 64)
    if b0:
        for k in range(len(D)):
            nxt = D[k + 1] if k + 1 < len(D) else 0
            D[k] = ((D[k] << b0) | (nxt >> (64 - b0))) & MASK
    idx = 2 + b1
    n = D[idx] & 3
    F = [D[idx + 1], D[idx + 2], D[idx + 3]]
    neg = False
    if F[0] >> 63:
        neg = True
        n = (n + 1) & 3
        # F = 2^192 - F
        val = (1 << 192) - ((F[0] << 128) | (F[1] << 64) | F[2])
        F = [(val >> 128) & MASK, (val >> 64) & MASK, val & MASK]
    return n, neg, F


def check(x):
    n, neg, F = reduce(x)
    f = Fraction((F[0] << 128) | (F[1] << 64) | F[2], 1 << 192)
    if neg:
        f = -f
    exact = Fraction(x) * 2 / Fraction(R.pi(200))
    exact_f = exact - n
    # n is mod 4: compare the fractional part mod 4
    d = (exact - n - f) % 4
    d = min(d, 4 - d)
    return float(d), f


if __name__ == '__main__':
    rng = random.Random(1)
    worst = 0
    for _ in range(3000):
        e = rng.randint(-1, 1023)
        x = math.ldexp(1 + rng.random(), e) if e > 0 else 0.8 + rng.random()
        if x < 0.79 or not math.isfinite(x):
            continue
        d, f = check(x)
        worst = max(worst, d)
    print('worst absolute error', worst)
    for x in (1e22, 1e300, 5.0e-1 + 0.3, 355.0, 103993.0, 6381956970095103.0 * 2 ** 797):
        d, f = check(x)
        print(x, d, float(f))
