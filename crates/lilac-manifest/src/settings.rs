//! The settings `lilac.toml` holds: the table of them ([`SETTINGS`]), the
//! reader that checks a manifest against it ([`Manifest::read`], `E076`
//! for a setting that is unknown or has a wrong value), and how the
//! settings meet the flags ([`Settings::resolve`]) and a session
//! ([`Settings::apply`]).
//!
//! Nothing here touches the machine: files come through
//! [`lilac_module::Fs`], and the environment and the compiler's folder are
//! arguments.

use std::path::{Path, PathBuf};

use lilac_driver::{Entry, OptimizeMode, Session};
use lilac_error::{closest, codes, Diagnostic, Diags, ErrorGuaranteed};
use lilac_module::{is_identifier, Fs, MANIFEST_FILE};
use lilac_span::{SourceMap, Span};
use lilac_target::Target;

use crate::toml::{self, Entry as TomlEntry, Table, Value, ValueKind};

/// The environment variable naming the folder of bundled libraries.
pub const LIB_DIR_VAR: &str = "LILAC_LIB_DIR";

/// What a setting's value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A program name: letters, digits, `-`, `_` and `.`.
    Name,
    /// A folder, relative to the manifest.
    Folder,
    /// An optimization mode's name.
    Mode,
    /// A target triple, or `"host"`.
    Target,
    /// `true` or `false`.
    Bool,
    /// A library: a folder, `{ path = "folder" }` or `{ bundled = true }`.
    Library,
    /// A program on `PATH`, or a path to one.
    Program,
    /// An array of strings.
    Strings,
}

/// One setting of [`SETTINGS`].
#[derive(Clone, Copy, Debug)]
pub struct Setting {
    /// Its section, the `[header]` it goes under.
    pub section: &'static str,
    /// Its key; `<Name>` stands for any library name.
    pub key: &'static str,
    /// What its value is.
    pub kind: Kind,
    /// Its default as TOML, or `None` when the default is computed.
    pub default: Option<&'static str>,
    /// The flag that overrides it for one run, if any.
    pub flag: Option<&'static str>,
    /// What it does, in one line.
    pub about: &'static str,
}

impl Setting {
    /// Whether this is the `[libraries]` entry, which takes any name.
    #[must_use]
    pub fn is_library(&self) -> bool {
        self.kind == Kind::Library
    }
}

/// The sections of a manifest, in the order a template writes them.
pub const SECTIONS: &[&str] = &["project", "build", "libraries", "link"];

/// Every setting a manifest takes: what `Manifest::read` checks keys
/// against, and what `lilac init`'s template (`templates/project/`) and
/// the README list.
pub const SETTINGS: &[Setting] = &[
    Setting {
        section: "project",
        key: "name",
        kind: Kind::Name,
        default: None,
        flag: None,
        about: "The program's name: the executable's file name. By default, the folder's name.",
    },
    Setting {
        section: "project",
        key: "root",
        kind: Kind::Folder,
        default: Some("\".\""),
        flag: None,
        about: "The folder holding the project's code, relative to this file.",
    },
    Setting {
        section: "build",
        key: "mode",
        kind: Kind::Mode,
        default: Some("\"debug\""),
        flag: Some("--mode=, --release"),
        about: "\"debug\", \"release\" or \"release-small\".",
    },
    Setting {
        section: "build",
        key: "target",
        kind: Kind::Target,
        default: Some("\"host\""),
        flag: Some("--target="),
        about: "A target triple, or \"host\" for this machine.",
    },
    Setting {
        section: "build",
        key: "stack-traces",
        kind: Kind::Bool,
        default: None,
        flag: Some("--stack-traces="),
        about: "Record panic stack traces. By default, in debug builds only.",
    },
    Setting {
        section: "build",
        key: "dir",
        kind: Kind::Folder,
        default: Some("\"build\""),
        flag: Some("--build-dir="),
        about: "Where builds go, relative to this file.",
    },
    Setting {
        section: "libraries",
        key: "<Name>",
        kind: Kind::Library,
        default: None,
        flag: Some("--lib=Name=dir"),
        about:
            "Name = \"folder\", or Name = { bundled = true } for a library installed with Lilac.",
    },
    Setting {
        section: "link",
        key: "libc",
        kind: Kind::Bool,
        default: Some("false"),
        flag: Some("--libc"),
        about: "Link through a C compiler and the C library.",
    },
    Setting {
        section: "link",
        key: "linker",
        kind: Kind::Program,
        default: None,
        flag: Some("--linker="),
        about:
            "The C compiler that links when libc is on. By default LILAC_LINKER, else cc or clang.",
    },
    Setting {
        section: "link",
        key: "args",
        kind: Kind::Strings,
        default: Some("[]"),
        flag: Some("--link-arg="),
        about: "More linker arguments. Relative paths are relative to this file.",
    },
];

/// The names of the optimization modes, as `build.mode` takes them.
const MODES: &[&str] = &["debug", "release", "release-small"];

/// A setting's value and where the manifest writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spanned<T> {
    /// The value.
    pub value: T,
    /// The value as written.
    pub span: Span,
}

/// `build.target`, or `--target=`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetChoice {
    /// The machine the compiler runs on.
    Host,
    /// A triple.
    Triple(Target),
}

/// One `[libraries]` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Library {
    /// The name it mounts as.
    pub name: String,
    /// The name as written.
    pub name_span: Span,
    /// Its folder: as written (relative to the manifest), or for a
    /// bundled one, the folder installed beside the compiler.
    pub dir: PathBuf,
    /// Whether it is `{ bundled = true }`.
    pub bundled: bool,
    /// The value as written.
    pub span: Span,
}

/// A read `lilac.toml`: every setting it gives that has a right value. A
/// setting that is `E076` is left out, as if it were not written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// The folder holding it, as the caller named it.
    pub dir: PathBuf,
    /// The manifest file.
    pub path: PathBuf,
    /// `project.name`.
    pub name: Option<Spanned<String>>,
    /// `project.root`, as written.
    pub root: Option<Spanned<PathBuf>>,
    /// `build.mode`.
    pub mode: Option<Spanned<OptimizeMode>>,
    /// `build.target`.
    pub target: Option<Spanned<TargetChoice>>,
    /// `build.stack-traces`.
    pub stack_traces: Option<Spanned<bool>>,
    /// `build.dir`, as written.
    pub build_dir: Option<Spanned<PathBuf>>,
    /// `[libraries]`, in the file's order.
    pub libraries: Vec<Library>,
    /// `link.libc`.
    pub libc: Option<Spanned<bool>>,
    /// `link.linker`, as written.
    pub linker: Option<Spanned<PathBuf>>,
    /// `link.args`.
    pub link_args: Option<Spanned<Vec<String>>>,
}

impl Manifest {
    /// An empty manifest in `dir`: every setting at its default.
    #[must_use]
    pub fn empty(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            path: dir.join(MANIFEST_FILE),
            name: None,
            root: None,
            mode: None,
            target: None,
            stack_traces: None,
            build_dir: None,
            libraries: Vec::new(),
            libc: None,
            linker: None,
            link_args: None,
        }
    }

    /// Reads `dir/lilac.toml` through `fs`. `None` when `dir` has none;
    /// an error when it cannot be read or is not TOML Lilac reads
    /// (`E075`). Otherwise every setting is checked (`E076`), and the
    /// manifest holds those that are right. The file is added to `sources`
    /// as `lilac.toml`, so its diagnostics render with snippets.
    /// `bundled` is the folder of bundled libraries ([`bundled_dir`]).
    pub fn read(
        dir: &Path,
        bundled: Option<&Path>,
        fs: &mut dyn Fs,
        sources: &mut SourceMap,
        diags: &mut Diags,
    ) -> Option<Result<Self, ErrorGuaranteed>> {
        let path = dir.join(MANIFEST_FILE);
        let listed = fs.list_dir(dir).ok()?;
        if !listed
            .iter()
            .any(|entry| entry.file_name() == Some(MANIFEST_FILE.as_ref()))
        {
            return None;
        }
        let text = match fs.read(&path) {
            Ok(text) => text,
            Err(err) => {
                return Some(Err(diags.emit(Diagnostic::error(
                    codes::E075,
                    format!("cannot read `{}`: {err}", path.display()),
                ))));
            }
        };
        let file = sources.add_file(MANIFEST_FILE, &text);
        let Some(table) = toml::read(file, &text, diags) else {
            return Some(Err(diags.already_reported()));
        };
        let mut reader = Reader {
            manifest: Self::empty(dir),
            text: &text,
            bundled,
            fs,
            diags,
        };
        reader.manifest.path = path;
        reader.read(&table);
        Some(Ok(reader.manifest))
    }
}

/// Checks a manifest's table into a [`Manifest`].
struct Reader<'a> {
    manifest: Manifest,
    text: &'a str,
    bundled: Option<&'a Path>,
    fs: &'a mut dyn Fs,
    diags: &'a mut Diags,
}

impl Reader<'_> {
    fn read(&mut self, table: &Table) {
        for entry in &table.entries {
            if SECTIONS.contains(&entry.key.as_str()) {
                let ValueKind::Table(section) = &entry.value.kind else {
                    self.wrong_type(&entry.key, "a table of settings", &entry.value);
                    continue;
                };
                if entry.key == "libraries" {
                    for library in &section.entries {
                        self.library(library);
                    }
                } else {
                    for setting in &section.entries {
                        self.setting(&entry.key, setting);
                    }
                }
            } else {
                self.unknown_section(entry);
            }
        }
    }

    /// A top-level key no section has.
    fn unknown_section(&mut self, entry: &TomlEntry) {
        let key = &entry.key;
        let is_table = matches!(entry.value.kind, ValueKind::Table(_));
        let home = SETTINGS
            .iter()
            .find(|setting| !setting.is_library() && setting.key == key);
        let diagnostic = match home {
            Some(setting) if !is_table => Diagnostic::error(
                codes::E076,
                format!("setting `{key}` is outside its section"),
            )
            .label(entry.key_span, true, "")
            .help(format!("write it under `[{}]`", setting.section)),
            _ => {
                let diagnostic = Diagnostic::error(codes::E076, format!("unknown section `{key}`"))
                    .label(entry.key_span, true, "");
                match closest(key, SECTIONS.iter().copied()) {
                    Some(name) => diagnostic.suggest(entry.key_span, name),
                    None => diagnostic.note(format!("the sections are {}", list(SECTIONS))),
                }
            }
        };
        self.diags.emit(diagnostic);
    }

    /// One key of `[section]` (not `[libraries]`).
    fn setting(&mut self, section: &str, entry: &TomlEntry) {
        let Some(setting) = SETTINGS
            .iter()
            .find(|setting| setting.section == section && setting.key == entry.key)
        else {
            self.unknown_setting(section, entry);
            return;
        };
        let name = format!("{section}.{}", setting.key);
        let value = &entry.value;
        let span = value.span;
        match setting.key {
            "name" => {
                let read = self.name(&name, value);
                self.manifest.name = read.map(|value| Spanned { value, span });
            }
            "root" => {
                let read = self.folder(&name, value, true);
                self.manifest.root = read.map(|value| Spanned { value, span });
            }
            "mode" => {
                let read = self.mode(&name, value);
                self.manifest.mode = read.map(|value| Spanned { value, span });
            }
            "target" => {
                let read = self.target(&name, value);
                self.manifest.target = read.map(|value| Spanned { value, span });
            }
            "stack-traces" => {
                let read = self.boolean(&name, value);
                self.manifest.stack_traces = read.map(|value| Spanned { value, span });
            }
            "dir" => {
                let read = self.folder(&name, value, false);
                self.manifest.build_dir = read.map(|value| Spanned { value, span });
            }
            "libc" => {
                let read = self.boolean(&name, value);
                self.manifest.libc = read.map(|value| Spanned { value, span });
            }
            "linker" => {
                let read = self.program(&name, value);
                self.manifest.linker = read.map(|value| Spanned { value, span });
            }
            "args" => {
                let read = self.strings(&name, value);
                self.manifest.link_args = read.map(|value| Spanned { value, span });
            }
            _ => unreachable!("every setting is read: {name}"),
        }
    }

    /// A key `[section]` does not take.
    fn unknown_setting(&mut self, section: &str, entry: &TomlEntry) {
        let key = &entry.key;
        let keys = SETTINGS
            .iter()
            .filter(|setting| setting.section == section)
            .map(|setting| setting.key);
        let mut diagnostic = Diagnostic::error(
            codes::E076,
            format!("unknown setting `{key}` in `[{section}]`"),
        )
        .label(entry.key_span, true, "");
        if let Some(name) = closest(key, keys) {
            diagnostic = diagnostic.suggest(entry.key_span, name);
        } else if let Some(home) = SETTINGS
            .iter()
            .find(|setting| !setting.is_library() && setting.key == key)
        {
            diagnostic = diagnostic.help(format!("`{key}` goes under `[{}]`", home.section));
        }
        self.diags.emit(diagnostic);
    }

    /// One `[libraries]` entry.
    fn library(&mut self, entry: &TomlEntry) {
        let name = &entry.key;
        let setting = format!("libraries.{name}");
        if name.starts_with('@') {
            self.diags.emit(
                Diagnostic::error(
                    codes::E076,
                    format!("library name `{name}` is reserved for the compiler"),
                )
                .label(entry.key_span, true, "")
                .note("`@` names belong to the compiler (`@builtin`)"),
            );
            return;
        }
        if !is_identifier(name) {
            self.diags.emit(
                Diagnostic::error(
                    codes::E076,
                    format!("library name `{name}` is not an identifier"),
                )
                .label(entry.key_span, true, "")
                .note("a library mounts as a namespace, so its name is letters, digits and `_`, not starting with a digit"),
            );
            return;
        }
        let (dir, bundled) = match &entry.value.kind {
            ValueKind::String(_) => {
                let Some(dir) = self.folder(&setting, &entry.value, false) else {
                    return;
                };
                (dir, false)
            }
            ValueKind::Table(table) => {
                let Some(read) = self.library_table(name, table, entry.value.span) else {
                    return;
                };
                read
            }
            _ => {
                self.wrong_type(&setting, "a folder or a table", &entry.value);
                return;
            }
        };
        let found = if bundled {
            self.fs.list_dir(&dir).is_ok()
        } else {
            self.fs.list_dir(&self.manifest.dir.join(&dir)).is_ok()
        };
        if !found {
            let message = if bundled {
                format!("no bundled library `{name}`")
            } else {
                format!("cannot find library folder `{}`", dir.display())
            };
            self.diags.emit(
                Diagnostic::error(codes::E076, message)
                    .label(entry.value.span, true, "")
                    .note(format!("looked for `{}`", dir.display())),
            );
            return;
        }
        self.manifest.libraries.push(Library {
            name: name.clone(),
            name_span: entry.key_span,
            dir,
            bundled,
            span: entry.value.span,
        });
    }

    /// `{ path = "folder" }` or `{ bundled = true }`: the folder and
    /// whether it is bundled.
    fn library_table(&mut self, name: &str, table: &Table, span: Span) -> Option<(PathBuf, bool)> {
        let mut path = None;
        let mut bundled = None;
        let mut failed = false;
        for entry in &table.entries {
            let setting = format!("libraries.{name}.{}", entry.key);
            match entry.key.as_str() {
                "path" => match self.folder(&setting, &entry.value, false) {
                    Some(dir) => path = Some(dir),
                    None => failed = true,
                },
                "bundled" => match self.boolean(&setting, &entry.value) {
                    Some(value) => bundled = Some((value, entry.value.span)),
                    None => failed = true,
                },
                key => {
                    let mut diagnostic = Diagnostic::error(
                        codes::E076,
                        format!("unknown key `{key}` in library `{name}`"),
                    )
                    .label(entry.key_span, true, "");
                    if let Some(near) = closest(key, ["path", "bundled"]) {
                        diagnostic = diagnostic.suggest(entry.key_span, near);
                    } else {
                        diagnostic = diagnostic.note("a library takes `path` or `bundled`");
                    }
                    self.diags.emit(diagnostic);
                    failed = true;
                }
            }
        }
        if failed {
            return None;
        }
        match (path, bundled) {
            (Some(_), Some(_)) => {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E076,
                        format!("library `{name}` has both `path` and `bundled`"),
                    )
                    .label(span, true, "")
                    .help("give `path` for a folder, or `bundled = true` for a library installed with Lilac"),
                );
                None
            }
            (Some(dir), None) => Some((dir, false)),
            (None, Some((false, at))) => {
                self.diags.emit(
                    Diagnostic::error(codes::E076, "`bundled = false` mounts nothing")
                        .label(at, true, "")
                        .help(format!(
                            "give the folder as `path`, or remove library `{name}`"
                        )),
                );
                None
            }
            (None, Some((true, _))) => {
                let Some(folder) = self.bundled else {
                    self.diags.emit(
                        Diagnostic::error(codes::E076, "cannot find Lilac's bundled libraries")
                            .label(span, true, "")
                            .help(format!("set {LIB_DIR_VAR} to the folder that holds them")),
                    );
                    return None;
                };
                Some((folder.join(name), true))
            }
            (None, None) => {
                self.diags.emit(
                    Diagnostic::error(
                        codes::E076,
                        format!("library `{name}` needs `path` or `bundled = true`"),
                    )
                    .label(span, true, ""),
                );
                None
            }
        }
    }

    /// `project.name`.
    fn name(&mut self, setting: &str, value: &Value) -> Option<String> {
        let name = self.string(setting, value)?;
        if is_program_name(&name) {
            return Some(name);
        }
        self.diags.emit(
            Diagnostic::error(codes::E076, format!("invalid project name `{name}`"))
                .label(value.span, true, "")
                .note("a name takes letters, digits, `-`, `_` and `.`, and starts with a letter, a digit or `_`"),
        );
        None
    }

    /// A folder; with `must_exist`, one the manifest's folder holds.
    fn folder(&mut self, setting: &str, value: &Value, must_exist: bool) -> Option<PathBuf> {
        let folder = self.string(setting, value)?;
        if folder.is_empty() {
            self.empty(setting, "a folder", value);
            return None;
        }
        let folder = PathBuf::from(folder);
        if must_exist && self.fs.list_dir(&self.manifest.dir.join(&folder)).is_err() {
            self.diags.emit(
                Diagnostic::error(
                    codes::E076,
                    format!("cannot find folder `{}`", folder.display()),
                )
                .label(value.span, true, "")
                .note(format!(
                    "`{setting}` is relative to the folder holding `{MANIFEST_FILE}`"
                )),
            );
            return None;
        }
        Some(folder)
    }

    /// `link.linker`.
    fn program(&mut self, setting: &str, value: &Value) -> Option<PathBuf> {
        let program = self.string(setting, value)?;
        if program.is_empty() {
            self.empty(setting, "a program", value);
            return None;
        }
        Some(PathBuf::from(program))
    }

    /// `build.mode`.
    fn mode(&mut self, setting: &str, value: &Value) -> Option<OptimizeMode> {
        let mode = self.string(setting, value)?;
        if let Some(parsed) = OptimizeMode::parse(&mode) {
            return Some(parsed);
        }
        let mut diagnostic = Diagnostic::error(codes::E076, format!("unknown mode `{mode}`"))
            .label(value.span, true, "");
        match (
            closest(&mode, MODES.iter().copied()),
            self.inner(value, &mode),
        ) {
            (Some(near), Some(inner)) => diagnostic = diagnostic.suggest(inner, near),
            _ => diagnostic = diagnostic.note(format!("the modes are {}", list(MODES))),
        }
        self.diags.emit(diagnostic);
        None
    }

    /// `build.target`.
    fn target(&mut self, setting: &str, value: &Value) -> Option<TargetChoice> {
        let triple = self.string(setting, value)?;
        if triple == "host" {
            return Some(TargetChoice::Host);
        }
        match Target::parse(&triple) {
            Ok(target) => Some(TargetChoice::Triple(target)),
            Err(err) => {
                self.diags.emit(
                    Diagnostic::error(codes::E076, format!("invalid target `{triple}`: {err}"))
                        .label(value.span, true, "")
                        .help("give a triple such as `x86_64-unknown-linux-gnu`, or \"host\""),
                );
                None
            }
        }
    }

    /// An array of strings.
    fn strings(&mut self, setting: &str, value: &Value) -> Option<Vec<String>> {
        let ValueKind::Array(items) = &value.kind else {
            self.wrong_type(setting, "an array of strings", value);
            return None;
        };
        let mut strings = Vec::with_capacity(items.len());
        for item in items {
            let ValueKind::String(text) = &item.kind else {
                self.wrong_type(setting, "an array of strings", item);
                return None;
            };
            strings.push(text.clone());
        }
        Some(strings)
    }

    fn string(&mut self, setting: &str, value: &Value) -> Option<String> {
        if let ValueKind::String(text) = &value.kind {
            Some(text.clone())
        } else {
            self.wrong_type(setting, "a string", value);
            None
        }
    }

    fn boolean(&mut self, setting: &str, value: &Value) -> Option<bool> {
        if let ValueKind::Bool(flag) = value.kind {
            Some(flag)
        } else {
            self.wrong_type(setting, "`true` or `false`", value);
            None
        }
    }

    /// `setting` takes `want`, and `value` is something else.
    fn wrong_type(&mut self, setting: &str, want: &str, value: &Value) {
        let found = match value.kind {
            ValueKind::String(_) => "a string",
            ValueKind::Bool(_) => "a boolean",
            ValueKind::Array(_) => "an array",
            ValueKind::Table(_) => "a table",
        };
        self.diags.emit(
            Diagnostic::error(codes::E076, format!("`{setting}` takes {want}")).label(
                value.span,
                true,
                format!("this is {found}"),
            ),
        );
    }

    /// `setting` takes `want`, and `value` is `""`.
    fn empty(&mut self, setting: &str, want: &str, value: &Value) {
        self.diags.emit(
            Diagnostic::error(codes::E076, format!("`{setting}` takes {want}, not \"\""))
                .label(value.span, true, ""),
        );
    }

    /// The span of a string value's text inside its quotes, when it is
    /// written without escapes (so a fix can replace just the text).
    fn inner(&self, value: &Value, text: &str) -> Option<Span> {
        let (lo, hi) = (value.span.lo.0, value.span.hi.0);
        let written = self
            .text
            .get(lo as usize + 1..(hi as usize).checked_sub(1)?)?;
        (written == text).then(|| Span::new(lo + 1, hi - 1, value.span.file))
    }
}

/// `a`, `b` and `c`.
fn list(names: &[&str]) -> String {
    let quoted: Vec<String> = names.iter().map(|name| format!("`{name}`")).collect();
    match quoted.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => quoted.concat(),
    }
}

/// Whether `name` can be `project.name`: letters, digits, `-`, `_` and
/// `.`, starting with a letter, a digit or `_`.
#[must_use]
pub fn is_program_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The default `project.name` for a folder named `folder`: its name, with
/// every character a name does not take turned into `-` (and a leading
/// `-` or `.` into `_`).
#[must_use]
pub fn default_name(folder: &str) -> String {
    let mut name: String = folder
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if name.starts_with(['-', '.']) {
        name.replace_range(..1, "_");
    }
    if name.is_empty() {
        name.push_str("main");
    }
    name
}

/// The folder of bundled libraries: `LILAC_LIB_DIR` (`env`), else
/// `<exe dir>/../lib`. `lilac` and `lilac-lsp` sit in one `bin/`, so both
/// find the same folder.
#[must_use]
pub fn bundled_dir(env: Option<PathBuf>, exe_dir: Option<&Path>) -> Option<PathBuf> {
    env.filter(|dir| !dir.as_os_str().is_empty())
        .or_else(|| exe_dir.map(|dir| dir.join("..").join("lib")))
}

/// The libraries in the bundled folder `dir`: its identifier-named
/// folders, except the sysroot `lilac`, sorted.
pub fn bundled_libraries(dir: &Path, fs: &mut dyn Fs) -> Vec<String> {
    let Ok(entries) = fs.list_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|entry| entry.file_name()?.to_str().map(str::to_owned))
        .filter(|name| is_identifier(name) && name != "lilac")
        .filter(|name| fs.list_dir(&dir.join(name)).is_ok())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The flags of one run, each overriding its setting ([`SETTINGS`]), and
/// the environment's `LILAC_LINKER`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `--mode=`, `--release`.
    pub mode: Option<OptimizeMode>,
    /// `--target=`.
    pub target: Option<TargetChoice>,
    /// `--stack-traces=`.
    pub stack_traces: Option<bool>,
    /// `--build-dir=`.
    pub build_dir: Option<PathBuf>,
    /// `--lib=Name=dir`, in order: each replaces the manifest's `Name`.
    pub libraries: Vec<(String, PathBuf)>,
    /// `--libc`.
    pub libc: Option<bool>,
    /// `--linker=`.
    pub linker: Option<PathBuf>,
    /// `--link-arg=`, in order, after the manifest's.
    pub link_args: Vec<String>,
    /// `LILAC_LINKER`: after `--linker=`, before `link.linker`.
    pub env_linker: Option<PathBuf>,
}

/// The settings of one run: the flags over the manifest, the manifest
/// over the defaults. A setting that is `None` was given by neither, and
/// the command keeps its own default (what it does without a manifest).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    /// The manifest's folder, when there is a manifest.
    pub manifest_dir: Option<PathBuf>,
    /// The program's name.
    pub name: Option<String>,
    /// The project's root folder.
    pub root: Option<PathBuf>,
    /// The optimization mode.
    pub mode: OptimizeMode,
    /// The target.
    pub target: Option<Target>,
    /// Stack traces: `--stack-traces=` over `build.stack-traces`. `build`
    /// defaults it from the mode; `check` and `emit-*` take only the flag.
    pub stack_traces: Option<bool>,
    /// The build folder.
    pub build_dir: Option<PathBuf>,
    /// The mounted libraries.
    pub libraries: Vec<(String, PathBuf)>,
    /// Whether to link through the C library.
    pub libc: Option<bool>,
    /// The `--libc` linker: `--linker=`, `LILAC_LINKER`, then
    /// `link.linker`; `None` leaves `cc` or `clang` on `PATH`.
    pub linker: Option<PathBuf>,
    /// The linker arguments: the manifest's, then the flags'.
    pub link_args: Vec<String>,
}

impl Settings {
    /// Applies `overrides` over `manifest` (when there is one) over the
    /// defaults. A manifest's paths are joined to its folder, so they are
    /// absolute when [`Manifest::dir`] is; `host` is what `"host"` means.
    #[must_use]
    pub fn resolve(manifest: Option<&Manifest>, overrides: &Overrides, host: Target) -> Self {
        let choose = |choice: TargetChoice| match choice {
            TargetChoice::Host => host,
            TargetChoice::Triple(target) => target,
        };
        let mut settings = Self {
            mode: overrides.mode.unwrap_or_default(),
            target: overrides.target.map(choose),
            stack_traces: overrides.stack_traces,
            build_dir: overrides.build_dir.clone(),
            libc: overrides.libc,
            linker: overrides
                .linker
                .clone()
                .or_else(|| overrides.env_linker.clone()),
            ..Self::default()
        };
        if let Some(manifest) = manifest {
            let dir = &manifest.dir;
            settings.manifest_dir = Some(dir.clone());
            settings.name = spanned_value(manifest.name.as_ref()).or_else(|| {
                dir.file_name()
                    .map(|name| default_name(&name.to_string_lossy()))
            });
            settings.root = Some(
                manifest
                    .root
                    .as_ref()
                    .map_or_else(|| dir.clone(), |root| dir.join(&root.value)),
            );
            if overrides.mode.is_none() {
                settings.mode = spanned_value(manifest.mode.as_ref()).unwrap_or_default();
            }
            if settings.target.is_none() {
                let choice = spanned_value(manifest.target.as_ref()).unwrap_or(TargetChoice::Host);
                settings.target = Some(choose(choice));
            }
            settings.stack_traces = settings
                .stack_traces
                .or_else(|| spanned_value(manifest.stack_traces.as_ref()));
            if settings.build_dir.is_none() {
                let build = manifest
                    .build_dir
                    .as_ref()
                    .map_or_else(|| PathBuf::from("build"), |dir| dir.value.clone());
                settings.build_dir = Some(dir.join(build));
            }
            settings.libraries = manifest
                .libraries
                .iter()
                .map(|library| (library.name.clone(), dir.join(&library.dir)))
                .collect();
            settings.libc = settings
                .libc
                .or_else(|| Some(spanned_value(manifest.libc.as_ref()).unwrap_or(false)));
            if settings.linker.is_none() {
                settings.linker = manifest.linker.as_ref().map(|linker| {
                    if is_bare_program(&linker.value) {
                        linker.value.clone()
                    } else {
                        dir.join(&linker.value)
                    }
                });
            }
            settings.link_args = spanned_value(manifest.link_args.as_ref()).unwrap_or_default();
        }
        for (name, dir) in &overrides.libraries {
            match settings
                .libraries
                .iter_mut()
                .find(|(mounted, _)| mounted == name)
            {
                Some(library) => library.1.clone_from(dir),
                None => settings.libraries.push((name.clone(), dir.clone())),
            }
        }
        settings
            .link_args
            .extend(overrides.link_args.iter().cloned());
        settings
    }

    /// Sets what a compile reads from the settings on `session`: the
    /// libraries, the target, the mode, the entry (from `libc`) and, with a
    /// manifest, the build folder the loader skips. A setting given by
    /// neither the flags nor a manifest leaves the session's own; without a
    /// manifest the loader skips `<root>/build`, whatever `--build-dir`
    /// says, as it always has.
    pub fn apply(&self, session: &mut Session) {
        session.libraries.clone_from(&self.libraries);
        if let Some(target) = self.target {
            session.target = target;
        }
        session.optimize = self.mode;
        if let Some(libc) = self.libc {
            session.entry = if libc { Entry::CMain } else { Entry::Native };
        }
        session.build_dir = if self.manifest_dir.is_some() {
            self.build_dir.clone()
        } else {
            None
        };
    }
}

/// Whether `program` names a program on `PATH` (no folder in it) rather
/// than a path.
fn is_bare_program(program: &Path) -> bool {
    program.components().count() == 1 && !program.has_root()
}

/// The value of a setting the manifest gives.
fn spanned_value<T: Clone>(spanned: Option<&Spanned<T>>) -> Option<T> {
    spanned.map(|spanned| spanned.value.clone())
}
