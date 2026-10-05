//! Task 5 runtime-swap tests: the runtime is carried as ordinary LIR
//! (`.lilac.*`) functions, injected after folding and culled by DCE, so a
//! program only emits the runtime it reaches.

use std::process::Command;

fn lilac() -> Command {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
}

fn ui(name: &str) -> String {
    format!("../../tests/ui/{name}")
}

/// Runs `emit-ir` for `name` and returns its stdout.
fn emit_ir(name: &str) -> String {
    let output = lilac()
        .args(["emit-ir"])
        .arg(ui(name))
        .output()
        .expect("runs lilac");
    assert!(
        output.status.success(),
        "{name} emit-ir failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf8 LIR")
}

/// A program that reaches no runtime function emits no runtime text: the
/// injected prelude is culled by DCE.
#[test]
fn no_runtime_fixture_emits_no_runtime_text() {
    let text = emit_ir("hello.lilac");
    assert!(!text.contains(".lilac.byte_buffer"), "{text}");
    assert!(!text.contains(".lilac.format_args"), "{text}");
}

/// `string.lilac` reaches `@byte_buffer_ptr` (which cannot be folded), so the
/// corresponding LIR runtime function survives DCE and is printed.
#[test]
fn string_uses_byte_buffer_runtime() {
    let text = emit_ir("string.lilac");
    assert!(text.contains(".lilac.byte_buffer_ptr"), "{text}");
}

/// `fstring.lilac` builds a `@format_args`, so its `new`/`push` runtime
/// functions survive.
#[test]
fn fstring_uses_format_args_runtime() {
    let text = emit_ir("fstring.lilac");
    assert!(text.contains(".lilac.format_args_new"), "{text}");
    assert!(text.contains(".lilac.format_args_push"), "{text}");
}

/// A program that reads a piece table reaches `@format_args_piece` (and
/// `@format_args_count`), so those bodies survive too.
#[test]
fn format_args_use_reaches_piece_runtime() {
    let text = emit_ir("format_args_use.lilac");
    assert!(text.contains(".lilac.format_args_count"), "{text}");
    assert!(text.contains(".lilac.format_args_piece"), "{text}");
}

/// A non-const `@load_u8`/`@store_u8` reaches the corresponding `.lilac.`
/// LIR bodies. This fixture is also host-compiled and run by the M5b harness,
/// so the bodies lower, link, and execute (the store targets writable local
/// memory, not the read-only string constant).
#[test]
fn non_const_byte_access_reaches_runtime() {
    let text = emit_ir("load_u8.lilac");
    assert!(text.contains(".lilac.load_u8"), "{text}");
    assert!(text.contains(".lilac.store_u8"), "{text}");
    assert!(text.contains(".lilac.byte_buffer_ptr"), "{text}");
}
