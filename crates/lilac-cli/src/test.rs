//! `lilac test`: build the program's `test "name" { … }` blocks into a test
//! executable (`<name>-test`, in the build folder like `lilac build`'s),
//! then run it once per test, in parallel, naming the test by its index in
//! the environment variable `LILAC_TEST`. A test passes when its process
//! exits 0; a panic is an abort, so each test needs a process of its own.
//! The tests of mounted libraries are not run.

use std::path::Path;
use std::process::{Command, ExitCode, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use lilac_driver::TestInfo;

use crate::build;

/// The variable the test executable reads (`Standard.Testing.run`).
const TEST_VAR: &str = "LILAC_TEST";

/// Splits `--filter=TEXT` (any number) off `args`.
fn parse_test_flags(args: &[String]) -> Result<(Vec<String>, Vec<String>), String> {
    let mut filters = Vec::new();
    let mut rest = Vec::new();
    for arg in args {
        if let Some(text) = arg.strip_prefix("--filter=") {
            if text.is_empty() {
                return Err("--filter= needs a value".to_owned());
            }
            filters.push(text.to_owned());
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((filters, rest))
}

/// Whether a test runs: every test without a filter, else one whose name
/// (or namespace) contains a filter.
fn selected(test: &TestInfo, filters: &[String]) -> bool {
    filters.is_empty()
        || filters
            .iter()
            .any(|f| test.name.contains(f.as_str()) || test.namespace.contains(f.as_str()))
}

/// How a test reads in the results: `test "name"`, after its namespace
/// outside the root.
fn title(test: &TestInfo) -> String {
    if test.namespace.is_empty() {
        format!("test \"{}\"", test.name)
    } else {
        format!("{} test \"{}\"", test.namespace, test.name)
    }
}

pub fn cmd_test(args: &[String]) -> ExitCode {
    let (filters, rest) = match parse_test_flags(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}\n{}", crate::HELP_HINT);
            return ExitCode::from(2);
        }
    };
    let built = match build::build(&rest, true) {
        Ok(built) => built,
        Err(code) => return code,
    };
    let tests = &built.session.tests;
    let chosen: Vec<usize> = (0..tests.len())
        .filter(|&i| selected(&tests[i], &filters))
        .collect();
    let filtered = tests.len() - chosen.len();
    let noun = if chosen.len() == 1 { "test" } else { "tests" };
    built.reporter.status(
        "Running",
        &format!(
            "{} {noun} ({})",
            chosen.len(),
            build::display_path(&built.exe)
        ),
    );
    let mut failures = Vec::new();
    let mut passed = 0;
    run_each(&built.exe, &chosen, |slot, outcome| {
        let test = &tests[chosen[slot]];
        match outcome {
            Ok(output) if output.status.success() => {
                passed += 1;
                println!("{} ... ok", title(test));
            }
            outcome => {
                println!("{} ... FAILED", title(test));
                failures.push((test, outcome));
            }
        }
    });
    if !failures.is_empty() {
        println!("\nfailures:");
        for (test, outcome) in &failures {
            println!("\n---- {} at {} ----", title(test), test.location);
            match outcome {
                Ok(output) => {
                    print!("{}", String::from_utf8_lossy(&output.stdout));
                    print!("{}", String::from_utf8_lossy(&output.stderr));
                    match output.status.code() {
                        Some(code) => println!("(exit code {code})"),
                        None => println!("({})", output.status),
                    }
                }
                Err(err) => println!("could not run the test: {err}"),
            }
        }
    }
    let verdict = if failures.is_empty() { "ok" } else { "FAILED" };
    println!(
        "\ntest result: {verdict}. {passed} passed; {} failed; {filtered} filtered out",
        failures.len()
    );
    if failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Runs `exe` once per index in `indices`, on as many threads as the
/// machine has, and hands each outcome to `report` in `indices`' order (by
/// position) as soon as it and those before it are in.
fn run_each(exe: &Path, indices: &[usize], mut report: impl FnMut(usize, Result<Output, String>)) {
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(indices.len());
    let next = AtomicUsize::new(0);
    let (send, receive) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..workers {
            let send = send.clone();
            let next = &next;
            scope.spawn(move || loop {
                let slot = next.fetch_add(1, Ordering::Relaxed);
                let Some(&index) = indices.get(slot) else {
                    break;
                };
                let outcome = Command::new(exe)
                    .env(TEST_VAR, index.to_string())
                    .output()
                    .map_err(|err| err.to_string());
                if send.send((slot, outcome)).is_err() {
                    break;
                }
            });
        }
        drop(send);
        let mut waiting: Vec<Option<Result<Output, String>>> =
            (0..indices.len()).map(|_| None).collect();
        let mut shown = 0;
        for (slot, outcome) in receive {
            waiting[slot] = Some(outcome);
            while let Some(outcome) = waiting.get_mut(shown).and_then(Option::take) {
                report(shown, outcome);
                shown += 1;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, namespace: &str) -> TestInfo {
        TestInfo {
            name: name.to_owned(),
            namespace: namespace.to_owned(),
            location: String::new(),
        }
    }

    #[test]
    fn filters_split_from_the_rest() {
        let args: Vec<String> = ["--filter=add", "--release", "--filter=sub", "src"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let (filters, rest) = parse_test_flags(&args).expect("valid");
        assert_eq!(filters, ["add", "sub"]);
        assert_eq!(rest, ["--release", "src"]);
        assert!(parse_test_flags(&["--filter=".to_owned()]).is_err());
    }

    #[test]
    fn a_filter_matches_names_and_namespaces() {
        let test = info("adds two numbers", "Math.Integers");
        assert!(selected(&test, &[]));
        assert!(selected(&test, &["two".to_owned()]));
        assert!(selected(&test, &["Integers".to_owned()]));
        assert!(!selected(&test, &["floats".to_owned()]));
        assert_eq!(title(&test), "Math.Integers test \"adds two numbers\"");
        assert_eq!(title(&info("x", "")), "test \"x\"");
    }
}
