//! Benchmark gate: the register VM must be at least 3× faster than a
//! straightforward tree-walking baseline on every program in a small suite.
//!
//! This integration test links `lilac-vm` as a library (without `cfg(test)`),
//! so the baseline lives behind the `bench-baseline` feature. Run it in release
//! (a debug gate measures the wrong thing) with:
//!
//! ```text
//! cargo test --release -p lilac-vm --test bench --features bench-baseline
//! ```
//!
//! Each entry prints its absolute VM time in nanoseconds, and the `fib` entry
//! is additionally held to [`FIB_BUDGET_NS`].
//!
//! Every suite program computes a non-trivial checksum and then *conditionally
//! traps* (through the runtime `@format_args_piece` abort path) based on that
//! checksum, so a baseline that computes different values exits with a
//! different code than the VM and fails [`treewalk_matches_vm_on_suite`]. The
//! `calls` entry inverts the condition and is expected to trap (code 101), so
//! the fidelity test is not merely "both engines return 0".

#![cfg(feature = "bench-baseline")]

use std::hint::black_box;
use std::time::{Duration, Instant};

use lilac_vm::{compile, treewalk_run, Machine, Program};

/// How many timed repetitions each closure gets (the best is reported).
const RUNS: u32 = 10;

/// The trap exit code (`@lilac_abort` / the runtime abort path).
const TRAP: i32 = 101;

/// Absolute wall-time budget for the `fib` program's VM run, in nanoseconds.
///
/// Initially generous (Task 1 records the baseline); tightened as the M5c.1
/// performance work lands. It is raised here for the Task 3 intermediate: the
/// interpreter now decodes the compact word stream per instruction (M5c matched
/// a typed enum), which is a deliberate transitional regression until Task 7's
/// inner dispatch loop and Tasks 5/6's precomputed metadata restore it. Task 9
/// sets the final `200_000`.
const FIB_BUDGET_NS: u128 = 4_000_000;

/// A small program suite: `(name, expected exit code, source)`.
///
/// Each source computes a checksum and compares it with `check_eq`
/// (trap-unless-equal, expected code 0) or `check_ne` (trap-on-equal, expected
/// code [`TRAP`]). A wrong checksum in either engine flips the exit code, so the
/// fidelity test pins equal computation per entry.
// Raw strings are kept uniform (`r#"…"#`) because the `strings` entry contains
// `"`, which forces hashes there.
#[allow(clippy::needless_raw_string_hashes)]
const SUITE: &[(&str, i32, &str)] = &[
    (
        "fib",
        0,
        r#"
fib: function (n: @bit_native) r: @bit_native {
    if @i_less_than(n, 0x2) {
        r = n
    } else {
        r = @i_add(fib(@i_sub(n, 0x1)), fib(@i_sub(n, 0x2)))
    }
}

check_eq: function (actual: @bit_native, expected: @bit_native) nothing {
    ok: @bit8 = actual == expected
    wide: @bit_native = @extend_zero(ok)
    part: @byte_buffer = "x"
    msg: @format_args = $"a{part}b"
    count: @bit_native = @format_args_count(msg)
    idx: @bit_native = @i_sub(count, wide)
    piece: @byte_buffer = @format_args_piece(msg, idx)
}

main: function () nothing {
    checksum: @bit_native = fib(0x12)
    check_eq(checksum, 0xa18)
}
"#,
    ),
    (
        "mandelbrot",
        0,
        r#"
mandel: function (cx: @bit_native, cy: @bit_native) result: @bit_native {
    zr: @bit_native = 0x0
    zi: @bit_native = 0x0
    i: @bit_native = 0x0
    loop {
        if @i_less_than(i, 0x20) {
            zr2: @bit_native = @i_mul(zr, zr)
            zi2: @bit_native = @i_mul(zi, zi)
            mag: @bit_native = @i_add(zr2, zi2)
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

check_eq: function (actual: @bit_native, expected: @bit_native) nothing {
    ok: @bit8 = actual == expected
    wide: @bit_native = @extend_zero(ok)
    part: @byte_buffer = "x"
    msg: @format_args = $"a{part}b"
    count: @bit_native = @format_args_count(msg)
    idx: @bit_native = @i_sub(count, wide)
    piece: @byte_buffer = @format_args_piece(msg, idx)
}

main: function () nothing {
    a: @bit_native = 0x0
    b: @bit_native = 0x0
    checksum: @bit_native = 0x0
    loop {
        if @i_less_than(a, 0x8) {
            b = 0x0
            loop {
                if @i_less_than(b, 0x8) {
                    checksum = @i_add(checksum, mandel(@i_sub(@i_mul(a, 0x80), 0x400), @i_sub(@i_mul(b, 0x80), 0x400)))
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
    check_eq(checksum, 0x80)
}
"#,
    ),
    (
        "strings",
        0,
        r#"
check_eq: function (actual: @bit_native, expected: @bit_native) nothing {
    ok: @bit8 = actual == expected
    wide: @bit_native = @extend_zero(ok)
    part: @byte_buffer = "x"
    msg: @format_args = $"a{part}b"
    count: @bit_native = @format_args_count(msg)
    idx: @bit_native = @i_sub(count, wide)
    piece: @byte_buffer = @format_args_piece(msg, idx)
}

main: function () nothing {
    i: @bit_native = 0x0
    checksum: @bit_native = 0x0
    loop {
        if @i_less_than(i, 0x200) {
            buf: @byte_buffer = "lilac"
            len: @bit_native = @byte_buffer_len(buf)
            ptr: ref @bit8 = @byte_buffer_ptr(buf)
            idx: @bit_native = @i_mod(i, len)
            c: @bit8 = @load_u8(ptr, idx)
            @store_u8(ptr, idx, c)
            wide: @bit_native = @extend_zero(c)
            checksum = @i_add(checksum, wide)
            i = @i_add(i, 0x1)
        } else {
            break
        }
    }
    check_eq(checksum, 0xced3)
}
"#,
    ),
    (
        "structs",
        0,
        r#"
Point: object {
    x: @bit_native
    y: @bit_native

    sum: function (self) result: @bit_native {
        result = @i_add(self.x, self.y)
    }
}

check_eq: function (actual: @bit_native, expected: @bit_native) nothing {
    ok: @bit8 = actual == expected
    wide: @bit_native = @extend_zero(ok)
    part: @byte_buffer = "x"
    msg: @format_args = $"a{part}b"
    count: @bit_native = @format_args_count(msg)
    idx: @bit_native = @i_sub(count, wide)
    piece: @byte_buffer = @format_args_piece(msg, idx)
}

main: function () nothing {
    p: Point = Point {
        x = 0x1
        y = 0x2
    }
    i: @bit_native = 0x0
    checksum: @bit_native = 0x0
    loop {
        if @i_less_than(i, 0x40) {
            p.x = @i_add(p.x, 0x1)
            p.y = @i_add(p.y, 0x1)
            checksum = @i_add(checksum, p.sum())
            i = @i_add(i, 0x1)
        } else {
            break
        }
    }
    check_eq(checksum, 0x1100)
}
"#,
    ),
    (
        "calls",
        TRAP,
        r#"
add: function (a: @bit_native, b: @bit_native) r: @bit_native {
    r = @i_add(a, b)
}

sub: function (a: @bit_native, b: @bit_native) r: @bit_native {
    r = @i_sub(a, b)
}

check_ne: function (actual: @bit_native, expected: @bit_native) nothing {
    ok: @bit8 = actual == expected
    wide: @bit_native = @extend_zero(ok)
    part: @byte_buffer = "x"
    msg: @format_args = $"a{part}b"
    count: @bit_native = @format_args_count(msg)
    idx: @bit_native = @i_sub(@i_add(count, wide), 0x1)
    piece: @byte_buffer = @format_args_piece(msg, idx)
}

main: function () nothing {
    op: function (@bit_native, @bit_native) @bit_native = add
    i: @bit_native = 0x0
    checksum: @bit_native = 0x0
    loop {
        if @i_less_than(i, 0x40) {
            checksum = add(checksum, i)
            checksum = sub(checksum, 0x1)
            checksum = op(checksum, i)
            i = @i_add(i, 0x1)
        } else {
            break
        }
    }
    check_ne(checksum, 0xf80)
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

/// Each suite entry makes its computed checksum observable as an exit code:
/// the tree-walk baseline and the VM must agree with the expected code, so a
/// baseline that computes different values fails here instead of silently
/// matching a bare `0`.
#[test]
fn treewalk_matches_vm_on_suite() {
    for (name, expected, src) in SUITE {
        let program = compile_src(src);
        let treewalk =
            treewalk_run(&program).unwrap_or_else(|err| panic!("{name}: treewalk error: {err}"));
        let vm = Machine::new(&program).expect("entry").run().expect("run");
        assert_eq!(treewalk, *expected, "{name}: treewalk exit code");
        assert_eq!(vm, *expected, "{name}: vm exit code");
    }
}

/// The VM must be at least 3× faster than the tree-walk baseline per entry.
#[test]
fn vm_is_at_least_3x_treewalk() {
    for (name, _expected, src) in SUITE {
        let program = compile_src(src);
        let tw = time(|| treewalk_run(&program));
        let vm = time(|| Machine::new(&program).expect("entry").run());
        eprintln!(
            "{name}: treewalk={tw:?} ({} ns) vm={vm:?} ({} ns)",
            tw.as_nanos(),
            vm.as_nanos()
        );
        assert!(vm > Duration::ZERO, "{name}: VM time measured as zero");
        assert!(
            tw.as_nanos() >= vm.as_nanos() * 3,
            "{name}: {tw:?} vs {vm:?}"
        );
        if *name == "fib" {
            assert!(
                vm.as_nanos() < FIB_BUDGET_NS,
                "fib: {} ns exceeds budget {} ns",
                vm.as_nanos(),
                FIB_BUDGET_NS
            );
        }
    }
}
