//! `lilac build`: compile, write an object file, and link an executable into
//! the build folder, printing progress as it goes.
//!
//! The build folder is `<project>/build` for a folder project and `./build`
//! for a single file (`--build-dir` moves it). Each mode gets a folder
//! (`build/debug`, `build/release`, …), nested under the triple for a cross
//! target (`build/<triple>/debug`). The mode folder holds the executable and
//! `obj/` with the object file, which is kept. `-o` puts the executable
//! elsewhere.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lilac_codegen::Backend as _;
use lilac_driver::{Entry, ErrorFormat, LinkInfo, Os, Progress, Session, Target};
use lilac_error::Level;

use crate::link;
use crate::progress::{Reporter, Verbosity};

/// The flags only `build` takes.
#[derive(Debug, Default, PartialEq)]
pub struct BuildFlags {
    /// `-o <path>`: where the executable goes.
    pub output: Option<PathBuf>,
    /// `--build-dir=<dir>`: the build folder.
    pub build_dir: Option<PathBuf>,
    /// `--libc`: link through a C compiler driver with C's startup code.
    pub libc: bool,
    /// `--linker=<path>`: the `--libc` driver.
    pub linker: Option<PathBuf>,
    /// `--link-arg=<arg>`: extra linker arguments, in order.
    pub link_args: Vec<String>,
    /// `--timings`.
    pub timings: bool,
    /// `-q`/`--quiet`.
    pub quiet: bool,
}

/// Splits the build-only flags off `args`, returning them with the rest
/// (for `parse_common`). A missing input defaults to `.`.
pub fn parse_build_flags(args: &[String]) -> Result<(BuildFlags, Vec<String>), String> {
    let mut flags = BuildFlags::default();
    let mut rest = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let nonempty = |value: &str, flag: &str| {
            if value.is_empty() {
                Err(format!("{flag} needs a value"))
            } else {
                Ok(value.to_owned())
            }
        };
        if arg == "-o" {
            let Some(path) = iter.next() else {
                return Err("-o needs a path".to_owned());
            };
            flags.output = Some(PathBuf::from(nonempty(path, "-o")?));
        } else if let Some(dir) = arg.strip_prefix("--build-dir=") {
            flags.build_dir = Some(PathBuf::from(nonempty(dir, "--build-dir=")?));
        } else if arg == "--libc" {
            flags.libc = true;
        } else if let Some(path) = arg.strip_prefix("--linker=") {
            flags.linker = Some(PathBuf::from(nonempty(path, "--linker=")?));
        } else if let Some(value) = arg.strip_prefix("--link-arg=") {
            flags.link_args.push(nonempty(value, "--link-arg=")?);
        } else if arg == "--timings" {
            flags.timings = true;
        } else if arg == "-q" || arg == "--quiet" {
            flags.quiet = true;
        } else {
            rest.push(arg.clone());
        }
    }
    if !rest.iter().any(|arg| !arg.starts_with('-')) {
        rest.push(".".to_owned());
    }
    Ok((flags, rest))
}

/// Where a build's files go.
#[derive(Debug, PartialEq)]
pub struct Layout {
    /// The object file, `<mode dir>/obj/<name><obj suffix>`.
    pub obj: PathBuf,
    /// The executable.
    pub exe: PathBuf,
}

/// Computes the build layout. `project_dir` is the folder project's root
/// (`None` for a single file, whose build folder is `./build`); `cross` is
/// the triple when it differs from the host.
pub fn layout(
    name: &str,
    project_dir: Option<&Path>,
    flags: &BuildFlags,
    mode: &str,
    cross: Option<&str>,
    info: &LinkInfo,
) -> Layout {
    let mut dir = flags.build_dir.clone().unwrap_or_else(|| {
        project_dir.map_or_else(|| PathBuf::from("build"), |root| root.join("build"))
    });
    if let Some(triple) = cross {
        dir.push(triple);
    }
    dir.push(mode);
    let exe = flags
        .output
        .clone()
        .unwrap_or_else(|| dir.join(format!("{name}{}", info.exe_suffix)));
    Layout {
        obj: dir.join("obj").join(format!("{name}{}", info.obj_suffix)),
        exe,
    }
}

/// Whether the native runtime (no C library) supports `target` yet. macOS
/// follows in a later step; until then it builds with `--libc`.
fn native_supported(target: Target) -> bool {
    matches!(target.os, Os::Windows | Os::Linux)
}

/// The program name: the file stem, or the project folder's name.
fn program_name(input: &Path, is_dir: bool) -> String {
    let named = if is_dir {
        std::fs::canonicalize(input).ok().and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
    } else {
        input
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    };
    named.unwrap_or_else(|| "main".to_owned())
}

/// Counts the session's errors, including suppressed ones.
fn error_count(session: &Session) -> usize {
    session
        .diags
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Error)
        .count()
        + session.diags.suppressed_errors()
}

pub fn cmd_build(args: &[String]) -> ExitCode {
    let (flags, rest) = match parse_build_flags(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}\n{}", crate::usage());
            return ExitCode::from(2);
        }
    };
    let common = match crate::parse_common(&rest, false) {
        Ok(common) => common,
        Err(code) => return code,
    };
    let mut session = common.session;
    if !common.target_set {
        session.target = session.host;
    }
    let triple = session.target.to_triple();
    let Some(info) = session.target.link_info() else {
        eprintln!("error: `lilac build` cannot link executables for `{triple}`");
        return ExitCode::from(2);
    };
    if !flags.libc && !native_supported(session.target) {
        eprintln!(
            "error: the native runtime is not available for `{triple}` yet; build with --libc"
        );
        return ExitCode::from(2);
    }
    let verbosity = if flags.quiet || session.opts.error_format == ErrorFormat::Json {
        Verbosity::Quiet
    } else if flags.timings {
        Verbosity::Timings
    } else {
        Verbosity::Normal
    };
    let reporter = Reporter::new(verbosity);

    let input = Path::new(&common.file);
    let is_dir = crate::is_dir(&common.file);
    let name = program_name(input, is_dir);
    let mode = session.optimize.name();
    let cross = (session.target != session.host).then_some(triple.as_str());
    let paths = layout(&name, is_dir.then_some(input), &flags, mode, cross, &info);

    reporter.status(
        "Compiling",
        &format!("{name} ({}) [{mode}, {triple}]", input.display()),
    );
    session.require_entry = true;
    session.entry = if flags.libc {
        Entry::CMain
    } else {
        Entry::Native
    };
    let hook = reporter.clone();
    session.progress = Some(Progress::new(move |stage, event| hook.stage(stage, event)));
    let (result, dump_err) =
        match crate::compile_input(&mut session, &common.file, common.dump_lir.as_deref()) {
            Ok(pair) => pair,
            Err(code) => return code,
        };
    reporter.end();
    eprint!("{}", session.render_diagnostics());
    let Ok(module) = result else {
        if session.opts.error_format == ErrorFormat::Human {
            let errors = error_count(&session);
            let noun = if errors == 1 { "error" } else { "errors" };
            reporter.error(&format!("could not compile `{name}` ({errors} {noun})"));
        }
        return ExitCode::from(1);
    };
    if let Some(err) = &dump_err {
        eprintln!("{err}");
        return ExitCode::from(1);
    }

    if let Err(err) = emit_and_link(&module, &session, &flags, &info, &paths, &reporter) {
        reporter.error(&err);
        return ExitCode::from(1);
    }
    let shown = std::fs::canonicalize(&paths.exe).unwrap_or_else(|_| paths.exe.clone());
    reporter.finished(mode, &display_path(&shown));
    ExitCode::SUCCESS
}

/// Compiles `module` to the object file and links the executable.
fn emit_and_link(
    module: &lilac_ir::LirModule,
    session: &Session,
    flags: &BuildFlags,
    info: &LinkInfo,
    paths: &Layout,
    reporter: &Reporter,
) -> Result<(), String> {
    let file_name = |path: &Path| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    let opt = crate::opt_level(session.optimize);
    let opt_name = format!("{opt:?}");
    let object_name = file_name(&paths.obj);
    let backend = lilac_codegen_llvm::LlvmBackend::new(&session.target.to_triple(), opt);
    let bytes = backend.emit_object(module, session.entry, &mut |step| {
        reporter.codegen(step, &opt_name, &object_name);
    })?;
    write_file(&paths.obj, &bytes)?;
    if let Some(parent) = paths.exe.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    let (program, link_args, linker_name) = link_command(flags, session, info, paths)?;
    reporter.linking(
        &file_name(&paths.exe),
        &linker_name,
        &link::render(&program, &link_args),
    );
    link::run(&program, &link_args)
}

/// The linker program, its arguments, and a short name for the progress
/// line.
fn link_command(
    flags: &BuildFlags,
    session: &Session,
    info: &LinkInfo,
    paths: &Layout,
) -> Result<(PathBuf, Vec<OsString>, String), String> {
    let exists = |path: &Path| path.is_file() || path.is_dir();
    let triple = session.target.to_triple();
    if flags.libc {
        let program = link::find_driver(
            flags.linker.as_deref(),
            std::env::var_os(link::LINKER_VAR).map(PathBuf::from),
            std::env::var_os("PATH").as_deref(),
            &exists,
        )?;
        let cross = (session.target != session.host).then_some(triple.as_str());
        let args = link::libc_args(&paths.obj, &paths.exe, cross, &flags.link_args);
        let name = program
            .file_stem()
            .map_or_else(|| "cc".to_owned(), |n| n.to_string_lossy().into_owned());
        return Ok((program, args, name));
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    let sysroot = link::find_sysroot(
        std::env::var_os(link::SYSROOT_VAR).map(PathBuf::from),
        exe_dir.as_deref(),
        &exists,
    )?;
    let program = sysroot
        .join("bin")
        .join(format!("lld{}", std::env::consts::EXE_SUFFIX));
    let args = link::native_args(
        info,
        &triple,
        session.target.arch.as_str(),
        &sysroot,
        &paths.obj,
        &paths.exe,
        &flags.link_args,
    );
    Ok((program, args, "lld".to_owned()))
}

/// Writes `bytes` to `path`, creating its folder.
fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("cannot create {}: {err}", parent.display()))?;
    }
    std::fs::write(path, bytes).map_err(|err| format!("cannot write {}: {err}", path.display()))
}

/// A path for messages, without Windows' `\\?\` verbatim prefix.
fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    text.strip_prefix(r"\\?\")
        .map_or(text.clone(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_owned()).collect()
    }

    fn windows() -> LinkInfo {
        Target::parse("x86_64-pc-windows-msvc")
            .expect("triple")
            .link_info()
            .expect("links")
    }

    #[test]
    fn build_flags_split_from_common_flags() {
        let (flags, rest) = parse_build_flags(&args(&[
            "--libc",
            "-o",
            "out/x.exe",
            "--release",
            "--link-arg=a.lib",
            "--link-arg=b.lib",
            "-q",
            "src",
        ]))
        .expect("valid");
        assert!(flags.libc && flags.quiet);
        assert_eq!(flags.output, Some(PathBuf::from("out/x.exe")));
        assert_eq!(flags.link_args, ["a.lib", "b.lib"]);
        assert_eq!(rest, ["--release", "src"]);
    }

    #[test]
    fn input_defaults_to_current_dir() {
        let (_, rest) = parse_build_flags(&args(&["--release"])).expect("valid");
        assert_eq!(rest, ["--release", "."]);
    }

    #[test]
    fn bad_build_flags_are_rejected() {
        assert!(parse_build_flags(&args(&["-o"])).is_err());
        assert!(parse_build_flags(&args(&["--build-dir="])).is_err());
        assert!(parse_build_flags(&args(&["--link-arg="])).is_err());
    }

    #[test]
    fn layout_uses_the_build_folder() {
        let flags = BuildFlags::default();
        let single = layout("hello", None, &flags, "debug", None, &windows());
        assert_eq!(
            single,
            Layout {
                obj: Path::new("build")
                    .join("debug")
                    .join("obj")
                    .join("hello.obj"),
                exe: Path::new("build").join("debug").join("hello.exe"),
            }
        );
        let project = layout(
            "app",
            Some(Path::new("proj")),
            &flags,
            "release",
            None,
            &windows(),
        );
        assert_eq!(
            project.exe,
            Path::new("proj")
                .join("build")
                .join("release")
                .join("app.exe")
        );
        let cross = layout(
            "app",
            None,
            &flags,
            "debug",
            Some("x86_64-unknown-linux-gnu"),
            &windows(),
        );
        assert_eq!(
            cross.exe,
            Path::new("build")
                .join("x86_64-unknown-linux-gnu")
                .join("debug")
                .join("app.exe")
        );
    }

    #[test]
    fn native_runtime_is_windows_and_linux_for_now() {
        let target = |triple| Target::parse(triple).expect("triple");
        assert!(native_supported(target("x86_64-pc-windows-msvc")));
        assert!(native_supported(target("aarch64-pc-windows-msvc")));
        assert!(native_supported(target("x86_64-unknown-linux-gnu")));
        assert!(native_supported(target("aarch64-unknown-linux-gnu")));
        assert!(!native_supported(target("aarch64-apple-darwin")));
    }

    #[test]
    fn layout_overrides() {
        let flags = BuildFlags {
            build_dir: Some(PathBuf::from("b")),
            output: Some(PathBuf::from("bin/x.exe")),
            ..BuildFlags::default()
        };
        let paths = layout(
            "app",
            Some(Path::new("proj")),
            &flags,
            "debug",
            None,
            &windows(),
        );
        assert_eq!(paths.exe, PathBuf::from("bin/x.exe"));
        assert_eq!(
            paths.obj,
            Path::new("b").join("debug").join("obj").join("app.obj")
        );
    }
}
