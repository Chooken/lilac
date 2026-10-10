//! Small comforts: parse errors that say what they found, unclosed blocks
//! at their `{`, help for operators on raw numbers, and the CLI's
//! did-you-mean, `explain`, `--timings` and `-q`.

use std::process::{Command, Output};

fn lilac(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lilac"))
        .args(args)
        .output()
        .expect("runs lilac")
}

fn ui(name: &str) -> String {
    format!("../../tests/ui/{name}")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn parse_error_names_what_it_found() {
    let output = lilac(&["check", &ui("FAILS_parse_found.lilac")]);
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(
        text.contains("expected `:` after declaration name, found `function`"),
        "{text}"
    );
}

#[test]
fn parse_error_at_end_of_file_says_so() {
    let dir = std::env::temp_dir().join("lilac-comforts-eof");
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let file = dir.join("main.lilac");
    std::fs::write(&file, "main:").expect("writes source");
    let output = lilac(&["check", file.to_str().expect("utf-8 path")]);
    let text = stderr(&output);
    assert!(text.contains("found end of file"), "{text}");
}

#[test]
fn unclosed_block_points_at_its_brace() {
    let output = lilac(&["check", &ui("FAILS_unclosed_block.lilac")]);
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(text.contains("FAILS_unclosed_block.lilac:2:27"), "{text}");
    assert!(text.contains("this `{` is never closed"), "{text}");
    assert!(text.contains("the file ends here"), "{text}");
}

#[test]
fn operator_on_raw_bits_names_standard_and_the_intrinsic() {
    let output = lilac(&["check", &ui("FAILS_raw_operator.lilac")]);
    assert!(!output.status.success());
    let text = stderr(&output);
    assert!(
        text.contains(
            "help: `@bit32` has no operators; use `Standard`'s `Int32`, or call `@i_add_32`"
        ),
        "{text}"
    );
    assert!(
        text.contains("help: `@fbit16` has no operators; call `@f_mul_16`"),
        "{text}"
    );
}

#[test]
fn misspelled_command_suggests_the_closest() {
    let output = lilac(&["chek", "x.lilac"]);
    assert_eq!(output.status.code(), Some(2));
    let text = stderr(&output);
    assert!(text.contains("did you mean `check`?"), "{text}");
    assert!(text.contains("run `lilac help` for all options"), "{text}");
    assert!(!text.contains("usage:"), "{text}");
}

#[test]
fn misspelled_flag_suggests_the_closest() {
    let output = lilac(&["check", "--relase", "x.lilac"]);
    assert_eq!(output.status.code(), Some(2));
    let text = stderr(&output);
    assert!(text.contains("did you mean `--release`?"), "{text}");
    assert!(!text.contains("usage:"), "{text}");

    let output = lilac(&["check", "--mod=release", "x.lilac"]);
    assert!(
        stderr(&output).contains("did you mean `--mode=release`?"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn explain_prints_the_code_page() {
    let output = lilac(&["explain", "e042"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let page = String::from_utf8_lossy(&output.stdout);
    assert!(page.starts_with("# E042: "), "{page}");

    let output = lilac(&["explain", "E04"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("unknown error code: E04"));
}

#[test]
fn check_takes_timings_and_quiet() {
    let output = lilac(&["check", "--timings", &ui("arrays.lilac")]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stderr(&output);
    assert!(text.contains("Checking"), "{text}");
    assert!(text.contains("parse"), "{text}");

    let output = lilac(&["check", "-q", &ui("FAILS_raw_operator.lilac")]);
    let quiet = stderr(&output);
    assert!(quiet.contains("error[E043]"), "{quiet}");
    assert!(!quiet.contains("aborting"), "{quiet}");

    for command in ["emit-ir", "emit-llvm"] {
        let output = lilac(&[command, "-q", "--timings", &ui("arrays.lilac")]);
        assert!(output.status.success(), "{command}: {}", stderr(&output));
        assert!(stderr(&output).is_empty(), "{command}: {}", stderr(&output));
    }
}
