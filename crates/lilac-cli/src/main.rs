//! The `lilac` binary: `check` / `emit-ir` / `emit-llvm` (more subcommands land
//! per milestone).

use lilac_codegen::Backend as _;
use lilac_driver::{ErrorFormat, OptimizeMode, Session, Target};
use lilac_error::ErrorGuaranteed;
use lilac_ir::LirModule;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn usage() -> &'static str {
    "usage:\n  lilac check <file> [--error-format=human|json] [--target=<triple>] [--verbose] [--allow operator-type-change] [--mode=debug|release|release-small] [--release] [--cache-dir=<path>] [--dump-lir=<dir>] [--export=<name>]...\n  lilac emit-ir <file> [--check] [--target=<triple>] [--mode=debug|release|release-small] [--release] [--cache-dir=<path>] [--dump-lir=<dir>] [--export=<name>]...\n  lilac emit-llvm <file> [--check] [--target=<triple>] [--mode=debug|release|release-small] [--release] [--cache-dir=<path>] [--dump-lir=<dir>] [--export=<name>]...\n  lilac emit-vm <file> [--check] [--target=<triple>] [--mode=debug|release|release-small] [--release] [--cache-dir=<path>] [--dump-lir=<dir>] [--export=<name>]...\n  lilac run <file> [--source=<path>] [--break=<loc>]... [--step] [--trace] [--target=<triple>] [--mode=debug|release|release-small] [--release] [--cache-dir=<path>] [--dump-lir=<dir>] [--export=<name>]..."
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
        "emit-vm" => cmd_emit_vm(&args[1..]),
        "run" => cmd_run(&args[1..]),
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
    source: Option<String>,
    debug: DebugOptions,
    dump_lir: Option<PathBuf>,
    session: Session,
}

/// `run`-only debugger flags, accepted here so the whole command line parses in
/// one place. Task 12 wires them to the interactive `Debugger`; until then they
/// are inert (a debug request still performs a plain run).
#[derive(Default)]
struct DebugOptions {
    breaks: Vec<String>,
    step: bool,
    trace: bool,
}

fn parse_common(args: &[String], allow_check_flag: bool) -> Result<Common, ExitCode> {
    let mut file: Option<String> = None;
    let mut check_only = false;
    let mut target_set = false;
    let mut source: Option<String> = None;
    let mut debug = DebugOptions::default();
    let mut dump_lir: Option<PathBuf> = None;
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
        } else if let Some(mode) = arg.strip_prefix("--mode=") {
            let Some(parsed) = OptimizeMode::parse(mode) else {
                eprintln!("unknown --mode={mode}; want debug|release|release-small");
                return Err(ExitCode::from(2));
            };
            session.optimize = parsed;
        } else if *arg == "--release" {
            session.optimize = OptimizeMode::Release;
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
        } else if let Some(name) = arg.strip_prefix("--export=") {
            if name.is_empty() {
                eprintln!("--export= needs a name\n{}", usage());
                return Err(ExitCode::from(2));
            }
            session.exports.insert(name.to_owned());
        } else if parse_run_flag(arg, &mut source, &mut debug)? {
            // Handled by the run-only parser above.
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
        source,
        debug,
        dump_lir,
        session,
    })
}

/// Parses the `run`-oriented flags `--source=`, `--break=`, `--step`, and
/// `--trace`, returning `Ok(true)` when `arg` was one of them.
///
/// Kept out of [`parse_common`] to stay under the pedantic line budget.
fn parse_run_flag(
    arg: &str,
    source: &mut Option<String>,
    debug: &mut DebugOptions,
) -> Result<bool, ExitCode> {
    if let Some(path) = arg.strip_prefix("--source=") {
        if path.is_empty() {
            eprintln!("--source= needs a path\n{}", usage());
            return Err(ExitCode::from(2));
        }
        *source = Some(path.to_owned());
    } else if let Some(loc) = arg.strip_prefix("--break=") {
        if loc.is_empty() {
            eprintln!("--break= needs a location\n{}", usage());
            return Err(ExitCode::from(2));
        }
        debug.breaks.push(loc.to_owned());
    } else if arg == "--step" {
        debug.step = true;
    } else if arg == "--trace" {
        debug.trace = true;
    } else {
        return Ok(false);
    }
    Ok(true)
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
    let Some(dir) = dump_dir else {
        return (session.compile_lir(file, src), None);
    };
    if let Err(err) = std::fs::create_dir_all(dir) {
        return (
            session.compile_lir(file, src),
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
    let result = session.compile_lir_with_stages(file, src, &mut |stage, lir| {
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
    let (result, dump_err) =
        compile_with_dumps(&mut session, &common.file, &src, common.dump_lir.as_deref());
    let code = match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    };
    eprint!("{}", session.render_diagnostics());
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
    let src = match read_source(&common.file) {
        Ok(src) => src,
        Err(code) => return code,
    };
    let mut session = common.session;
    let (result, dump_err) =
        compile_with_dumps(&mut session, &common.file, &src, common.dump_lir.as_deref());
    let Ok(module) = result else {
        eprint!("{}", session.render_diagnostics());
        return ExitCode::from(1);
    };
    eprint!("{}", session.render_diagnostics());
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
    let src = match read_source(&common.file) {
        Ok(src) => src,
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
    if target.is_vm() {
        eprintln!(
            "emit-llvm requires a non-VM target (got `{}`)",
            target.to_triple()
        );
        return ExitCode::from(2);
    }
    session.target = target;
    let triple = target.to_triple();
    let (result, dump_err) =
        compile_with_dumps(&mut session, &common.file, &src, common.dump_lir.as_deref());
    let Ok(module) = result else {
        eprint!("{}", session.render_diagnostics());
        return ExitCode::from(1);
    };
    eprint!("{}", session.render_diagnostics());
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

/// Emits canonical `.lbc` text for the VM target, or checks it with `--check`.
///
/// Unlike `emit-ir`/`emit-llvm`, `emit-vm` defaults to the canonical VM target
/// (`lilacvm-unknown-none`) when no `--target=` is given; an explicit non-VM
/// target is a usage error (exit 2), matching `emit-llvm`'s refusal of a VM
/// target.
fn cmd_emit_vm(args: &[String]) -> ExitCode {
    let common = match parse_common(args, true) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let src = match read_source(&common.file) {
        Ok(src) => src,
        Err(code) => return code,
    };
    let mut session = common.session;
    if common.target_set {
        if !session.target.is_vm() {
            eprintln!(
                "emit-vm requires a VM target (got `{}`)",
                session.target.to_triple()
            );
            return ExitCode::from(2);
        }
    } else {
        session.target = match Target::parse(lilac_vm::VM_TARGET) {
            Ok(target) => target,
            Err(err) => {
                eprintln!("internal error: cannot construct the VM target: {err}");
                return ExitCode::from(1);
            }
        };
    }
    let (result, dump_err) =
        compile_with_dumps(&mut session, &common.file, &src, common.dump_lir.as_deref());
    let Ok(module) = result else {
        eprint!("{}", session.render_diagnostics());
        return ExitCode::from(1);
    };
    eprint!("{}", session.render_diagnostics());
    if let Some(err) = &dump_err {
        eprintln!("{err}");
    }
    let emitted = match lilac_vm::VmBackend.emit_bytecode(&module, &session.source_map) {
        Ok(emitted) => emitted,
        Err(err) => {
            eprintln!("internal error: VM codegen failed: {err}");
            return ExitCode::from(1);
        }
    };
    if common.check_only {
        let snapshot = Path::new(&common.file).with_extension("lbc");
        let Ok(expected) = std::fs::read_to_string(&snapshot) else {
            eprintln!(
                "cannot read snapshot {}; run emit-vm without --check to generate it",
                snapshot.display()
            );
            return ExitCode::from(1);
        };
        if expected != emitted {
            eprintln!(
                "expected {} to match emit-vm output; re-run without --check to refresh it",
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

/// Compiles `file` to LIR, emits VM bytecode, and executes it on the bytecode
/// interpreter.
///
/// Like `emit-vm`, `run` defaults to the canonical VM target
/// (`lilacvm-unknown-none`) when no `--target=` is given and rejects an
/// explicit non-VM target with exit 2. A normal return maps to its
/// [`ExitCode`]; a VM runtime error prints to stderr and exits 1. `--export`
/// roots survive reachability pruning exactly as in the other subcommands.
fn cmd_run(args: &[String]) -> ExitCode {
    let common = match parse_common(args, false) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let src = match read_source(&common.file) {
        Ok(src) => src,
        Err(code) => return code,
    };
    let mut session = common.session;
    if common.target_set {
        if !session.target.is_vm() {
            eprintln!(
                "run requires a VM target (got `{}`)",
                session.target.to_triple()
            );
            return ExitCode::from(2);
        }
    } else {
        session.target = match Target::parse(lilac_vm::VM_TARGET) {
            Ok(target) => target,
            Err(err) => {
                eprintln!("internal error: cannot construct the VM target: {err}");
                return ExitCode::from(1);
            }
        };
    }
    let (result, dump_err) =
        compile_with_dumps(&mut session, &common.file, &src, common.dump_lir.as_deref());
    let Ok(module) = result else {
        eprint!("{}", session.render_diagnostics());
        return ExitCode::from(1);
    };
    eprint!("{}", session.render_diagnostics());
    if let Some(err) = &dump_err {
        eprintln!("{err}");
    }
    let text = match lilac_vm::VmBackend.emit_bytecode(&module, &session.source_map) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("internal error: VM codegen failed: {err}");
            return ExitCode::from(1);
        }
    };
    // Task 12: Debugger. A breakpoint, `--step`, `--trace`, or a `--source`
    // override will build a `lilac_vm::Debugger` over `text` and drive its
    // REPL on stdin/stdout instead of the plain `run_lbc` below. Until Task 12
    // lands those flags are accepted but inert, so a debug request runs plainly.
    let _debug_requested = common.debug.step
        || common.debug.trace
        || !common.debug.breaks.is_empty()
        || common.source.is_some();
    match lilac_vm::run_lbc(&text) {
        Ok(code) => exit_code(code),
        Err(err) => {
            eprintln!("{err}");
            ExitCode::from(1)
        }
    }
}

/// Maps a VM exit code (`i32`) onto a process [`ExitCode`].
///
/// VM codes are a normal return (`0`) or the abort code (`101`), so they fit a
/// `u8`; a negative or out-of-range code is an internal error reported as 1
/// rather than silently wrapped.
fn exit_code(code: i32) -> ExitCode {
    match u8::try_from(code) {
        Ok(value) => ExitCode::from(value),
        Err(_) => ExitCode::from(1),
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
            "main: function () nothing {\n    x: @bit_native = @i_add(0x1, 0x2)\n}\n",
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
