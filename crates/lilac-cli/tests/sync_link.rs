//! `Standard.Sync` links on every native target: its locks wait in the
//! kernel through `Standard.Os`'s wait layer (`futex` on Linux,
//! `WaitOnAddress` from `synchronization.lib` on Windows, `__ulock_wait2`
//! from `libSystem` on macOS) and spin with `@spin_hint` (a `pause` or
//! `yield` instruction, or nothing), so a missing stub or an unlowered
//! hint shows up as a failed link. Threads link the same way (`clone` and
//! `mprotect`, `CreateThread`, the `pthread_attr_*` stack size), and so
//! do the terminal checks (`GetConsoleMode`, the `TCGETS` `ioctl`,
//! `isatty`) and Windows' `WriteConsoleW`.
//!
//! The program uses every lock, wait and wake once, starts and joins a
//! thread, and is built for each
//! native target at `debug` and `release`, cross-linked with the
//! development sysroot's `lld`. The run tests (`tests/run/mutex_*`,
//! `condvar_queue`) run it on the host.

mod support;

use std::path::Path;
use std::process::Command;

/// The native targets: every `Target::link_info` pair.
const TARGETS: [&str; 6] = [
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

const PROGRAM: &str = "\
using Standard
using Standard.Io
using Standard.Os as Os
using Standard.Sync as Sync

counter: Sync.Mutex[Int] = Sync.Mutex[Int].new(1)
raw: Sync.RawMutex
nested: Sync.ReentrantMutex[Int] = Sync.ReentrantMutex[Int].new(1)
ready: Sync.Condvar
word: @bit32

work: function (argument: ref unknown) nothing {
    Os.yield_thread()
}

main: function () nothing {
    thread: Os.Thread = Os.spawn_with(ref work, ref word as ref unknown, Os.default_stack_size).get() else {
        return
    }
    Os.join(thread)
    if Os.available_parallelism() == 0x0 {
        return
    }
    guard: Sync.MutexGuard[Int] = counter.lock()
    guard.value() += 1
    again: Sync.MutexGuard[Int], timed_out: Bool = @move(guard).wait_timeout(ref ready, 0x1)
    again = @move(again).wait(ref ready)
    @move(again).unlock()
    if counter.try_lock() -> held: Sync.MutexGuard[Int] {
        held.value() += 1
    }
    if counter.lock_timeout(0x1) -> held: Sync.MutexGuard[Int] {
        held.value() += 1
    }
    raw.lock()
    if ready.wait_raw_timeout(ref raw, 0x1) {
        ready.notify_one()
    }
    raw.unlock()
    if raw.try_lock() {
        raw.unlock()
    }
    outer: Sync.ReentrantMutexGuard[Int] = nested.lock()
    if nested.try_lock() -> inner: Sync.ReentrantMutexGuard[Int] {
        inner.value() += 1
    }
    if nested.lock_timeout(0x1) -> inner: Sync.ReentrantMutexGuard[Int] {
        inner.value() += 1
    }
    @move(outer).unlock()
    ready.notify_all()
    Os.wait_on_address_timeout(ref word, 0x1, 0x1)
    Os.wake_one(ref word)
    Os.wake_all(ref word)
    @spin_hint()
    output: Os.Handle = Os.standard_output()
    if Os.is_terminal(output) {
        unit: @bit16 = 0x41
        Os.write_console(output, ref unit, 0x1)
    }
    print_line($\"{counter.lock().value()}\")
}
";

#[test]
fn sync_links_natively() {
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = std::env::temp_dir().join(format!("lilac-sync-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch dir");
    std::fs::write(dir.join("sync.lilac"), PROGRAM).expect("writes the program");
    let standard = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/Standard");
    let builds: Vec<(&str, &str)> = TARGETS
        .iter()
        .flat_map(|triple| ["debug", "release"].map(|mode| (*triple, mode)))
        .collect();
    let failures: Vec<String> = support::parallel(&builds, |(triple, mode)| {
        let out = Command::new(env!("CARGO_BIN_EXE_lilac"))
            .args(["build", "-q"])
            .arg(format!("--lib=Standard={}", standard.display()))
            .arg(format!("--target={triple}"))
            .arg(format!("--mode={mode}"))
            .arg(format!(
                "--build-dir={}",
                dir.join(format!("build-{triple}-{mode}")).display()
            ))
            .arg("-o")
            .arg(dir.join(format!("{triple}-{mode}")))
            .arg("sync.lilac")
            .current_dir(&dir)
            .env("LILAC_SYSROOT", &sysroot)
            .env("NO_COLOR", "1")
            .output()
            .expect("runs lilac");
        (!out.status.success()).then(|| {
            format!(
                "{triple} ({mode}):\n{}",
                String::from_utf8_lossy(&out.stderr)
            )
        })
    })
    .into_iter()
    .flatten()
    .collect();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
