//! The `lilac` binary: `check` / `emit-ir` / `emit-llvm` (more subcommands land
//! per milestone).

use lilac_codegen::Backend as _;
use lilac_driver::{ErrorFormat, Opt, Session};
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage:\n  lilac check <file> [--error-format=human|json] [--target=<triple>] [--verbose] [--allow operator-type-change] [-O0|-O1|-O2|-O3|-Os] [--release] [--cache-dir=<path>]\n  lilac emit-ir <file> [--check] [--target=<triple>] [-O0|-O1|-O2|-O3|-Os] [--release] [--cache-dir=<path>]\n  lilac emit-llvm <file> [--check] [--target=<triple>] [-O0|-O1|-O2|-O3|-Os] [--release] [--cache-dir=<path>]"
}

/// Maps the driver's [`Opt`] onto the backend's [`lilac_codegen::OptLevel`].
fn opt_level(opt: Opt) -> lilac_codegen::OptLevel {
    match opt {
        Opt::O0 => lilac_codegen::OptLevel::O0,
        Opt::O1 => lilac_codegen::OptLevel::O1,
        Opt::O2 => lilac_codegen::OptLevel::O2,
        Opt::O3 => lilac_codegen::OptLevel::O3,
        Opt::Os => lilac_codegen::OptLevel::Os,
    }
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
        "emit-llvm" => cmd_emit_llvm(&args[1..]),
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
    target_set: bool,
    session: Session,
}

fn parse_common(args: &[String], allow_check_flag: bool) -> Result<Common, ExitCode> {
    let mut file: Option<String> = None;
    let mut check_only = false;
    let mut target_set = false;
    let mut session = Session::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
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
            target_set = true;
        } else if *arg == "--verbose" {
            session.opts.verbose = true;
        } else if *arg == "--allow" {
            index += 1;
            if args.get(index).map(String::as_str) == Some("operator-type-change") {
                session.warn_on_operator_type_change = false;
            } else {
                eprintln!("unknown --allow value; want `--allow operator-type-change`");
                return Err(ExitCode::from(2));
            }
        } else if let Some(allowed) = arg.strip_prefix("--allow=") {
            if allowed == "operator-type-change" {
                session.warn_on_operator_type_change = false;
            } else {
                eprintln!("unknown --allow={allowed}");
                return Err(ExitCode::from(2));
            }
        } else if allow_check_flag && *arg == "--check" {
            check_only = true;
        } else if let Some(opt) = Opt::parse(arg) {
            session.optimize = opt;
        } else if *arg == "--release" {
            session.optimize = Opt::O2;
        } else if let Some(dir) = arg.strip_prefix("--cache-dir=") {
            if dir.is_empty() {
                eprintln!("--cache-dir= needs a path\n{}", usage());
                return Err(ExitCode::from(2));
            }
            session.cache_dir = Some(std::path::PathBuf::from(dir));
        } else if arg.starts_with('-') {
            eprintln!("unknown flag: {arg}\n{}", usage());
            return Err(ExitCode::from(2));
        } else if file.is_none() {
            file = Some(arg.clone());
        } else {
            eprintln!("expected one file\n{}", usage());
            return Err(ExitCode::from(2));
        }
        index += 1;
    }
    let Some(file) = file else {
        eprintln!("missing <file>\n{}", usage());
        return Err(ExitCode::from(2));
    };
    Ok(Common {
        file,
        check_only,
        target_set,
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
    let Ok(module) = session.compile_lir(&common.file, &src) else {
        eprint!("{}", session.render_diagnostics());
        return ExitCode::from(1);
    };
    eprint!("{}", session.render_diagnostics());
    let emitted = module.to_text();
    if common.check_only {
        let snapshot = std::path::Path::new(&common.file).with_extension("lir");
        let Ok(expected) = std::fs::read_to_string(&snapshot) else {
            eprintln!(
                "cannot read snapshot {}; run emit-ir without --check to generate it",
                snapshot.display()
            );
            return ExitCode::from(1);
        };
        if expected != emitted {
            eprintln!(
                "expected {} to match emit-ir output; re-run without --check to refresh it",
                snapshot.display()
            );
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    print!("{emitted}");
    ExitCode::SUCCESS
}

fn cmd_emit_llvm(args: &[String]) -> ExitCode {
    let common = match parse_common(args, true) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let src = match read_source(&common.file) {
        Ok(src) => src,
        Err(code) => return code,
    };
    let mut session = common.session;
    // Unlike `check`/`emit-ir`, `emit-llvm` defaults to the real host so the IR
    // data layout matches the machine it will run on. An explicit `--target=`
    // still wins.
    let triple = if common.target_set {
        session.target.triple.clone()
    } else {
        session.host.triple.clone()
    };
    session.target.triple.clone_from(&triple);
    let Ok(module) = session.compile_lir(&common.file, &src) else {
        eprint!("{}", session.render_diagnostics());
        return ExitCode::from(1);
    };
    eprint!("{}", session.render_diagnostics());
    let backend = lilac_codegen_llvm::LlvmBackend::new(&triple, opt_level(session.optimize));
    let emitted = match backend.emit_llvm(&module) {
        Ok(emitted) => emitted,
        Err(err) => {
            eprintln!("internal error: LLVM codegen failed: {err}");
            return ExitCode::from(1);
        }
    };
    if common.check_only {
        let snapshot = std::path::Path::new(&common.file).with_extension("ll");
        let Ok(expected) = std::fs::read_to_string(&snapshot) else {
            eprintln!(
                "cannot read snapshot {}; run emit-llvm without --check to generate it",
                snapshot.display()
            );
            return ExitCode::from(1);
        };
        if expected != emitted {
            eprintln!(
                "expected {} to match emit-llvm output; re-run without --check to refresh it",
                snapshot.display()
            );
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
    print!("{emitted}");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{opt_level, parse_common, Opt};
    use lilac_codegen::OptLevel;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn target_set_tracks_explicit_target() {
        let with = parse_common(&args(&["--target=foo", "f.lilac"]), true).expect("valid args");
        assert!(with.target_set);
        let without = parse_common(&args(&["f.lilac"]), true).expect("valid args");
        assert!(!without.target_set);
    }

    #[test]
    fn opt_maps_to_codegen_level() {
        assert_eq!(opt_level(Opt::O0), OptLevel::O0);
        assert_eq!(opt_level(Opt::O1), OptLevel::O1);
        assert_eq!(opt_level(Opt::O2), OptLevel::O2);
        assert_eq!(opt_level(Opt::O3), OptLevel::O3);
        assert_eq!(opt_level(Opt::Os), OptLevel::Os);
    }

    #[test]
    fn optimization_flags_last_wins() {
        let common = parse_common(&args(&["f.lilac", "-O1", "-O3"]), false).expect("valid args");
        assert_eq!(common.session.optimize, Opt::O3);
    }

    #[test]
    fn release_is_o2_and_cache_dir_parses() {
        let common = parse_common(
            &args(&["f.lilac", "--release", "--cache-dir=out/cache"]),
            false,
        )
        .expect("valid args");
        assert_eq!(common.session.optimize, Opt::O2);
        assert_eq!(
            common.session.cache_dir,
            Some(std::path::PathBuf::from("out/cache"))
        );
    }

    #[test]
    fn unknown_flag_is_rejected() {
        assert!(parse_common(&args(&["f.lilac", "-O9"]), false).is_err());
    }

    #[test]
    fn empty_cache_dir_is_rejected() {
        assert!(parse_common(&args(&["f.lilac", "--cache-dir="]), false).is_err());
    }

    #[test]
    fn release_then_opt_flag_last_wins() {
        let common =
            parse_common(&args(&["f.lilac", "--release", "-O0"]), false).expect("valid args");
        assert_eq!(common.session.optimize, Opt::O0);
    }

    #[test]
    fn target_flag_overrides_target_not_host() {
        let common = parse_common(&args(&["f.lilac", "--target=foo"]), false).expect("valid args");
        assert_eq!(common.session.target.triple, "foo");
        assert_eq!(
            common.session.host.triple,
            lilac_driver::Target::host().triple
        );
    }
}
