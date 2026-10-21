"""Random stress of one function against the reference, outside the tests.

    python tools/math-reference/stress.py <function> <width> <rows> [seed]

Draws `rows` fresh random inputs from the function's test domain, computes
the reference results, builds a release program that checks the Standard
method on each, and prints the mismatches. Needs a built `target/debug/lilac`
and `LILAC_SYSROOT` (the test harness assembles one under `target/tmp`).
"""
import os
import random
import subprocess
import sys
import tempfile

import gen_tests as G

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), '..', '..'))

CALL = {}


def call_for(spec):
    return CALL.get(spec.name) or ('x.' + spec.name + '()' if spec.arity == 1 else 'x.' + spec.name + '(y)')


def main():
    name, width, rows = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    seed = sys.argv[4] if len(sys.argv) > 4 else '1'
    spec = next(s for s in G.all_specs() if s.name == name)
    rng = random.Random(f"stress-{name}-{width}-{seed}")
    digits = width // 4
    lines = []
    for _ in range(rows):
        args = spec.domain(rng, width)
        try:
            res = G.expected_bits(spec, args, width)
        except Exception:
            continue
        lines.append(''.join(f"{a:0{digits}x}" for a in args) + f"{res:0{digits}x}")
    float_t = {16: 'Float16', 32: 'Float32', 64: 'Float64'}[width]
    uint_t = {16: 'Uint16', 32: 'Uint32', 64: 'Uint64'}[width]
    arity = spec.arity
    width_chars = digits * (arity + 1)
    tmp = tempfile.mkdtemp(prefix='stress-')
    inputs = ''
    for i in range(arity):
        var = ['x', 'y'][i]
        inputs += f"        a{i}: @bit64 = hex(data, @i_add_native(at, 0x{i * digits:X}), 0x{digits:X})\n"
        t = f"@truncate(a{i})" if width != 64 else f"a{i}"
        inputs += f"        a{i}t: @bit{width} = {t}\n"
        inputs += f"        {var}: {float_t} = {float_t}.from_bits(a{i}t as {uint_t})\n"
    want_t = "@truncate(wb)" if width != 64 else "wb"
    src = f'''using Standard
using Standard.Io
using Standard.Process

hex: function (data: @byte_buffer, at: @bit_native, count: @bit_native) value: @bit64 {{
    ptr: ref @bit8 = @byte_buffer_ptr(data)
    value = 0x0
    i: @bit_native = 0x0
    loop {{
        if i == count {{ break }}
        c: @bit8 = @load_u8(ptr, @i_add_native(at, i))
        d: @bit8 = if @i_less_than_u_8(c, 0x3A) then @i_sub_8(c, 0x30) else @i_sub_8(c, 0x57)
        value = @bit_or_64(@bit_shift_left_64(value, 0x4), @extend_zero(d))
        i = @i_add_native(i, 0x1)
    }}
}}

main: function () nothing {{
    data: @byte_buffer = "{''.join(lines)}"
    row: @bit_native = 0x0
    bad: @bit_native = 0x0
    loop {{
        if row == 0x{len(lines):X} {{ break }}
        at: @bit_native = @i_mul_native(row, 0x{width_chars:X})
{inputs}        wb: @bit64 = hex(data, @i_add_native(at, 0x{arity * digits:X}), 0x{digits:X})
        wt: @bit{width} = {want_t}
        want: {float_t} = {float_t}.from_bits(wt as {uint_t})
        got: {float_t} = {call_for(spec)}
        gb: {uint_t} = got.to_bits()
        if gb != (wt as {uint_t}) and not (got.is_nan() and want.is_nan()) {{
            bad = @i_add_native(bad, 0x1)
            print_line($"BAD {{row}} {{(a0 as Uint64).hex()}} got {{(gb as Uint64).hex()}} want {{(wb as Uint64).hex()}}")
        }}
        row = @i_add_native(row, 0x1)
    }}
    print_line($"done {{row}} rows, {{bad}} wrong")
}}
'''
    path = os.path.join(tmp, 's.lilac')
    with open(path, 'w') as f:
        f.write(src)
    env = dict(os.environ)
    env.setdefault('LILAC_SYSROOT', os.path.join(ROOT, 'target', 'tmp', 'sysroot'))
    exe = os.path.join(tmp, 's.exe')
    subprocess.run([os.path.join(ROOT, 'target', 'debug', 'lilac.exe'), 'build', '-q',
                    f"--lib=Standard={os.path.join(ROOT, 'lib', 'Standard')}", '--mode=release',
                    '-o', exe, path], check=True, env=env)
    out = subprocess.run([exe], capture_output=True, text=True).stdout
    print(out[-3000:])


if __name__ == '__main__':
    main()
