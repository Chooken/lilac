//! Linking for `lilac build`: finding the linker and building its command
//! line.
//!
//! Native mode runs the `lld` shipped in the sysroot with a fixed command
//! line per [`LinkFlavor`]; nothing is looked up on the machine. `--libc` mode
//! links through a C compiler driver (`--linker`, `LILAC_LINKER`, else `cc`
//! or `clang` on `PATH`) with C's startup code. Discovery takes the
//! environment and an exists-check as arguments, so it is unit-tested
//! without touching the machine.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use lilac_driver::{LinkFlavor, LinkInfo};

/// The sysroot environment override.
pub const SYSROOT_VAR: &str = "LILAC_SYSROOT";
/// The `--libc` linker environment override.
pub const LINKER_VAR: &str = "LILAC_LINKER";

/// Finds the sysroot: `LILAC_SYSROOT`, else `<exe dir>/../lib/lilac`.
pub fn find_sysroot(
    env_sysroot: Option<PathBuf>,
    exe_dir: Option<&Path>,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    if let Some(dir) = env_sysroot {
        return if exists(&dir) {
            Ok(dir)
        } else {
            Err(format!(
                "{SYSROOT_VAR} is set to {}, which does not exist",
                dir.display()
            ))
        };
    }
    let bundled = exe_dir.map(|dir| dir.join("..").join("lib").join("lilac"));
    match bundled {
        Some(dir) if exists(&dir) => Ok(dir),
        _ => Err(format!(
            "cannot find the Lilac sysroot; looked in {SYSROOT_VAR} and {}",
            bundled.map_or_else(
                || "<lilac dir>/../lib/lilac".to_owned(),
                |dir| dir.display().to_string()
            )
        )),
    }
}

/// Finds the C compiler driver for `--libc`: `flag` (`--linker`), else
/// `LILAC_LINKER`, else `cc` then `clang` on `PATH`.
pub fn find_driver(
    flag: Option<&Path>,
    env_linker: Option<PathBuf>,
    path_var: Option<&OsStr>,
    exists: &dyn Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    if let Some(path) = flag {
        return Ok(path.to_owned());
    }
    if let Some(path) = env_linker {
        return Ok(path);
    }
    let dirs: Vec<PathBuf> = path_var
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    for name in ["cc", "clang"] {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        if let Some(found) = dirs.iter().map(|dir| dir.join(&file)).find(|p| exists(p)) {
            return Ok(found);
        }
    }
    Err(format!(
        "--libc needs a C compiler to link; none found (looked at --linker, {LINKER_VAR}, and `cc`/`clang` on PATH)"
    ))
}

/// The arguments for a native link with the sysroot's `lld`.
pub fn native_args(
    info: &LinkInfo,
    triple: &str,
    arch: &str,
    sysroot: &Path,
    obj: &Path,
    exe: &Path,
    link_args: &[String],
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-flavor".into(), info.flavor.lld_flavor().into()];
    args.push(obj.into());
    for lib in info.system_libs {
        args.push(sysroot.join(triple).join(lib).into());
    }
    match info.flavor {
        LinkFlavor::Coff => {
            args.push(format!("/entry:{}", info.entry_symbol).into());
            args.push("/subsystem:console".into());
            args.push("/nodefaultlib".into());
            let mut out = OsString::from("/out:");
            out.push(exe);
            args.push(out);
        }
        LinkFlavor::Elf => {
            match info.dynamic_linker {
                Some(loader) if link_args.iter().any(|arg| is_shared_library(arg)) => {
                    args.extend(["-dynamic-linker".into(), loader.into()]);
                }
                _ => args.push("-static".into()),
            }
            args.extend(["-e".into(), info.entry_symbol.into()]);
            args.extend(["-o".into(), exe.into()]);
        }
        LinkFlavor::MachO => {
            let arch = if arch == "aarch64" { "arm64" } else { arch };
            args.extend(["-arch".into(), arch.into()]);
            args.extend([
                "-platform_version".into(),
                "macos".into(),
                "11.0".into(),
                "11.0".into(),
            ]);
            args.extend(["-e".into(), format!("_{}", info.entry_symbol).into()]);
            args.extend(["-o".into(), exe.into()]);
        }
    }
    args.extend(link_args.iter().map(OsString::from));
    args
}

/// Whether a link argument names a shared library (`libfoo.so`,
/// `libfoo.so.1`, or `-lfoo`), which makes an ELF executable dynamically
/// linked: the loader then brings in the libraries it depends on, `libc.so`
/// among them.
fn is_shared_library(arg: &str) -> bool {
    arg.starts_with("-l")
        || arg.contains(".so.")
        || Path::new(arg).extension().is_some_and(|ext| ext == "so")
}

/// The arguments for a `--libc` link through a C compiler driver. `cross` is
/// the target triple when it differs from the host.
pub fn libc_args(
    obj: &Path,
    exe: &Path,
    cross: Option<&str>,
    link_args: &[String],
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![obj.into(), "-o".into(), exe.into()];
    if let Some(triple) = cross {
        args.push(format!("--target={triple}").into());
    }
    args.extend(link_args.iter().map(OsString::from));
    args
}

/// Renders a command line for messages.
pub fn render(program: &Path, args: &[OsString]) -> String {
    let mut line = program.display().to_string();
    for arg in args {
        line.push(' ');
        let arg = arg.to_string_lossy();
        if arg.contains(' ') {
            line.push('"');
            line.push_str(&arg);
            line.push('"');
        } else {
            line.push_str(&arg);
        }
    }
    line
}

/// Runs the linker. `Err` carries the message to print.
pub fn run(program: &Path, args: &[OsString]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("cannot run linker {}: {err}", program.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let mut message = String::from_utf8_lossy(&output.stderr).into_owned();
    message.push_str(&String::from_utf8_lossy(&output.stdout));
    Err(format!(
        "linking failed ({})\n{}\n  command: {}",
        output.status,
        message.trim_end(),
        render(program, args)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lilac_driver::Target;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn info(triple: &str) -> LinkInfo {
        Target::parse(triple)
            .expect("triple")
            .link_info()
            .expect("links")
    }

    #[test]
    fn sysroot_prefers_env_then_exe_dir() {
        let yes = |_: &Path| true;
        let no = |_: &Path| false;
        let env = Some(PathBuf::from("/sys"));
        assert_eq!(
            find_sysroot(env.clone(), Some(Path::new("/bin")), &yes),
            Ok(PathBuf::from("/sys"))
        );
        assert!(find_sysroot(env, None, &no)
            .unwrap_err()
            .contains("does not exist"));
        assert_eq!(
            find_sysroot(None, Some(Path::new("/opt/lilac")), &yes),
            Ok(Path::new("/opt/lilac").join("..").join("lib").join("lilac"))
        );
        let err = find_sysroot(None, Some(Path::new("/opt/lilac")), &no).unwrap_err();
        assert!(err.contains(SYSROOT_VAR) && err.contains("lib"), "{err}");
    }

    #[test]
    fn driver_prefers_flag_then_env_then_path() {
        let yes = |_: &Path| true;
        let flag = Path::new("/my/cc");
        assert_eq!(
            find_driver(Some(flag), Some(PathBuf::from("/env/cc")), None, &yes),
            Ok(flag.to_owned())
        );
        assert_eq!(
            find_driver(None, Some(PathBuf::from("/env/cc")), None, &yes),
            Ok(PathBuf::from("/env/cc"))
        );
        let path = std::env::join_paths([Path::new("/a"), Path::new("/b")]).expect("joins");
        let clang = format!("clang{}", std::env::consts::EXE_SUFFIX);
        let only_clang_in_b = |p: &Path| p == Path::new("/b").join(&clang);
        assert_eq!(
            find_driver(None, None, Some(&path), &only_clang_in_b),
            Ok(Path::new("/b").join(&clang))
        );
        let err = find_driver(None, None, Some(&path), &|_| false).unwrap_err();
        assert!(err.contains(LINKER_VAR), "{err}");
    }

    #[test]
    fn coff_native_command() {
        let args = native_args(
            &info("x86_64-pc-windows-msvc"),
            "x86_64-pc-windows-msvc",
            "x86_64",
            Path::new("sys"),
            Path::new("o.obj"),
            Path::new("o.exe"),
            &["extra.lib".to_owned()],
        );
        let kernel32 = Path::new("sys")
            .join("x86_64-pc-windows-msvc")
            .join("kernel32.lib");
        assert_eq!(
            strings(&args),
            [
                "-flavor",
                "link",
                "o.obj",
                &kernel32.display().to_string(),
                "/entry:.lilac.native_entry",
                "/subsystem:console",
                "/nodefaultlib",
                "/out:o.exe",
                "extra.lib",
            ]
        );
    }

    #[test]
    fn elf_native_command() {
        let args = native_args(
            &info("x86_64-unknown-linux-gnu"),
            "x86_64-unknown-linux-gnu",
            "x86_64",
            Path::new("sys"),
            Path::new("o.o"),
            Path::new("o"),
            &[],
        );
        assert_eq!(
            strings(&args),
            [
                "-flavor",
                "gnu",
                "o.o",
                "-static",
                "-e",
                ".lilac.native_entry",
                "-o",
                "o"
            ]
        );
    }

    /// A shared library makes the executable dynamically linked, naming the
    /// loader; static archives and other flags do not.
    #[test]
    fn elf_links_shared_libraries_dynamically() {
        let link = |extra: &[&str]| {
            let extra: Vec<String> = extra.iter().map(|arg| (*arg).to_owned()).collect();
            let args = native_args(
                &info("aarch64-unknown-linux-gnu"),
                "aarch64-unknown-linux-gnu",
                "aarch64",
                Path::new("sys"),
                Path::new("o.o"),
                Path::new("o"),
                &extra,
            );
            strings(&args)
        };
        for shared in [&["libgreet.so"][..], &["-L.", "-lgreet"], &["libz.so.1"]] {
            let args = link(shared);
            assert!(!args.contains(&"-static".to_owned()), "{args:?}");
            let at = args
                .iter()
                .position(|arg| arg == "-dynamic-linker")
                .expect("names a loader");
            assert_eq!(args[at + 1], "/lib/ld-linux-aarch64.so.1");
        }
        for other in [&["libgreet.a"][..], &["--gc-sections"]] {
            let args = link(other);
            assert!(args.contains(&"-static".to_owned()), "{args:?}");
        }
    }

    #[test]
    fn macho_native_command() {
        let args = native_args(
            &info("aarch64-apple-darwin"),
            "aarch64-apple-darwin",
            "aarch64",
            Path::new("sys"),
            Path::new("o.o"),
            Path::new("o"),
            &[],
        );
        let tbd = Path::new("sys")
            .join("aarch64-apple-darwin")
            .join("libSystem.tbd");
        assert_eq!(
            strings(&args),
            [
                "-flavor",
                "darwin",
                "o.o",
                &tbd.display().to_string(),
                "-arch",
                "arm64",
                "-platform_version",
                "macos",
                "11.0",
                "11.0",
                "-e",
                "_.lilac.native_entry",
                "-o",
                "o",
            ]
        );
    }

    #[test]
    fn libc_command_host_and_cross() {
        let host = libc_args(Path::new("o.obj"), Path::new("o.exe"), None, &[]);
        assert_eq!(strings(&host), ["o.obj", "-o", "o.exe"]);
        let cross = libc_args(
            Path::new("o.o"),
            Path::new("o"),
            Some("x86_64-unknown-linux-gnu"),
            &["-lm".to_owned()],
        );
        assert_eq!(
            strings(&cross),
            ["o.o", "-o", "o", "--target=x86_64-unknown-linux-gnu", "-lm"]
        );
    }

    #[test]
    fn render_quotes_spaces() {
        let line = render(Path::new("lld"), &["a b".into(), "c".into()]);
        assert_eq!(line, "lld \"a b\" c");
    }
}
