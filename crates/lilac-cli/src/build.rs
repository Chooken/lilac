//! `lilac build`: compile, write an object file, and link an executable into
//! the build folder, printing progress as it goes.
//!
//! The build folder is the manifest's `build.dir` (`build` beside
//! `lilac.toml` by default), else `<project>/build` for a folder project
//! and `./build` for a single file (`--build-dir` moves it). Each mode gets
//! a folder (`build/debug`, `build/release`, …), nested under the triple
//! for a cross target (`build/<triple>/debug`). The mode folder holds the
//! executable and `obj/` with the object file, which is kept. `-o` puts the
//! executable elsewhere. In a manifest build the linker runs in the
//! manifest's folder, so a relative link argument is relative to it, and
//! every path Lilac passes it is absolute.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lilac_codegen::Backend as _;
use lilac_driver::{Entry, ErrorFormat, LinkInfo, OptimizeMode, Progress, Session};
use lilac_manifest::{Overrides, Settings};

use crate::link;
use crate::progress::{Reporter, Verbosity};

/// Splits the build-only flags off `args` into `overrides`, returning
/// `-o`'s path and the rest (for `parse_common`).
pub fn parse_build_flags(
    args: &[String],
    overrides: &mut Overrides,
) -> Result<(Option<PathBuf>, Vec<String>), String> {
    let mut output = None;
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
            output = Some(PathBuf::from(nonempty(path, "-o")?));
        } else if let Some(dir) = arg.strip_prefix("--build-dir=") {
            overrides.build_dir = Some(PathBuf::from(nonempty(dir, "--build-dir=")?));
        } else if let Some(path) = arg.strip_prefix("--linker=") {
            overrides.linker = Some(PathBuf::from(nonempty(path, "--linker=")?));
        } else if let Some(value) = arg.strip_prefix("--link-arg=") {
            overrides.link_args.push(nonempty(value, "--link-arg=")?);
        } else {
            rest.push(arg.clone());
        }
    }
    Ok((output, rest))
}

/// Where a build's files go.
#[derive(Debug, PartialEq)]
pub struct Layout {
    /// The object file, `<mode dir>/obj/<name><obj suffix>`.
    pub obj: PathBuf,
    /// The executable.
    pub exe: PathBuf,
}

/// Computes the build layout in the build folder `build_dir`. `output` is
/// `-o`; `cross` is the triple when it differs from the host.
pub fn layout(
    name: &str,
    build_dir: &Path,
    output: Option<&Path>,
    mode: &str,
    cross: Option<&str>,
    info: &LinkInfo,
) -> Layout {
    let mut dir = build_dir.to_path_buf();
    if let Some(triple) = cross {
        dir.push(triple);
    }
    dir.push(mode);
    let exe = output.map_or_else(
        || dir.join(format!("{name}{}", info.exe_suffix)),
        Path::to_path_buf,
    );
    Layout {
        obj: dir.join("obj").join(format!("{name}{}", info.obj_suffix)),
        exe,
    }
}

/// The program name without a manifest: the file stem, or the project
/// folder's name.
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

/// A path made absolute against the working folder, for a linker that runs
/// in another one.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

pub fn cmd_build(args: &[String]) -> ExitCode {
    let mut overrides = Overrides::default();
    let (output, rest) = match parse_build_flags(args, &mut overrides) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}\n{}", crate::HELP_HINT);
            return ExitCode::from(2);
        }
    };
    let mut common = match crate::parse_common(&rest, false) {
        Ok(common) => common,
        Err(code) => return code,
    };
    common.overrides.build_dir = overrides.build_dir;
    common.overrides.linker = overrides.linker;
    common.overrides.link_args = overrides.link_args;
    let (settings, input) = match common.configure() {
        Ok(configured) => configured,
        Err(code) => return code,
    };
    let verbosity = common
        .verbosity(Some(Verbosity::Normal))
        .unwrap_or(Verbosity::Normal);
    let mut session = common.session;
    session.stack_traces = settings
        .stack_traces
        .unwrap_or(session.optimize == OptimizeMode::Debug);
    if settings.target.is_none() {
        session.target = session.host;
    }
    let triple = session.target.to_triple();
    let Some(info) = session.target.link_info() else {
        eprintln!("error: `lilac build` cannot link executables for `{triple}`");
        return ExitCode::from(2);
    };
    let reporter = Reporter::new(verbosity, session.opts.color);

    let is_dir = crate::is_dir(&input);
    let name = settings
        .name
        .clone()
        .unwrap_or_else(|| program_name(&input, is_dir));
    let mode = session.optimize.name();
    let cross = (session.target != session.host).then_some(triple.as_str());
    let build_dir = settings.build_dir.clone().unwrap_or_else(|| {
        if is_dir {
            input.join("build")
        } else {
            PathBuf::from("build")
        }
    });
    let mut paths = layout(&name, &build_dir, output.as_deref(), mode, cross, &info);
    // The linker runs in the manifest's folder, so Lilac's own paths are
    // absolute.
    let link_dir = settings.manifest_dir.clone();
    if link_dir.is_some() {
        paths = Layout {
            obj: absolute(&paths.obj),
            exe: absolute(&paths.exe),
        };
    }

    reporter.status(
        "Compiling",
        &format!("{name} ({}) [{mode}, {triple}]", common.file),
    );
    session.require_entry = true;
    if settings.libc.is_none() {
        session.entry = Entry::Native;
    }
    let hook = reporter.clone();
    session.progress = Some(Progress::new(move |stage, event| hook.stage(stage, event)));
    let (result, dump_err) =
        match crate::compile_input(&mut session, &input, common.dump_lir.as_deref()) {
            Ok(pair) => pair,
            Err(code) => return code,
        };
    reporter.end();
    eprint!("{}", session.render_diagnostics());
    let Ok(module) = result else {
        could_not_compile(&session, &reporter, &name);
        return ExitCode::from(1);
    };
    if let Some(err) = &dump_err {
        eprintln!("{err}");
        return ExitCode::from(1);
    }

    let link = Link {
        settings: &settings,
        info: &info,
        paths: &paths,
        dir: link_dir.as_deref(),
    };
    if let Err(err) = emit_and_link(&module, &session, &link, &reporter) {
        reporter.error(&err);
        return ExitCode::from(1);
    }
    let shown = std::fs::canonicalize(&paths.exe).unwrap_or_else(|_| paths.exe.clone());
    reporter.finished(mode, &display_path(&shown));
    ExitCode::SUCCESS
}

/// The closing line of a build that did not compile (human output only).
fn could_not_compile(session: &Session, reporter: &Reporter, name: &str) {
    if session.opts.error_format != ErrorFormat::Human {
        return;
    }
    let errors = session.diags.error_count();
    let noun = if errors == 1 { "error" } else { "errors" };
    let hidden = match session.hidden_errors() {
        0 => String::new(),
        n => format!("; {n} not shown"),
    };
    reporter.error(&format!(
        "could not compile `{name}` ({errors} {noun}{hidden})"
    ));
}

/// What linking needs.
struct Link<'a> {
    settings: &'a Settings,
    info: &'a LinkInfo,
    paths: &'a Layout,
    /// The folder the linker runs in: the manifest's, else the working
    /// folder (`None`).
    dir: Option<&'a Path>,
}

/// Compiles `module` to the object file and links the executable.
fn emit_and_link(
    module: &lilac_ir::LirModule,
    session: &Session,
    link: &Link<'_>,
    reporter: &Reporter,
) -> Result<(), String> {
    let paths = link.paths;
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
    let (program, link_args, linker_name) = link_command(link, session)?;
    reporter.linking(
        &file_name(&paths.exe),
        &linker_name,
        &link::render(&program, &link_args),
    );
    link::run(&program, &link_args, link.dir)
}

/// The linker program, its arguments, and a short name for the progress
/// line.
fn link_command(
    link: &Link<'_>,
    session: &Session,
) -> Result<(PathBuf, Vec<OsString>, String), String> {
    let exists = |path: &Path| path.is_file() || path.is_dir();
    // In another folder, a path the run was given is made absolute.
    let place = |path: PathBuf| {
        if link.dir.is_some() && path.components().count() > 1 {
            absolute(&path)
        } else {
            path
        }
    };
    let triple = session.target.to_triple();
    let (settings, paths) = (link.settings, link.paths);
    if session.entry == Entry::CMain {
        let program = link::find_driver(
            settings.linker.as_deref(),
            None,
            std::env::var_os("PATH").as_deref(),
            &exists,
        )?;
        let program = place(program);
        let cross = (session.target != session.host).then_some(triple.as_str());
        let args = link::libc_args(&paths.obj, &paths.exe, cross, &settings.link_args);
        let name = program
            .file_stem()
            .map_or_else(|| "cc".to_owned(), |n| n.to_string_lossy().into_owned());
        return Ok((program, args, name));
    }
    let sysroot = link::find_sysroot(
        std::env::var_os(link::SYSROOT_VAR).map(PathBuf::from),
        crate::exe_dir().as_deref(),
        &exists,
    )?;
    let sysroot = if link.dir.is_some() {
        absolute(&sysroot)
    } else {
        sysroot
    };
    let program = sysroot
        .join("bin")
        .join(format!("lld{}", std::env::consts::EXE_SUFFIX));
    let args = link::native_args(
        link.info,
        &triple,
        session.target.arch.as_str(),
        &sysroot,
        &paths.obj,
        &paths.exe,
        &settings.link_args,
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
    use lilac_driver::Target;

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
        let mut overrides = Overrides::default();
        let (output, rest) = parse_build_flags(
            &args(&[
                "--libc",
                "-o",
                "out/x.exe",
                "--release",
                "--link-arg=a.lib",
                "--link-arg=b.lib",
                "--build-dir=b",
                "--linker=clang",
                "-q",
                "src",
            ]),
            &mut overrides,
        )
        .expect("valid");
        assert_eq!(output, Some(PathBuf::from("out/x.exe")));
        assert_eq!(overrides.link_args, ["a.lib", "b.lib"]);
        assert_eq!(overrides.build_dir, Some(PathBuf::from("b")));
        assert_eq!(overrides.linker, Some(PathBuf::from("clang")));
        assert_eq!(rest, ["--libc", "--release", "-q", "src"]);
    }

    #[test]
    fn bad_build_flags_are_rejected() {
        for bad in [
            &["-o"][..],
            &["--build-dir="],
            &["--link-arg="],
            &["--linker="],
        ] {
            assert!(parse_build_flags(&args(bad), &mut Overrides::default()).is_err());
        }
    }

    #[test]
    fn layout_uses_the_build_folder() {
        let single = layout("hello", Path::new("build"), None, "debug", None, &windows());
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
            &Path::new("proj").join("build"),
            None,
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
            Path::new("build"),
            None,
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
    fn output_overrides_the_executable() {
        let paths = layout(
            "app",
            Path::new("b"),
            Some(Path::new("bin/x.exe")),
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
