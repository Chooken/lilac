//! The `lilac` binary: `check` / `emit-ir` (more subcommands land per milestone).

use lilac_driver::{ErrorFormat, Session};
use lilac_ir::LirModule;
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage:\n  lilac check <file> [--error-format=human|json] [--target=<triple>] [--verbose]\n  lilac emit-ir <file> [--check] [--target=<triple>]"
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{}", usage());
        return ExitCode::from(2);
    }
    match args[0].as_str() {
        "check" => cmd_check(&args[1..]),
        "emit-ir" => cmd_emit_ir(&args[1..]),
        "-h" | "--help" | "help" => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command: {other}\n{}", usage());
            ExitCode::from(2)
        }
    }
}

/// Shared flags parsed by both subcommands.
struct Common {
    file: String,
    check_only: bool,
    session: Session,
}

fn parse_common(args: &[String], allow_check_flag: bool) -> Result<Common, ExitCode> {
    let mut file: Option<String> = None;
    let mut check_only = false;
    let mut session = Session::new();
    for arg in args {
        if let Some(format) = arg.strip_prefix("--error-format=") {
            match format {
                "human" => session.opts.error_format = ErrorFormat::Human,
                "json" => session.opts.error_format = ErrorFormat::Json,
                _ => {
                    eprintln!("unknown --error-format={format}");
                    return Err(ExitCode::from(2));
                }
            }
        } else if let Some(triple) = arg.strip_prefix("--target=") {
            triple.clone_into(&mut session.target.triple);
        } else if *arg == "--verbose" {
            session.opts.verbose = true;
        } else if allow_check_flag && *arg == "--check" {
            check_only = true;
        } else if arg.starts_with("--") {
            eprintln!("unknown flag: {arg}\n{}", usage());
            return Err(ExitCode::from(2));
        } else if file.is_none() {
            file = Some(arg.clone());
        } else {
            eprintln!("expected one file\n{}", usage());
            return Err(ExitCode::from(2));
        }
    }
    let Some(file) = file else {
        eprintln!("missing <file>\n{}", usage());
        return Err(ExitCode::from(2));
    };
    Ok(Common {
        file,
        check_only,
        session,
    })
}

fn read_source(path: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|err| {
        eprintln!("cannot read {path}: {err}");
        ExitCode::from(2)
    })
}

fn cmd_check(args: &[String]) -> ExitCode {
    let common = match parse_common(args, false) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let src = match read_source(&common.file) {
        Ok(src) => src,
        Err(code) => return code,
    };
    let mut session = common.session;
    let code = match session.compile_source(&common.file, &src) {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    };
    eprint!("{}", session.render_diagnostics());
    code
}

fn cmd_emit_ir(args: &[String]) -> ExitCode {
    let common = match parse_common(args, true) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let src = match read_source(&common.file) {
        Ok(src) => src,
        Err(code) => return code,
    };
    let mut session = common.session;
    if session.compile_source(&common.file, &src).is_err() {
        eprint!("{}", session.render_diagnostics());
        return ExitCode::from(1);
    }
    eprint!("{}", session.render_diagnostics());
    let _ = common.check_only;
    let module = LirModule::new(&session.target.triple);
    print!("{}", module.header_text());
    ExitCode::SUCCESS
}
