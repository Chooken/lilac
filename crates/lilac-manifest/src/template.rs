//! The project templates `lilac init` copies: a folder of files
//! (`templates/project/` in the install folder, holding `lilac.toml` and
//! `src/main.lilac`) read through [`Fs`], with placeholders filled in.
//!
//! The placeholders are `{{name}}` (the program's name), `{{host}}` (this
//! machine's triple) and `{{libraries}}`: the whole line holding it
//! becomes one `Name = { bundled = true }` line per bundled library, or a
//! commented example when there is none.

use std::io;
use std::path::{Path, PathBuf};

use lilac_module::Fs;

/// The environment variable naming the folder of project templates.
pub const TEMPLATE_DIR_VAR: &str = "LILAC_TEMPLATE_DIR";

/// The template `lilac init` copies, a folder of the templates folder.
pub const PROJECT_TEMPLATE: &str = "project";

/// How deep a template's folders may nest (it is read recursively).
const MAX_DEPTH: usize = 16;

/// The folder of project templates: `LILAC_TEMPLATE_DIR` (`env`), else
/// `<exe dir>/../templates`, beside `lib/` in the install folder.
#[must_use]
pub fn template_dir(env: Option<PathBuf>, exe_dir: Option<&Path>) -> Option<PathBuf> {
    env.filter(|dir| !dir.as_os_str().is_empty())
        .or_else(|| exe_dir.map(|dir| dir.join("..").join("templates")))
}

/// Every file under the template folder `dir`, as its path relative to
/// `dir` and its text, sorted by path. An error when `dir` cannot be
/// listed, a file cannot be read, or folders nest past [`MAX_DEPTH`].
pub fn read_template(dir: &Path, fs: &mut dyn Fs) -> io::Result<Vec<(PathBuf, String)>> {
    let mut files = Vec::new();
    walk(dir, Path::new(""), 0, fs, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk(
    dir: &Path,
    relative: &Path,
    depth: usize,
    fs: &mut dyn Fs,
    files: &mut Vec<(PathBuf, String)>,
) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(io::Error::other(format!(
            "`{}` nests more than {MAX_DEPTH} folders deep",
            dir.display()
        )));
    }
    for entry in fs.list_dir(dir)? {
        let Some(name) = entry.file_name() else {
            continue;
        };
        let relative = relative.join(name);
        // A folder lists; anything else is a file.
        if fs.list_dir(&entry).is_ok() {
            walk(&entry, &relative, depth + 1, fs, files)?;
        } else {
            files.push((relative, fs.read(&entry)?));
        }
    }
    Ok(())
}

/// What a template's placeholders become.
#[derive(Clone, Copy, Debug)]
pub struct Fill<'a> {
    /// `{{name}}`: the program's name (a valid `project.name`).
    pub name: &'a str,
    /// `{{host}}`: this machine's triple.
    pub host: &'a str,
    /// `{{libraries}}`: the bundled libraries to mount.
    pub libraries: &'a [String],
}

/// `text` with its placeholders filled in.
#[must_use]
pub fn fill(text: &str, fill: &Fill<'_>) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if line.contains("{{libraries}}") {
            let end = if line.ends_with("\r\n") {
                "\r\n"
            } else if line.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            let mounts: Vec<String> = if fill.libraries.is_empty() {
                vec!["# Standard = { bundled = true }".to_owned()]
            } else {
                fill.libraries
                    .iter()
                    .map(|library| format!("{library} = {{ bundled = true }}"))
                    .collect()
            };
            out.push_str(&mounts.join(if end.is_empty() { "\n" } else { end }));
            out.push_str(end);
        } else {
            out.push_str(
                &line
                    .replace("{{name}}", fill.name)
                    .replace("{{host}}", fill.host),
            );
        }
    }
    out
}
