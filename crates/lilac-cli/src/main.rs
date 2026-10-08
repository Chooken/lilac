//! The `lilac` binary: `check` / `emit-ir` / `emit-llvm` / `build`.

mod build;
mod link;
mod progress;
mod std_fs;

use lilac_codegen::Backend as _;
use lilac_driver::{ErrorFormat, OptimizeMode, Session, Target};
use lilac_error::ErrorGuaranteed;
use lilac_ir::LirModule;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std_fs::StdFs;

fn usage() -> &'static str {
    "usage:\n  lilac check <file-or-dir> [--error-format=human|json] [--color=auto|always|never] [--target=<triple>] [--verbose] [--mode=debug|release|release-small] [--release] [--cache-dir=<path>] [--dump-lir=<dir>] [--export=<name>]... [--lib=<Name>=<dir>]...\n  lilac emit-ir <file-or-dir> [--check] [--target=<triple>] [--mode=debug|release|release-small] [--release] [--cache-dir=<path>] [--dump-lir=<dir>] [--export=<name>]... [--lib=<Name>=<dir>]...\n  lilac emit-llvm <file-or-dir> [--check] [--target=<triple>] [--mode=debug|release|release-small] [--release] [--cache-dir=<path>] [--dump-lir=<dir>] [--export=<name>]... [--lib=<Name>=<dir>]...
  lilac build [<file-or-dir>] [-o <path>] [--build-dir=<dir>] [--mode=debug|release|release-small] [--release] [--target=<triple>] [--lib=<Name>=<dir>]... [--libc] [--linker=<path>] [--link-arg=<arg>]... [--timings] [-q|--quiet] [--error-format=human|json] [--color=auto|always|never] [--dump-lir=<dir>]"
}

/// Maps the driver's [`OptimizeMode`] onto the backend's
/// [`lilac_codegen::OptLevel`].
fn opt_level(mode: OptimizeMode) -> lilac_codegen::OptLevel {
    match mode {
        OptimizeMode::Debug => lilac_codegen::OptLevel::O0,
        OptimizeMode::Release => lilac_codegen::OptLevel::O2,
        OptimizeMode::ReleaseSmall => lilac_codegen::OptLevel::Os,
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
        "build" => build::cmd_build(&args[1..]),
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
    dump_lir: Option<PathBuf>,
    session: Session,
}

/// `--color=auto|always|never`: whether diagnostics and progress are coloured.
fn parse_color(choice: &str) -> Result<bool, ExitCode> {
    match choice {
        "auto" => Ok(progress::auto_color()),
        "always" => Ok(true),
        "never" => Ok(false),
        _ => {
            eprintln!("unknown --color={choice}; want auto|always|never");
            Err(ExitCode::from(2))
        }
    }
}

fn parse_common(args: &[String], allow_check_flag: bool) -> Result<Common, ExitCode> {
    let mut file: Option<String> = None;
    let mut check_only = false;
    let mut target_set = false;
    let mut dump_lir: Option<PathBuf> = None;
    let mut session = Session::new();
    session.opts.color = progress::auto_color();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if let Some(choice) = arg.strip_prefix("--color=") {
            session.opts.color = parse_color(choice)?;
        } else if let Some(format) = arg.strip_prefix("--error-format=") {
            match format {
                "human" => session.opts.error_format = ErrorFormat::Human,
                "json" => session.opts.error_format = ErrorFormat::Json,
                _ => {
                    eprintln!("unknown --error-format={format}");
                    return Err(ExitCode::from(2));
                }
            }
        } else if let Some(value) = arg.strip_prefix("--target=") {
            match Target::parse(value) {
                Ok(target) => {
                    session.target = target;
                    target_set = true;
                }
                Err(err) => {
                    eprintln!("invalid --target={value}: {err}");
                    return Err(ExitCode::from(2));
                }
            }
        } else if *arg == "--verbose" {
            session.opts.verbose = true;
        } else if allow_check_flag && *arg == "--check" {
            check_only = true;
        } else if let Some(mode) = arg.strip_prefix("--mode=") {
            let Some(parsed) = OptimizeMode::parse(mode) else {
                eprintln!("unknown --mode={mode}; want debug|release|release-small");
                return Err(ExitCode::from(2));
            };
            session.optimize = parsed;
        } else if parse_session_flag(arg, &mut session) {
            // Consumed by the session-wide parser above: `--release`.
        } else if let Some(dir) = arg.strip_prefix("--cache-dir=") {
            if dir.is_empty() {
                eprintln!("--cache-dir= needs a path\n{}", usage());
                return Err(ExitCode::from(2));
            }
            session.cache_dir = Some(std::path::PathBuf::from(dir));
        } else if let Some(dir) = arg.strip_prefix("--dump-lir=") {
            if dir.is_empty() {
                eprintln!("--dump-lir= needs a path\n{}", usage());
                return Err(ExitCode::from(2));
            }
            dump_lir = Some(PathBuf::from(dir));
        } else if let Some(mount) = arg.strip_prefix("--lib=") {
            let Some((name, dir)) = mount
                .split_once('=')
                .filter(|(n, d)| !n.is_empty() && !d.is_empty())
            else {
                eprintln!("--lib= needs `Name=dir`\n{}", usage());
                return Err(ExitCode::from(2));
            };
            session
                .libraries
                .push((name.to_owned(), PathBuf::from(dir)));
        } else if let Some(name) = arg.strip_prefix("--export=") {
            if name.is_empty() {
                eprintln!("--export= needs a name\n{}", usage());
                return Err(ExitCode::from(2));
            }
            session.exports.insert(name.to_owned());
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
        dump_lir,
        session,
    })
}

/// Parses the session-wide scalar flag `--release`, returning `true` when `arg`
/// was it.
///
/// Kept out of [`parse_common`] to stay under the pedantic line budget.
fn parse_session_flag(arg: &str, session: &mut Session) -> bool {
    if arg == "--release" {
        session.optimize = OptimizeMode::Release;
        return true;
    }
    false
}

/// Runs the pipeline, optionally writing one `.lir` file per pass boundary into
/// `dump_dir` (`<stem>.<stage>.lir`). The driver does no I/O; this CLI helper
/// owns it. Returns the compile result plus the first dump I/O error, if any —
/// a dump failure never changes the compile result.
fn compile_with_dumps(
    session: &mut Session,
    file: &str,
    src: &str,
    dump_dir: Option<&Path>,
) -> (Result<LirModule, ErrorGuaranteed>, Option<String>) {
    let mut fs = StdFs;
    let Some(dir) = dump_dir else {
        return (
            session.compile_file_with_stages(file, src, &mut fs, &mut |_, _| {}),
            None,
        );
    };
    if let Err(err) = std::fs::create_dir_all(dir) {
        return (
            session.compile_file_with_stages(file, src, &mut fs, &mut |_, _| {}),
            Some(format!(
                "cannot create LIR dump directory {}: {err}",
                dir.display()
            )),
        );
    }
    let stem = Path::new(file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("module")
        .to_owned();
    let mut io_error: Option<String> = None;
    let result = session.compile_file_with_stages(file, src, &mut fs, &mut |stage, lir| {
        if io_error.is_some() {
            return;
        }
        let path = dir.join(format!("{stem}.{stage}.lir"));
        if let Err(err) = std::fs::write(&path, lir.to_text()) {
            io_error = Some(format!("cannot write LIR dump {}: {err}", path.display()));
        }
    });
    (result, io_error)
}

fn read_source(path: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|err| {
        eprintln!("cannot read {path}: {err}");
        ExitCode::from(2)
    })
}

/// Whether `path` names a directory (a project root) rather than a source file.
fn is_dir(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Compiles `path`: a directory is loaded as a folder project, while a file
/// keeps the existing single-source pipeline so every single-file snapshot is
/// byte-identical. Returns the compile result plus any LIR-dump I/O error, or
/// `Err(ExitCode)` when the source file cannot be read (exit 2).
fn compile_input(
    session: &mut Session,
    path: &str,
    dump_dir: Option<&Path>,
) -> Result<(Result<LirModule, ErrorGuaranteed>, Option<String>), ExitCode> {
    if is_dir(path) {
        Ok(compile_project_with_dumps(session, path, dump_dir))
    } else {
        let src = read_source(path)?;
        Ok(compile_with_dumps(session, path, &src, dump_dir))
    }
}

/// Like [`compile_with_dumps`], but for a project directory root: the folder's
/// `*.lilac` files (and every transitively-`using`d folder) are loaded through
/// [`StdFs`] and compiled as one project.
fn compile_project_with_dumps(
    session: &mut Session,
    root: &str,
    dump_dir: Option<&Path>,
) -> (Result<LirModule, ErrorGuaranteed>, Option<String>) {
    let mut fs = StdFs;
    let Some(dir) = dump_dir else {
        return (session.compile_project(Path::new(root), &mut fs), None);
    };
    if let Err(err) = std::fs::create_dir_all(dir) {
        return (
            session.compile_project(Path::new(root), &mut fs),
            Some(format!(
                "cannot create LIR dump directory {}: {err}",
                dir.display()
            )),
        );
    }
    let stem = Path::new(root)
        .file_name()
        .and_then(|stem| stem.to_str())
        .unwrap_or("module")
        .to_owned();
    let mut io_error: Option<String> = None;
    let result =
        session.compile_project_with_stages(Path::new(root), &mut fs, &mut |stage, lir| {
            if io_error.is_some() {
                return;
            }
            let path = dir.join(format!("{stem}.{stage}.lir"));
            if let Err(err) = std::fs::write(&path, lir.to_text()) {
                io_error = Some(format!("cannot write LIR dump {}: {err}", path.display()));
            }
        });
    (result, io_error)
}

fn cmd_check(args: &[String]) -> ExitCode {
    let common = match parse_common(args, false) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let mut session = common.session;
    let (result, dump_err) =
        match compile_input(&mut session, &common.file, common.dump_lir.as_deref()) {
            Ok(pair) => pair,
            Err(code) => return code,
        };
    let code = match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    };
    eprint!("{}", session.render_diagnostics());
    eprint!("{}", session.render_summary());
    if let Some(err) = &dump_err {
        eprintln!("{err}");
        return ExitCode::from(1);
    }
    code
}

fn cmd_emit_ir(args: &[String]) -> ExitCode {
    let common = match parse_common(args, true) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let mut session = common.session;
    let (result, dump_err) =
        match compile_input(&mut session, &common.file, common.dump_lir.as_deref()) {
            Ok(pair) => pair,
            Err(code) => return code,
        };
    eprint!("{}", session.render_diagnostics());
    eprint!("{}", session.render_summary());
    let Ok(module) = result else {
        return ExitCode::from(1);
    };
    if let Some(err) = &dump_err {
        eprintln!("{err}");
    }
    let emitted = module.to_text();
    if common.check_only {
        let snapshot = Path::new(&common.file).with_extension("lir");
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
        return if dump_err.is_some() {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }
    print!("{emitted}");
    if dump_err.is_some() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_emit_llvm(args: &[String]) -> ExitCode {
    let common = match parse_common(args, true) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let mut session = common.session;
    // Unlike `check`/`emit-ir`, `emit-llvm` defaults to the real host so the IR
    // data layout matches the machine it will run on. An explicit `--target=`
    // still wins.
    let target = if common.target_set {
        session.target
    } else {
        session.host
    };
    session.target = target;
    let triple = target.to_triple();
    let (result, dump_err) =
        match compile_input(&mut session, &common.file, common.dump_lir.as_deref()) {
            Ok(pair) => pair,
            Err(code) => return code,
        };
    eprint!("{}", session.render_diagnostics());
    eprint!("{}", session.render_summary());
    let Ok(module) = result else {
        return ExitCode::from(1);
    };
    if let Some(err) = &dump_err {
        eprintln!("{err}");
    }
    let backend = lilac_codegen_llvm::LlvmBackend::new(&triple, opt_level(session.optimize));
    let emitted = match backend.emit_llvm(&module) {
        Ok(emitted) => emitted,
        Err(err) => {
            eprintln!("internal error: LLVM codegen failed: {err}");
            return ExitCode::from(1);
        }
    };
    if common.check_only {
        let snapshot = Path::new(&common.file).with_extension("ll");
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
        return if dump_err.is_some() {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }
    print!("{emitted}");
    if dump_err.is_some() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::{compile_with_dumps, opt_level, parse_common, ExitCode, OptimizeMode, Session};
    use lilac_codegen::OptLevel;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn target_set_tracks_explicit_target() {
        let with = parse_common(&args(&["--target=x86_64-pc-windows-msvc", "f.lilac"]), true)
            .expect("valid args");
        assert!(with.target_set);
        let without = parse_common(&args(&["f.lilac"]), true).expect("valid args");
        assert!(!without.target_set);
    }

    #[test]
    fn mode_maps_to_codegen_level() {
        assert_eq!(opt_level(OptimizeMode::Debug), OptLevel::O0);
        assert_eq!(opt_level(OptimizeMode::Release), OptLevel::O2);
        assert_eq!(opt_level(OptimizeMode::ReleaseSmall), OptLevel::Os);
    }

    #[test]
    fn optimization_flags_last_wins() {
        let common = parse_common(
            &args(&["f.lilac", "--mode=debug", "--mode=release-small"]),
            false,
        )
        .expect("valid args");
        assert_eq!(common.session.optimize, OptimizeMode::ReleaseSmall);
    }

    #[test]
    fn release_is_release_and_cache_dir_parses() {
        let common = parse_common(
            &args(&["f.lilac", "--release", "--cache-dir=out/cache"]),
            false,
        )
        .expect("valid args");
        assert_eq!(common.session.optimize, OptimizeMode::Release);
        assert_eq!(
            common.session.cache_dir,
            Some(std::path::PathBuf::from("out/cache"))
        );
    }

    #[test]
    fn unknown_mode_is_rejected() {
        assert!(parse_common(&args(&["f.lilac", "--mode=bogus"]), false).is_err());
    }

    #[test]
    fn empty_cache_dir_is_rejected() {
        assert!(parse_common(&args(&["f.lilac", "--cache-dir="]), false).is_err());
    }

    #[test]
    fn release_then_opt_flag_last_wins() {
        let common = parse_common(&args(&["f.lilac", "--release", "--mode=debug"]), false)
            .expect("valid args");
        assert_eq!(common.session.optimize, OptimizeMode::Debug);
    }

    #[test]
    fn target_flag_overrides_target_not_host() {
        let common = parse_common(
            &args(&["f.lilac", "--target=x86_64-pc-windows-msvc"]),
            false,
        )
        .expect("valid args");
        assert_eq!(common.session.target.to_triple(), "x86_64-pc-windows-msvc");
        assert_eq!(
            common.session.host.to_triple(),
            lilac_driver::Target::host().to_triple()
        );
    }

    #[test]
    fn unknown_target_is_rejected() {
        assert_eq!(
            parse_common(&args(&["f.lilac", "--target=foo"]), false).err(),
            Some(ExitCode::from(2))
        );
    }

    #[test]
    fn dump_lir_parses_and_rejects_empty() {
        let with =
            parse_common(&args(&["f.lilac", "--dump-lir=out/dumps"]), true).expect("valid args");
        assert_eq!(with.dump_lir, Some(std::path::PathBuf::from("out/dumps")));
        assert!(parse_common(&args(&["f.lilac", "--dump-lir="]), true).is_err());
    }

    #[test]
    fn dump_lir_writes_every_stage() {
        let dir = std::env::temp_dir().join(format!("lilac-dump-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::new();
        let (result, dump_err) = compile_with_dumps(
            &mut session,
            "s.lilac",
            "main: function () nothing {\n    x: @bit_native = @i_add_native(0x1, 0x2)\n}\n",
            Some(&dir),
        );
        assert!(result.is_ok(), "{}", session.render_diagnostics());
        assert!(dump_err.is_none(), "{dump_err:?}");
        for stage in ["lower", "fold", "runtime", "final"] {
            assert!(
                dir.join(format!("s.{stage}.lir")).exists(),
                "missing {stage}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
