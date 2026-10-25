"""Picks the hardest-to-round binary64 inputs from CORE-MATH's worst cases.

    python tools/math-reference/pick_worst.py <core-math>/src/binary64 [function...]

CORE-MATH (https://core-math.gitlabpages.inria.fr/, MIT licence) keeps, per
function, a list of inputs whose exact result lies very close to a rounding
boundary (`<f>/<f>.wc`, the results of Lefèvre and Muller's searches and of
CORE-MATH's own). This script reads each list, measures every candidate
(all of them in short lists, an evenly spaced sample of `SAMPLE` in long
ones) with `reference.py`, and writes the `KEEP` hardest, plus the lists'
overflow and underflow edges (their first section), to `worst/<name>.txt`,
which `gen_tests.py` adds to the 64-bit tables. The output is deterministic.
"""
import os
import struct
import sys
from decimal import Decimal, localcontext
from fractions import Fraction

import gen_tests as G
import reference as R

HERE = os.path.dirname(os.path.abspath(__file__))
WORST = os.path.join(HERE, 'worst')

SAMPLE = 60000
KEEP = 40
EDGES = 8

# CORE-MATH's name for each function with a correctly rounded fallback.
NAMES = {
    'exp': 'exp', 'exp2': 'exp2', 'exp10': 'exp10', 'exp_m1': 'expm1',
    'ln': 'log', 'log2': 'log2', 'log10': 'log10', 'ln_1p': 'log1p',
    'sinh': 'sinh', 'cosh': 'cosh', 'tanh': 'tanh', 'asinh': 'asinh',
    'acosh': 'acosh', 'atanh': 'atanh', 'atan': 'atan', 'asin': 'asin',
    'acos': 'acos', 'atan2': 'atan2', 'sin': 'sin', 'cos': 'cos', 'tan': 'tan',
    'sin_pi': 'sinpi', 'cos_pi': 'cospi', 'tan_pi': 'tanpi', 'pow': 'pow',
    'cbrt': 'cbrt', 'hypot': 'hypot',
}

HEADER = """# The hardest-to-round binary64 inputs of `{name}` known, chosen by
# tools/math-reference/pick_worst.py from CORE-MATH's `{core}.wc`
# (https://core-math.gitlabpages.inria.fr/), used under its MIT licence:
#
# Copyright (c) the CORE-MATH authors.
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
# The above copyright notice and this permission notice shall be included in
# all copies or substantial portions of the Software.
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND.
#
# One input per line (`x` or `x,y`, hexadecimal floats), hardest first after
# the overflow and underflow edges; a comment `h` says the exact result is
# 2^-h ulp from the nearest midpoint.
"""


def read_inputs(path):
    """The `.wc` file's inputs as text, and the first section's (the edges)."""
    inputs = []
    edges = []
    with open(path, encoding='utf-8') as f:
        text = f.read()
    for line in text.splitlines():
        line = line.split('#')[0].strip()
        if line:
            inputs.append(line)
    # the edge section, in the files that have one
    marker = 'underflow or overflow'
    if marker in text:
        rest = text.split(marker, 1)[1].split('\n')[1:]
        for line in rest:
            if line.startswith('#'):
                break
            line = line.strip()
            if line:
                edges.append(line)
    return inputs, edges[:EDGES]


def args_of(line):
    """The bit patterns of a line's arguments, or None for NaNs, infinities
    and anything that is not a hexadecimal float."""
    try:
        values = [float.fromhex(p) for p in line.split(',')]
    except ValueError:
        return None
    if any(v != v or v in (float('inf'), float('-inf')) for v in values):
        return None
    return tuple(struct.unpack('<Q', struct.pack('<d', v))[0] for v in values)


def hardness(spec, line):
    """`h` with the exact result 2^-h ulp from the nearest midpoint, or None."""
    args = args_of(line)
    if args is None:
        return None
    dv = G.decode_args(args, 64)
    try:
        exact = spec.expected(dv, 64)
    except Exception:
        return None
    if isinstance(exact, G.Bits) and spec.name == 'hypot':
        # the expectation is the exact square root's bits: measure the root
        with localcontext() as c:
            c.prec = 120
            sq = Fraction(R.hypot_squared(*dv))
            exact = Fraction((Decimal(sq.numerator) / Decimal(sq.denominator)).sqrt())
    if exact is None or isinstance(exact, (str, G.Bits)):
        return None
    try:
        dist = R.distance_to_midpoint(Fraction(exact), 64)
    except Exception:
        return None
    if dist <= 0:
        return 999
    bits = 0
    while dist < 1:
        dist *= 2
        bits += 1
    return bits


def pick(name, core_dir):
    core = NAMES[name]
    spec = next(s for s in G.all_specs() if s.name == name)
    G.spec_kinds_hint = spec.kinds
    path = os.path.join(core_dir, core, core + '.wc')
    if not os.path.exists(path):
        path = os.path.join(core_dir, core + '.wc')
    inputs, edges = read_inputs(path)
    step = max(1, len(inputs) // SAMPLE)
    scored = []
    for line in inputs[::step]:
        h = hardness(spec, line)
        if h is not None:
            scored.append((-h, line))
    scored.sort()
    chosen = []
    seen = set()
    for line in edges:
        if line not in seen and hardness(spec, line) is not None:
            seen.add(line)
            chosen.append((None, line))
    for neg, line in scored:
        if len(chosen) >= KEEP + len(edges):
            break
        if line not in seen:
            seen.add(line)
            chosen.append((-neg, line))
    os.makedirs(WORST, exist_ok=True)
    with open(os.path.join(WORST, name + '.txt'), 'w', newline='\n') as f:
        f.write(HEADER.format(name=name, core=core))
        for h, line in chosen:
            f.write(line + ('' if h is None else f'  # {h}') + '\n')
    return len(chosen), (chosen[len(edges)][0] if len(chosen) > len(edges) else None)


def main(argv):
    core_dir = argv[0]
    names = argv[1:] or list(NAMES)
    for name in names:
        n, top = pick(name, core_dir)
        print(f"{name}: {n} inputs, hardest {top} bits", flush=True)


if __name__ == '__main__':
    main(sys.argv[1:])
