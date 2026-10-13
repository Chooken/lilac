//! `lilac init [<dir>]`: copies the project template (`templates/project/`
//! in the install folder: a `lilac.toml` and a hello-world
//! `src/main.lilac`) into a folder, filling in the program's name, this
//! machine and the bundled libraries ([`lilac_manifest::fill`]).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lilac_driver::Target;
use lilac_manifest::{Fill, MANIFEST_FILE, PROJECT_TEMPLATE, TEMPLATE_DIR_VAR};

use crate::progress::{auto_color, Reporter, Verbosity};
use crate::std_fs::StdFs;
use crate::{exe_dir, parse_progress_flag, unknown_flag, HELP_HINT};

/// `lilac init [<dir>] [-q]`: creates `<dir>` (`.` by default) if it is
/// missing and copies the project template into it. An existing
/// `lilac.toml` stops it before anything is written; any other file
/// already there is kept.
pub fn cmd_init(args: &[String]) -> ExitCode {
    let (dir, progress) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let reporter = Reporter::new(progress.unwrap_or(Verbosity::Normal), auto_color());
    let quiet = progress == Some(Verbosity::Quiet);
    // How a path in the new project is shown: relative to where `init` ran.
    let shown = |path: &Path| {
        if dir == Path::new(".") {
            path.display().to_string()
        } else {
            dir.join(path).display().to_string()
        }
    };

    let files = match project_template() {
        Ok(files) => files,
        Err(message) => {
            reporter.error(&message);
            return ExitCode::from(1);
        }
    };
    if dir.join(MANIFEST_FILE).exists() {
        reporter.error(&format!(
            "`{}` already exists; `lilac init` never changes one",
            shown(Path::new(MANIFEST_FILE))
        ));
        return ExitCode::from(1);
    }
    if let Err(error) = std::fs::create_dir_all(&dir) {
        reporter.error(&format!("cannot create `{}`: {error}", dir.display()));
        return ExitCode::from(1);
    }
    let name = project_name(&dir);
    let bundled = lilac_manifest::bundled_dir(
        std::env::var_os(lilac_manifest::LIB_DIR_VAR).map(PathBuf::from),
        exe_dir().as_deref(),
    );
    let libraries = bundled
        .as_deref()
        .map(|bundled| lilac_manifest::bundled_libraries(bundled, &mut StdFs))
        .unwrap_or_default();
    let fill = Fill {
        name: &name,
        host: &Target::host().to_triple(),
        libraries: &libraries,
    };

    // The manifest goes first: `create_new` refuses one written since we
    // looked, before any other file is touched.
    let mut ordered: Vec<&(PathBuf, String)> = files.iter().collect();
    ordered.sort_by_key(|(path, _)| path != Path::new(MANIFEST_FILE));
    let mut created = Vec::new();
    let mut kept = Vec::new();
    for (path, text) in ordered {
        match create(&dir.join(path), &lilac_manifest::fill(text, &fill)) {
            Ok(()) => created.push(path.display().to_string()),
            Err(error)
                if error.kind() == std::io::ErrorKind::AlreadyExists
                    && path != Path::new(MANIFEST_FILE) =>
            {
                kept.push(shown(path));
            }
            Err(error) => {
                reporter.error(&format!("cannot write `{}`: {error}", shown(path)));
                return ExitCode::from(1);
            }
        }
    }

    let place = if dir == Path::new(".") {
        String::new()
    } else {
        format!(" in {}", dir.display())
    };
    reporter.status(
        "Created",
        &format!("`{name}`{place} ({})", created.join(", ")),
    );
    if !quiet {
        for path in kept {
            eprintln!("note: kept the existing `{path}`");
        }
        if libraries.is_empty() {
            let looked = bundled.map_or_else(
                || {
                    format!(
                        "set {} to a folder of libraries",
                        lilac_manifest::LIB_DIR_VAR
                    )
                },
                |bundled| format!("looked in `{}`", bundled.display()),
            );
            eprintln!("note: found no bundled library to mount ({looked})");
        }
        if dir.join("lilac.json").is_file() {
            eprintln!(
                "note: the editor reads {MANIFEST_FILE} now: move the `root` and `libraries` \
                 of lilac.json into it"
            );
        }
    }
    ExitCode::SUCCESS
}

/// `[<dir>] [-q]`: the folder (`.` by default) and `-q`/`--timings`.
fn parse_args(args: &[String]) -> Result<(PathBuf, Option<Verbosity>), ExitCode> {
    let mut dir = None;
    let mut progress = None;
    for arg in args {
        if parse_progress_flag(arg, &mut progress) {
            continue;
        }
        if arg.starts_with('-') {
            eprintln!("{}", unknown_flag(arg));
            return Err(ExitCode::from(2));
        }
        if dir.replace(PathBuf::from(arg)).is_some() {
            eprintln!("`lilac init` takes one folder\n{HELP_HINT}");
            return Err(ExitCode::from(2));
        }
    }
    Ok((dir.unwrap_or_else(|| PathBuf::from(".")), progress))
}

/// The program's name for the folder `dir`: its own name (`.` resolved),
/// made valid.
fn project_name(dir: &Path) -> String {
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let folder = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    lilac_manifest::default_name(
        &folder
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
    )
}

/// The project template's files (relative path, text), which must
/// include a `lilac.toml`; else the error to print.
fn project_template() -> Result<Vec<(PathBuf, String)>, String> {
    let template = lilac_manifest::template_dir(
        std::env::var_os(TEMPLATE_DIR_VAR).map(PathBuf::from),
        exe_dir().as_deref(),
    )
    .map(|templates| templates.join(PROJECT_TEMPLATE))
    .ok_or_else(|| {
        format!(
            "cannot find the project template; set {TEMPLATE_DIR_VAR} to a folder \
             holding `{PROJECT_TEMPLATE}/`"
        )
    })?;
    let files = lilac_manifest::read_template(&template, &mut StdFs).map_err(|error| {
        format!(
            "cannot read the project template `{}`: {error}\n\
             help: set {TEMPLATE_DIR_VAR} to a folder holding `{PROJECT_TEMPLATE}/`",
            template.display()
        )
    })?;
    if !files
        .iter()
        .any(|(path, _)| path == Path::new(MANIFEST_FILE))
    {
        return Err(format!(
            "the project template `{}` has no {MANIFEST_FILE}",
            template.display()
        ));
    }
    Ok(files)
}

/// Writes `text` to a new file at `path`, creating its folder; an
/// `AlreadyExists` error when there is one.
fn create(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(text.as_bytes())
}
