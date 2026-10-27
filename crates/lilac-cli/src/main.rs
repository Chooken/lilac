//! The `lilac` binary: `check` / `emit-ir` / `emit-llvm` / `build` /
//! `test` / `init` / `explain` / `demangle`.

mod build;
mod init;
mod link;
mod progress;
mod std_fs;
mod test;

use lilac_codegen::Backend as _;
use lilac_driver::{ErrorFormat, OptimizeMode, Progress, Session, Target};
use lilac_error::ErrorGuaranteed;
use lilac_ir::LirModule;
use lilac_manifest::{Manifest, Overrides, Settings, TargetChoice};
use progress::{Reporter, Verbosity};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std_fs::StdFs;

/// The commands, for did-you-mean.
const COMMANDS: [&str; 9] = [
    "check",
    "emit-ir",
    "emit-llvm",
    "build",
    "test",
    "init",
    "explain",
    "demangle",
    "help",
];

/// Every flag any command takes (without its `=value`), for did-you-mean.
const FLAGS: [&str; 21] = [
    "--error-format",
    "--color",
    "--target",
    "--verbose",
    "--mode",
    "--release",
    "--stack-traces",
    "--dump-lir",
    "--lib",
    "--timings",
    "-q",
    "--quiet",
    "--check",
    "-o",
    "--build-dir",
    "--libc",
    "--linker",
    "--link-arg",
    "--filter",
    "-h",
    "--help",
];

/// The line a usage error ends with instead of the whole usage text.
const HELP_HINT: &str = "run `lilac help` for all options";

/// "did you mean `x`?" for a misspelled `word`, else nothing.
fn did_you_mean(word: &str, candidates: &[&str]) -> String {
    lilac_error::closest(word, candidates.iter().copied())
        .map(|best| format!("\nhelp: did you mean `{best}`?"))
        .unwrap_or_default()
}

/// An unknown flag, with the closest known one (keeping its `=value`).
fn unknown_flag(arg: &str) -> String {
    let (name, value) = arg
        .split_once('=')
        .map_or((arg, None), |(n, v)| (n, Some(v)));
    let help = lilac_error::closest(name, FLAGS.iter().copied())
        .map(|best| match value {
            Some(value) => format!("\nhelp: did you mean `{best}={value}`?"),
            None => format!("\nhelp: did you mean `{best}`?"),
        })
        .unwrap_or_default();
    format!("unknown flag: {arg}{help}\n{HELP_HINT}")
}

fn usage() -> &'static str {
    "usage:\n  lilac check [<file-or-dir>] [--error-format=human|json] [--color=auto|always|never] [--target=<triple>|host] [--verbose] [--mode=debug|release|release-small] [--release] [--stack-traces=on|off] [--dump-lir=<dir>] [--lib=<Name>=<dir>]... [--libc[=on|off]] [--timings] [-q|--quiet]\n  lilac emit-ir [<file-or-dir>] [--check] [--target=<triple>|host] [--mode=debug|release|release-small] [--release] [--stack-traces=on|off] [--dump-lir=<dir>] [--lib=<Name>=<dir>]... [--libc[=on|off]] [--timings] [-q|--quiet]\n  lilac emit-llvm [<file-or-dir>] [--check] [--target=<triple>|host] [--mode=debug|release|release-small] [--release] [--stack-traces=on|off] [--dump-lir=<dir>] [--lib=<Name>=<dir>]... [--libc[=on|off]] [--timings] [-q|--quiet]
  lilac build [<file-or-dir>] [-o <path>] [--build-dir=<dir>] [--mode=debug|release|release-small] [--release] [--stack-traces=on|off] [--target=<triple>|host] [--lib=<Name>=<dir>]... [--libc[=on|off]] [--linker=<path>] [--link-arg=<arg>]... [--timings] [-q|--quiet] [--error-format=human|json] [--color=auto|always|never] [--dump-lir=<dir>]
  lilac test [<file-or-dir>] [--filter=<text>]... and `lilac build`'s options
  lilac init [<dir>] [-q|--quiet]
  lilac explain <code>
  lilac demangle [<symbol>...]

A folder with a lilac.toml is a project: its settings apply, and a flag
overrides its setting for one run. The folder defaults to `.`. `lilac init`
starts a project from the install's template (lilac.toml, src/main.lilac).
`lilac test` builds the program's `test \"name\" { … }` blocks and runs each;
`--filter` runs those whose name contains its text."
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
    std::thread::Builder::new()
        .name("lilac".to_owned())
        .stack_size(lilac_driver::COMPILE_STACK_BYTES)
        .spawn(run)
        .expect("spawning the compiler thread")
        .join()
        // The panic message is already printed.
        .unwrap_or(ExitCode::from(101))
}

/// The command line, run on a thread with [`lilac_driver::COMPILE_STACK_BYTES`].
fn run() -> ExitCode {
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
        "test" => test::cmd_test(&args[1..]),
        "init" => init::cmd_init(&args[1..]),
        "explain" => cmd_explain(&args[1..]),
        "demangle" => cmd_demangle(&args[1..]),
        "-h" | "--help" | "help" => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        other => {
            let help = did_you_mean(other, &COMMANDS);
            eprintln!("unknown command: {other}{help}\n{HELP_HINT}");
            ExitCode::from(2)
        }
    }
}

/// Shared flags parsed by every compiling command.
struct Common {
    /// The file or folder to compile: `.` when none is given.
    file: String,
    check_only: bool,
    dump_lir: Option<PathBuf>,
    /// `-q`/`--quiet` (nothing but diagnostics, winning over `--timings`)
    /// or `--timings` (each stage with its duration), when given.
    progress: Option<Verbosity>,
    /// The flags that override a `lilac.toml` setting.
    overrides: Overrides,
    /// A session with only how it prints set; [`Common::configure`] sets
    /// the rest from the settings.
    session: Session,
}

impl Common {
    /// How much progress the command prints: nothing under `-q` or JSON
    /// diagnostics, else stage timings under `--timings`. `normal` is the
    /// command's own default (`build` prints a line per step, the others
    /// nothing).
    fn verbosity(&self, normal: Option<Verbosity>) -> Option<Verbosity> {
        if self.session.opts.error_format == ErrorFormat::Json {
            Some(Verbosity::Quiet)
        } else {
            self.progress.or(normal)
        }
    }

    /// Whether `-q` was given.
    fn quiet(&self) -> bool {
        self.progress == Some(Verbosity::Quiet)
    }

    /// Reads the folder argument's `lilac.toml`, if it has one, resolves
    /// the flags over it and sets the session. Returns the settings and
    /// what to compile: the manifest's `project.root`, else the argument.
    /// A file argument reads no manifest. A manifest with an error is
    /// reported here, and the command stops with 1.
    fn configure(&mut self) -> Result<(Settings, PathBuf), ExitCode> {
        let given = PathBuf::from(&self.file);
        let manifest = if is_dir(&given) {
            let dir = std::path::absolute(&given).unwrap_or_else(|_| given.clone());
            let bundled = lilac_manifest::bundled_dir(
                std::env::var_os(lilac_manifest::LIB_DIR_VAR).map(PathBuf::from),
                exe_dir().as_deref(),
            );
            let quiet = self.quiet();
            let session = &mut self.session;
            match Manifest::read(
                &dir,
                bundled.as_deref(),
                &mut StdFs,
                &mut session.source_map,
                &mut session.diags,
            ) {
                Some(Ok(manifest)) if session.diags.error_count() == 0 => Some(manifest),
                Some(_) => {
                    print_diagnostics(session, quiet);
                    return Err(ExitCode::from(1));
                }
                None => None,
            }
        } else {
            None
        };
        self.overrides.env_linker = std::env::var_os(link::LINKER_VAR)
            .filter(|linker| !linker.is_empty())
            .map(PathBuf::from);
        let settings = Settings::resolve(manifest.as_ref(), &self.overrides, self.session.host);
        settings.apply(&mut self.session);
        self.session.stack_traces = self.overrides.stack_traces.unwrap_or(false);
        let input = settings.root.clone().unwrap_or(given);
        Ok((settings, input))
    }
}

/// The folder holding the running `lilac`.
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// Hooks a `--timings` reporter into `session` for `check` and `emit-*`.
/// Returns it so the caller can close the last stage.
fn stage_timings(verbosity: Option<Verbosity>, session: &mut Session) -> Option<Reporter> {
    if verbosity != Some(Verbosity::Timings) {
        return None;
    }
    let reporter = Reporter::new(Verbosity::Timings, session.opts.color);
    let hook = reporter.clone();
    session.progress = Some(Progress::new(move |stage, event| hook.stage(stage, event)));
    Some(reporter)
}

/// Prints the diagnostics and, unless `-q`, the summary line.
fn print_diagnostics(session: &Session, quiet: bool) {
    eprint!("{}", session.render_diagnostics());
    if !quiet {
        eprint!("{}", session.render_summary());
    }
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
    let mut dump_lir: Option<PathBuf> = None;
    let mut progress: Option<Verbosity> = None;
    let mut overrides = Overrides::default();
    let mut session = Session::new();
    session.opts.color = progress::auto_color();
    for arg in args {
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
            overrides.target = Some(parse_target(value)?);
        } else if parse_progress_flag(arg, &mut progress) {
            // `--timings` or `-q`.
        } else if allow_check_flag && *arg == "--check" {
            check_only = true;
        } else if parse_setting_flag(arg, &mut overrides)? {
            // `--mode=`, `--release`, `--stack-traces=`, `--libc`.
        } else if *arg == "--verbose" {
            session.opts.verbose = true;
        } else if let Some(dir) = arg.strip_prefix("--dump-lir=") {
            if dir.is_empty() {
                eprintln!("--dump-lir= needs a path\n{HELP_HINT}");
                return Err(ExitCode::from(2));
            }
            dump_lir = Some(PathBuf::from(dir));
        } else if let Some(mount) = arg.strip_prefix("--lib=") {
            overrides.libraries.push(parse_library(mount)?);
        } else if arg.starts_with('-') {
            eprintln!("{}", unknown_flag(arg));
            return Err(ExitCode::from(2));
        } else if file.is_none() {
            file = Some(arg.clone());
        } else {
            eprintln!("expected one file or folder\n{HELP_HINT}");
            return Err(ExitCode::from(2));
        }
    }
    Ok(Common {
        file: file.unwrap_or_else(|| ".".to_owned()),
        check_only,
        dump_lir,
        progress,
        overrides,
        session,
    })
}

/// `--target=`: a triple, or `host`.
fn parse_target(value: &str) -> Result<TargetChoice, ExitCode> {
    if value == "host" {
        return Ok(TargetChoice::Host);
    }
    Target::parse(value)
        .map(TargetChoice::Triple)
        .map_err(|err| {
            eprintln!("invalid --target={value}: {err}");
            ExitCode::from(2)
        })
}

/// `--lib=Name=dir`.
fn parse_library(mount: &str) -> Result<(String, PathBuf), ExitCode> {
    let Some((name, dir)) = mount
        .split_once('=')
        .filter(|(n, d)| !n.is_empty() && !d.is_empty())
    else {
        eprintln!("--lib= needs `Name=dir`\n{HELP_HINT}");
        return Err(ExitCode::from(2));
    };
    if name.starts_with('@') {
        eprintln!("--lib={name}: `@` names are reserved for the compiler (`@builtin`)");
        return Err(ExitCode::from(2));
    }
    Ok((name.to_owned(), PathBuf::from(dir)))
}

/// `on`/`off` for a switch flag.
fn parse_on_off(flag: &str, value: &str) -> Result<bool, ExitCode> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => {
            eprintln!("unknown {flag}={value}; want on|off");
            Err(ExitCode::from(2))
        }
    }
}

/// Parses the flags that override a scalar setting (`--mode=`,
/// `--release`, `--stack-traces=` and `--libc[=on|off]`), returning `true`
/// when `arg` was one.
fn parse_setting_flag(arg: &str, overrides: &mut Overrides) -> Result<bool, ExitCode> {
    if arg == "--release" {
        overrides.mode = Some(OptimizeMode::Release);
    } else if let Some(mode) = arg.strip_prefix("--mode=") {
        let Some(parsed) = OptimizeMode::parse(mode) else {
            eprintln!("unknown --mode={mode}; want debug|release|release-small");
            return Err(ExitCode::from(2));
        };
        overrides.mode = Some(parsed);
    } else if let Some(value) = arg.strip_prefix("--stack-traces=") {
        overrides.stack_traces = Some(parse_on_off("--stack-traces", value)?);
    } else if arg == "--libc" {
        overrides.libc = Some(true);
    } else if let Some(value) = arg.strip_prefix("--libc=") {
        overrides.libc = Some(parse_on_off("--libc", value)?);
    } else {
        return Ok(false);
    }
    Ok(true)
}

/// Parses `--timings` and `-q`/`--quiet` into `progress` (quiet wins),
/// returning `true` when `arg` was one.
fn parse_progress_flag(arg: &str, progress: &mut Option<Verbosity>) -> bool {
    match arg {
        "--timings" if *progress != Some(Verbosity::Quiet) => {
            *progress = Some(Verbosity::Timings);
        }
        "--timings" => {}
        "-q" | "--quiet" => *progress = Some(Verbosity::Quiet),
        _ => return false,
    }
    true
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

fn read_source(path: &Path) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|err| {
        eprintln!("cannot read {}: {err}", path.display());
        ExitCode::from(2)
    })
}

/// Whether `path` names a directory (a project root) rather than a source file.
fn is_dir(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Compiles `path`: a directory is loaded as a folder project, while a file
/// keeps the existing single-source pipeline so every single-file snapshot is
/// byte-identical. Returns the compile result plus any LIR-dump I/O error, or
/// `Err(ExitCode)` when the source file cannot be read (exit 2).
fn compile_input(
    session: &mut Session,
    path: &Path,
    dump_dir: Option<&Path>,
) -> Result<(Result<LirModule, ErrorGuaranteed>, Option<String>), ExitCode> {
    if is_dir(path) {
        Ok(compile_project_with_dumps(session, path, dump_dir))
    } else {
        let src = read_source(path)?;
        Ok(compile_with_dumps(
            session,
            &path.to_string_lossy(),
            &src,
            dump_dir,
        ))
    }
}

/// Like [`compile_with_dumps`], but for a project directory root: the folder's
/// `*.lilac` files (and every transitively-`using`d folder) are loaded through
/// [`StdFs`] and compiled as one project.
fn compile_project_with_dumps(
    session: &mut Session,
    root: &Path,
    dump_dir: Option<&Path>,
) -> (Result<LirModule, ErrorGuaranteed>, Option<String>) {
    let mut fs = StdFs;
    let Some(dir) = dump_dir else {
        return (session.compile_project(root, &mut fs), None);
    };
    if let Err(err) = std::fs::create_dir_all(dir) {
        return (
            session.compile_project(root, &mut fs),
            Some(format!(
                "cannot create LIR dump directory {}: {err}",
                dir.display()
            )),
        );
    }
    let stem = root
        .file_name()
        .and_then(|stem| stem.to_str())
        .unwrap_or("module")
        .to_owned();
    let mut io_error: Option<String> = None;
    let result = session.compile_project_with_stages(root, &mut fs, &mut |stage, lir| {
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

/// `lilac explain E010`: prints the code's page from `docs/errors/`.
fn cmd_explain(args: &[String]) -> ExitCode {
    let [id] = args else {
        eprintln!("`lilac explain` takes one error code, like `E010`\n{HELP_HINT}");
        return ExitCode::from(2);
    };
    let Some(code) = lilac_error::codes::lookup(id) else {
        let ids: Vec<&str> = lilac_error::codes::ALL
            .iter()
            .map(|code| code.id())
            .collect();
        let help = did_you_mean(&id.to_ascii_uppercase(), &ids);
        eprintln!("unknown error code: {id}{help}");
        return ExitCode::from(2);
    };
    print!("{}", code.doc());
    ExitCode::SUCCESS
}

/// `lilac demangle Holder[m].make`: each symbol with its types written as
/// Lilac types (`Holder[@bit32].make`), one per line, or as it is when it
/// has none. With no symbol, a filter: it copies standard input to
/// standard output with every symbol in it demangled, so
/// `lilac emit-ir … | lilac demangle` reads like source.
fn cmd_demangle(args: &[String]) -> ExitCode {
    use std::io::{BufRead as _, Write as _};

    if let Some(flag) = args.iter().find(|arg| arg.starts_with('-')) {
        eprintln!("{}", unknown_flag(flag));
        return ExitCode::from(2);
    }
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    let written = if args.is_empty() {
        let mut input = std::io::stdin().lock();
        let mut line = Vec::new();
        loop {
            line.clear();
            match input.read_until(b'\n', &mut line) {
                Ok(0) => break Ok(()),
                Ok(_) => {}
                Err(error) => {
                    eprintln!("error: reading standard input: {error}");
                    return ExitCode::FAILURE;
                }
            }
            // A line that is not UTF-8 holds no symbol of ours.
            let written = match std::str::from_utf8(&line) {
                Ok(text) => out.write_all(lilac_ty::demangle_text(text).as_bytes()),
                Err(_) => out.write_all(&line),
            };
            if written.is_err() {
                break written;
            }
        }
    } else {
        args.iter().try_for_each(|symbol| {
            let shown = lilac_ty::demangle(symbol).unwrap_or_else(|| symbol.clone());
            writeln!(out, "{shown}")
        })
    };
    // A closed output (`| head`) ends the filter quietly.
    match written.and_then(|()| out.flush()) {
        Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => {
            eprintln!("error: writing standard output: {error}");
            ExitCode::FAILURE
        }
        _ => ExitCode::SUCCESS,
    }
}

fn cmd_check(args: &[String]) -> ExitCode {
    let mut common = match parse_common(args, false) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let quiet = common.quiet();
    let (_, input) = match common.configure() {
        Ok(configured) => configured,
        Err(code) => return code,
    };
    let timings = stage_timings(common.verbosity(None), &mut common.session);
    let mut session = common.session;
    let (result, dump_err) = match compile_input(&mut session, &input, common.dump_lir.as_deref()) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    if let Some(reporter) = &timings {
        reporter.end();
    }
    let code = match result {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    };
    print_diagnostics(&session, quiet);
    if let Some(err) = &dump_err {
        eprintln!("{err}");
        return ExitCode::from(1);
    }
    code
}

fn cmd_emit_ir(args: &[String]) -> ExitCode {
    let mut common = match parse_common(args, true) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let quiet = common.quiet();
    let (_, input) = match common.configure() {
        Ok(configured) => configured,
        Err(code) => return code,
    };
    let timings = stage_timings(common.verbosity(None), &mut common.session);
    let mut session = common.session;
    let (result, dump_err) = match compile_input(&mut session, &input, common.dump_lir.as_deref()) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    if let Some(reporter) = &timings {
        reporter.end();
    }
    print_diagnostics(&session, quiet);
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
    let mut common = match parse_common(args, true) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let quiet = common.quiet();
    let (settings, input) = match common.configure() {
        Ok(configured) => configured,
        Err(code) => return code,
    };
    let timings = stage_timings(common.verbosity(None), &mut common.session);
    let mut session = common.session;
    // Unlike `check`/`emit-ir`, `emit-llvm` defaults to the real host so the IR
    // data layout matches the machine it will run on. An explicit `--target=`
    // still wins.
    if settings.target.is_none() {
        session.target = session.host;
    }
    let target = session.target;
    let triple = target.to_triple();
    let (result, dump_err) = match compile_input(&mut session, &input, common.dump_lir.as_deref()) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    if let Some(reporter) = &timings {
        reporter.end();
    }
    print_diagnostics(&session, quiet);
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
    use super::{
        compile_with_dumps, opt_level, parse_common, ExitCode, OptimizeMode, Session, TargetChoice,
    };
    use lilac_codegen::OptLevel;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn target_flag_takes_a_triple_or_host() {
        let with = parse_common(&args(&["--target=x86_64-pc-windows-msvc", "f.lilac"]), true)
            .expect("valid args");
        assert_eq!(
            with.overrides.target,
            Some(TargetChoice::Triple(
                lilac_driver::Target::parse("x86_64-pc-windows-msvc").expect("triple")
            ))
        );
        let host = parse_common(&args(&["--target=host"]), true).expect("valid args");
        assert_eq!(host.overrides.target, Some(TargetChoice::Host));
        let without = parse_common(&args(&["f.lilac"]), true).expect("valid args");
        assert_eq!(without.overrides.target, None);
    }

    #[test]
    fn input_defaults_to_current_dir() {
        let common = parse_common(&args(&["--release"]), false).expect("valid args");
        assert_eq!(common.file, ".");
    }

    #[test]
    fn mode_maps_to_codegen_level() {
        assert_eq!(opt_level(OptimizeMode::Debug), OptLevel::O0);
        assert_eq!(opt_level(OptimizeMode::Release), OptLevel::O2);
        assert_eq!(opt_level(OptimizeMode::ReleaseSmall), OptLevel::Os);
    }

    #[test]
    fn optimization_flags_last_wins() {
        let mode = |flags: &[&str]| {
            parse_common(&args(flags), false)
                .expect("valid args")
                .overrides
                .mode
        };
        assert_eq!(
            mode(&["f.lilac", "--mode=debug", "--mode=release-small"]),
            Some(OptimizeMode::ReleaseSmall)
        );
        assert_eq!(mode(&["f.lilac", "--release"]), Some(OptimizeMode::Release));
        assert_eq!(
            mode(&["f.lilac", "--release", "--mode=debug"]),
            Some(OptimizeMode::Debug)
        );
        assert_eq!(mode(&["f.lilac"]), None);
    }

    #[test]
    fn stack_traces_flag_parses() {
        let traces = |flags: &[&str]| {
            let mut all = vec!["f.lilac"];
            all.extend(flags);
            parse_common(&args(&all), false).map(|common| common.overrides.stack_traces)
        };
        assert_eq!(traces(&[]), Ok(None));
        assert_eq!(traces(&["--release", "--stack-traces=on"]), Ok(Some(true)));
        assert_eq!(traces(&["--stack-traces=off"]), Ok(Some(false)));
        assert_eq!(traces(&["--stack-traces=maybe"]), Err(ExitCode::from(2)));
    }

    #[test]
    fn libc_flag_parses() {
        let libc =
            |flags: &[&str]| parse_common(&args(flags), false).map(|common| common.overrides.libc);
        assert_eq!(libc(&[]), Ok(None));
        assert_eq!(libc(&["--libc"]), Ok(Some(true)));
        assert_eq!(libc(&["--libc=on"]), Ok(Some(true)));
        assert_eq!(libc(&["--libc", "--libc=off"]), Ok(Some(false)));
        assert_eq!(libc(&["--libc=yes"]), Err(ExitCode::from(2)));
    }

    #[test]
    fn library_flags_parse() {
        let common = parse_common(&args(&["--lib=A=a", "--lib=B=b/c"]), false).expect("valid args");
        assert_eq!(
            common.overrides.libraries,
            [("A".to_owned(), "a".into()), ("B".to_owned(), "b/c".into())]
        );
        for bad in ["--lib=A", "--lib==a", "--lib=@builtin=a"] {
            assert!(parse_common(&args(&[bad]), false).is_err(), "{bad}");
        }
    }

    #[test]
    fn unknown_mode_is_rejected() {
        assert!(parse_common(&args(&["f.lilac", "--mode=bogus"]), false).is_err());
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
        for stage in ["lower", "runtime", "final"] {
            assert!(
                dir.join(format!("s.{stage}.lir")).exists(),
                "missing {stage}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
