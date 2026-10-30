//! `Standard.Io`'s files link on every native target: opening goes
//! through `CreateFileW` (with the path converted by
//! `MultiByteToWideChar`), `openat` or `open`, reading through `ReadFile`
//! or `read`, and `seek`, `size`, `close` and `remove_file` through
//! `SetFilePointerEx`, `GetFileSizeEx`, `CloseHandle`, `DeleteFileW`,
//! `lseek`, `statx`, `close`, `unlinkat` and `unlink`. A missing stub
//! shows up as a failed link.
//!
//! The program uses every `File` function and file function once, and
//! standard output, and is built for each native target at `debug` and
//! `release`, cross-linked with the development sysroot's `lld`. The
//! tests in `tests/standard/src/Io/files.lilac` run them on the host.

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

main: function () nothing {
    write_file(\"io-link.txt\", $\"{1}\\n\") else {
        return
    }
    append_file(\"io-link.txt\", \"2\\n\") else {
        return
    }
    text: String = read_file(\"io-link.txt\") else {
        return
    }
    file: File = File.open_with(\"io-link.txt\", .ReadWrite) else {
        return
    }
    size: Uint64 = file.size() else {
        return
    }
    at: Uint64 = file.seek(0 as Int64, .Start) else {
        return
    }
    bytes: @array(Byte, 0x4) = @array(Byte, 0x4) {}
    count: Int = file.read(Slice[Byte].new(@array_at(ref bytes, 0x0), 4)) else {
        return
    }
    file.write_line(\"3\") else {
        return
    }
    @move(file).close() else {
        return
    }
    remove_file(\"io-link.txt\") else {
        return
    }
    out: Writer = standard_output()
    out.write_line($\"{text}{size} {at} {count}\") else {
        return
    }
}
";

#[test]
fn io_links_natively() {
    let Some(sysroot) = support::sysroot() else {
        eprintln!("skipping: no lld");
        return;
    };
    let dir = std::env::temp_dir().join(format!("lilac-io-link-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("creates scratch dir");
    std::fs::write(dir.join("io.lilac"), PROGRAM).expect("writes the program");
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
            .arg("io.lilac")
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
