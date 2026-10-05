//! Benchmark gate: the register VM must be at least 3× faster than a
//! straightforward tree-walking baseline on every program in a small suite.
//!
//! This integration test links `lilac-vm` as a library (without `cfg(test)`),
//! so the baseline lives behind the `bench-baseline` feature. Run it with:
//!
//! ```text
//! cargo test -p lilac-vm --test bench --features bench-baseline
//! ```

#![cfg(feature = "bench-baseline")]

use std::hint::black_box;
use std::time::{Duration, Instant};

use lilac_vm::{compile, treewalk_run, Machine, Program};

/// How many timed repetitions each closure gets (the best is reported).
const RUNS: u32 = 10;

/// A small program suite: recursion, a numeric loop, aggregate/runtime work,
/// struct field access, and direct + indirect calls.
// Raw strings are kept uniform (`r#"…"#`) because the `strings` entry contains
// `"`, which forces hashes there.
#[allow(clippy::needless_raw_string_hashes)]
const SUITE: &[(&str, &str)] = &[
    (
        "fib",
        r#"
fib: function (n: @bit_native) r: @bit_native {
    if @i_less_than(n, 0x2) {
        r = n
    } else {
        r = @i_add(fib(@i_sub(n, 0x1)), fib(@i_sub(n, 0x2)))
    }
}

main: function () nothing {
    x: @bit_native = fib(0x12)
}
"#,
    ),
    (
        "mandelbrot",
        r#"
mandel: function (cx: @bit64, cy: @bit64) result: @bit64 {
    zr: @bit64 = 0x0
    zi: @bit64 = 0x0
    i: @bit64 = 0x0
    loop {
        if @i_less_than(i, 0x20) {
            zr2: @bit64 = @i_mul(zr, zr)
            zi2: @bit64 = @i_mul(zi, zi)
            mag: @bit64 = @i_add(zr2, zi2)
            if @i_less_than(mag, 0x400000) {
                zi = @i_add(@i_mul(0x2, @i_mul(zr, zi)), cy)
                zr = @i_add(@i_sub(zr2, zi2), cx)
                i = @i_add(i, 0x1)
            } else {
                break
            }
        } else {
            break
        }
    }
    result = i
}

main: function () nothing {
    a: @bit64 = 0x0
    b: @bit64 = 0x0
    total: @bit64 = 0x0
    loop {
        if @i_less_than(a, 0x8) {
            b = 0x0
            loop {
                if @i_less_than(b, 0x8) {
                    total = @i_add(total, mandel(@i_sub(@i_mul(a, 0x80), 0x400), @i_sub(@i_mul(b, 0x80), 0x400)))
                    b = @i_add(b, 0x1)
                } else {
                    break
                }
            }
            a = @i_add(a, 0x1)
        } else {
            break
        }
    }
}
"#,
    ),
    (
        "strings",
        r#"
main: function () nothing {
    i: @bit_native = 0x0
    acc: @bit_native = 0x0
    loop {
        if @i_less_than(i, 0x200) {
            buf: @byte_buffer = "lilac"
            len: @bit_native = @byte_buffer_len(buf)
            ptr: ref @bit8 = @byte_buffer_ptr(buf)
            idx: @bit_native = @i_mod(i, len)
            c: @bit8 = @load_u8(ptr, idx)
            @store_u8(ptr, idx, c)
            wide: @bit_native = @extend_zero(c)
            acc = @i_add(acc, wide)
            i = @i_add(i, 0x1)
        } else {
            break
        }
    }
}
"#,
    ),
    (
        "structs",
        r#"
Point: object {
    x: @bit64
    y: @bit64

    sum: function (self) result: @bit64 {
        result = @i_add(self.x, self.y)
    }
}

main: function () nothing {
    p: Point = Point {
        x = 0x1
        y = 0x2
    }
    i: @bit64 = 0x0
    acc: @bit64 = 0x0
    loop {
        if @i_less_than(i, 0x40) {
            p.x = @i_add(p.x, 0x1)
            p.y = @i_add(p.y, 0x1)
            acc = @i_add(acc, p.sum())
            i = @i_add(i, 0x1)
        } else {
            break
        }
    }
}
"#,
    ),
    (
        "calls",
        r#"
add: function (a: @bit32, b: @bit32) r: @bit32 {
    r = @i_add(a, b)
}

sub: function (a: @bit32, b: @bit32) r: @bit32 {
    r = @i_sub(a, b)
}

main: function () nothing {
    op: function (@bit32, @bit32) @bit32 = add
    i: @bit32 = 0x0
    acc: @bit32 = 0x0
    loop {
        if @i_less_than(i, 0x40) {
            acc = add(acc, i)
            acc = sub(acc, 0x1)
            acc = op(acc, i)
            i = @i_add(i, 0x1)
        } else {
            break
        }
    }
}
"#,
    ),
];

/// Compiles `.lilac` source to a VM [`Program`] through the real pipeline.
fn compile_src(src: &str) -> Program {
    let mut session = lilac_driver::Session::new();
    session.target = lilac_driver::Target::parse(lilac_vm::VM_TARGET).expect("VM target");
    let module = session
        .compile_lir("bench.lilac", src)
        .unwrap_or_else(|_| panic!("{}", session.render_diagnostics()));
    compile(&module, &session.source_map).expect("compile")
}

/// Runs `f` once to warm up, then `RUNS` times, returning the best wall time.
fn time<R>(mut f: impl FnMut() -> R) -> Duration {
    let _ = black_box(f());
    let mut best = Duration::MAX;
    for _ in 0..RUNS {
        let start = Instant::now();
        let _ = black_box(f());
        best = best.min(start.elapsed());
    }
    best
}

/// The tree-walk baseline computes the same exit code as the VM on every entry.
#[test]
fn treewalk_matches_vm_on_suite() {
    for (name, src) in SUITE {
        let program = compile_src(src);
        let treewalk =
            treewalk_run(&program).unwrap_or_else(|err| panic!("{name}: treewalk error: {err}"));
        let vm = Machine::new(&program).expect("entry").run().expect("run");
        assert_eq!(treewalk, vm, "{name}: engine exit codes differ");
    }
}

/// The VM must be at least 3× faster than the tree-walk baseline per entry.
#[test]
fn vm_is_at_least_3x_treewalk() {
    for (name, src) in SUITE {
        let program = compile_src(src);
        let tw = time(|| treewalk_run(&program));
        let vm = time(|| Machine::new(&program).expect("entry").run());
        eprintln!("{name}: treewalk={tw:?} vm={vm:?}");
        assert!(
            tw.as_nanos() >= vm.as_nanos() * 3,
            "{name}: {tw:?} vs {vm:?}"
        );
    }
}
