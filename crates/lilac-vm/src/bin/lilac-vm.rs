//! The standalone `lilac-vm` runner: loads a `.lbc` artifact and executes it
//! with no frontend and no LLVM.
//!
//! ```text
//! lilac-vm <file.lbc> [--source=<path>] [--break=<loc>]... [--step] [--trace]
//! ```
//!
//! A `--break`/`--step`/`--trace`/`--source` request enters the interactive
//! debugger; without any such flag the program runs to completion directly.

use std::process::ExitCode;

use lilac_vm::{Machine, Program, LBC_VERSION, VM_TARGET};

/// Command-line usage line.
const USAGE: &str =
    "usage: lilac-vm <file.lbc> [--source=<path>] [--break=<loc>]... [--step] [--trace]";

/// Parsed `lilac-vm` command line.
struct Cli {
    /// The `.lbc` artifact to run.
    file: String,
    /// `--source=<path>`: source override for the Task 12 debugger.
    source: Option<String>,
    /// `--break=<loc>`: breakpoints for the Task 12 debugger.
    breaks: Vec<String>,
    /// `--step`: start the Task 12 debugger stepping.
    step: bool,
    /// `--trace`: start the Task 12 debugger tracing.
    trace: bool,
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("-h" | "--help")) {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match parse_args(&args) {
        Ok(cli) => run(&cli),
        Err(code) => code,
    }
}

/// Parses `lilac-vm` arguments, rejecting unknown flags and a missing file
/// with exit 2.
fn parse_args(args: &[String]) -> Result<Cli, ExitCode> {
    let mut file: Option<String> = None;
    let mut source: Option<String> = None;
    let mut breaks: Vec<String> = Vec::new();
    let mut step = false;
    let mut trace = false;
    for arg in args {
        if let Some(path) = arg.strip_prefix("--source=") {
            if path.is_empty() {
                eprintln!("--source= needs a path\n{USAGE}");
                return Err(ExitCode::from(2));
            }
            source = Some(path.to_owned());
        } else if let Some(loc) = arg.strip_prefix("--break=") {
            if loc.is_empty() {
                eprintln!("--break= needs a location\n{USAGE}");
                return Err(ExitCode::from(2));
            }
            breaks.push(loc.to_owned());
        } else if arg == "--step" {
            step = true;
        } else if arg == "--trace" {
            trace = true;
        } else if arg.starts_with('-') {
            eprintln!("unknown flag `{arg}`\n{USAGE}");
            return Err(ExitCode::from(2));
        } else if file.is_none() {
            file = Some(arg.clone());
        } else {
            eprintln!("unexpected argument `{arg}`\n{USAGE}");
            return Err(ExitCode::from(2));
        }
    }
    let Some(file) = file else {
        eprintln!("missing <file.lbc>\n{USAGE}");
        return Err(ExitCode::from(2));
    };
    Ok(Cli {
        file,
        source,
        breaks,
        step,
        trace,
    })
}

/// Loads and runs the `.lbc` artifact named on the command line.
///
/// A bad version, a non-VM target, or malformed text exits 2; any other load
/// or runtime failure exits 1; a normal run propagates the program's exit code.
fn run(cli: &Cli) -> ExitCode {
    let text = match std::fs::read_to_string(&cli.file) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("cannot read {}: {err}", cli.file);
            return ExitCode::from(1);
        }
    };
    let program = match Program::parse_text(&text) {
        Ok(program) => program,
        Err(err) => {
            eprintln!("{}: {err}", cli.file);
            return ExitCode::from(2);
        }
    };
    if program.version != LBC_VERSION || program.target != VM_TARGET {
        // `parse_text`/`verify` already enforce both; this keeps the runner's
        // contract explicit and independent of the parser's internals.
        eprintln!(
            "{}: unsupported artifact (version `{}`, target `{}`)",
            cli.file, program.version, program.target
        );
        return ExitCode::from(2);
    }
    // A `--break`, `--step`, `--trace`, or `--source` request drops into the
    // interactive debugger instead of the plain run.
    let debug_requested = cli.step || cli.trace || !cli.breaks.is_empty() || cli.source.is_some();
    if debug_requested {
        return run_debugger(&program, cli);
    }
    let mut machine = match Machine::with_entry(&program, "main") {
        Ok(machine) => machine,
        Err(err) => {
            eprintln!("{}: {err}", cli.file);
            return ExitCode::from(1);
        }
    };
    match machine.run() {
        Ok(code) => exit_code(code),
        Err(err) => {
            eprintln!("{}: {err}", cli.file);
            ExitCode::from(1)
        }
    }
}

/// Drives the interactive debugger over an already-parsed program.
///
/// `--source` text, when readable, overrides the line table's recorded path; a
/// `--break` location that does not resolve is a usage error (exit 2). At end
/// of input the debugger resumes to completion, so a non-interactive invocation
/// still terminates with the program's exit code.
fn run_debugger(program: &Program, cli: &Cli) -> ExitCode {
    let machine = match Machine::with_entry(program, "main") {
        Ok(machine) => machine,
        Err(err) => {
            eprintln!("{}: {err}", cli.file);
            return ExitCode::from(1);
        }
    };
    let source = cli
        .source
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    let mut debugger = lilac_vm::Debugger::new(machine, source);
    for loc in &cli.breaks {
        if let Err(err) = debugger.set_breakpoint(loc) {
            eprintln!("{}: invalid breakpoint `{loc}`: {err}", cli.file);
            return ExitCode::from(2);
        }
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    match debugger.run_repl(stdin.lock(), &mut stdout) {
        Ok(code) => exit_code(code),
        Err(err) => {
            eprintln!("{}: {err}", cli.file);
            ExitCode::from(1)
        }
    }
}

/// Maps a program exit code onto a process [`ExitCode`].
///
/// A negative or out-of-range code cannot reach a process status, so it is
/// reported as 1 rather than silently wrapped.
fn exit_code(code: i32) -> ExitCode {
    match u8::try_from(code) {
        Ok(value) => ExitCode::from(value),
        Err(_) => ExitCode::from(1),
    }
}
