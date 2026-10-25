//! Folder loader: root discovery plus a `using` fixpoint over an injected
//! [`Fs`].
//!
//! A directory is the root namespace; the `*.lilac` files directly inside it
//! are read and parsed only far enough to collect `using` directives. Each
//! `using A.B` resolves root-relative to `<root>/A/B`, and `using *.A.B`
//! resolves relative to the importing file's own namespace; newly-reached folders
//! are read and scanned, repeating until no new folder appears. Every folder is
//! visited once, so cycles terminate: a namespace path is reached once, and
//! a folder once by its canonical path ([`Fs::canonicalize`]), so a second
//! path to a loaded folder (a link back up the tree) is `E025` and is not
//! followed. A `using` inside a `private { ... }` block is honored (it still
//! loads the folder).
//!
//! Projects and mounted libraries load whole: after the fixpoint, every
//! folder below the root and each library whose name is an identifier is
//! reached too, so code is checked (and an editor sees it in the project)
//! even where nothing `using`s it. `using` itself is visibility only. Below
//! the root, the build folder (`build`, unless the load names another), a
//! folder holding its own
//! `lilac.toml` (another project) and a library's mount name are skipped,
//! and in any tree the folder of a library mounted inside it (a vendored
//! library loads once, under its mount name); a root with no `*.lilac` file of its own (a repository holding
//! several projects) loads only what its `using`s reach.
//!
//! `E025` is the module-load error: a `using` whose target is missing or is not
//! a directory, an unreadable root or library folder, an unreadable file, or a
//! folder reached again by a second path (through a link, or a `using`
//! naming a vendored library by its path in the project). Each is reported at the most
//! specific span available (the `using` span where there is one, a dummy
//! span otherwise, with the path named in the headline) and fails the load.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use lilac_ast::{Item, ItemKind};
use lilac_error::{codes, Diagnostic, Diags, ErrorGuaranteed};
use lilac_lex::Lexer;
use lilac_parse::Parser;
use lilac_span::{FileId, SourceMap, Span};

/// The project manifest's file name (`docs/manifest-plan.md`).
pub const MANIFEST_FILE: &str = "lilac.toml";

/// Injected filesystem: the loader never touches `std::fs` directly.
pub trait Fs {
    /// Reads a whole file to a string.
    fn read(&mut self, path: &Path) -> io::Result<String>;
    /// Lists a directory's entries (files and subdirectories).
    fn list_dir(&mut self, dir: &Path) -> io::Result<Vec<PathBuf>>;
    /// The folder or file `path` names, links resolved: two paths to one
    /// folder canonicalize equal, so the loader loads it once.
    fn canonicalize(&mut self, path: &Path) -> io::Result<PathBuf>;
}

/// One loaded source file.
#[derive(Clone, Debug)]
pub struct FileUnit {
    /// File name (no directory).
    pub name: String,
    /// Path the loader read this file from.
    pub path: PathBuf,
    /// Identity of this file in the caller's [`SourceMap`].
    pub file: FileId,
    /// Owning namespace (index into [`Project::namespaces`]).
    pub ns: usize,
    /// Raw source text.
    pub src: String,
    /// The file's `using` directives, in source order.
    pub usings: Vec<Using>,
}

/// One `using` directive.
#[derive(Clone, Debug)]
pub struct Using {
    /// Dotted path from the root (a relative `using *.X` is stored already
    /// joined to the file's namespace path).
    pub path: String,
    /// The directive's span: the `E029` ambiguity diagnostic labels these
    /// import sites.
    pub span: Span,
    /// `using A.B as Name`: the name and its span. An aliased `using` binds
    /// only the alias; it imports none of the namespace's names.
    pub alias: Option<(String, Span)>,
    /// The span of each segment the directive writes: the tail of `path`
    /// (a relative `using` writes no namespace path before it).
    pub segments: Vec<Span>,
}

/// One namespace: a folder and the files directly inside it.
#[derive(Clone, Debug)]
pub struct Namespace {
    /// Dotted path from the root (`""` for the root namespace).
    pub path: String,
    /// Folder this namespace lives in.
    pub dir: PathBuf,
    /// Indices into [`Project::files`].
    pub files: Vec<usize>,
}

/// A whole loaded project: root folder, namespace tree, and files.
#[derive(Clone, Debug)]
pub struct Project {
    /// Root folder of the compilation unit.
    pub root: PathBuf,
    /// All namespaces; index 0 is the root.
    pub namespaces: Vec<Namespace>,
    /// All source files, in load order.
    pub files: Vec<FileUnit>,
}

/// Loads the folder tree rooted at `root`, emitting `E025` for bad `using`s.
///
/// The root namespace is index 0 (path `""`). Namespaces are appended in the
/// order their folders are first reached, and each namespace's files are kept
/// sorted by file name. Every source is registered in `sources` as it is read,
/// and its returned [`FileId`] is stored on [`FileUnit::file`]; `E025` spans
/// (and later the loader's `using` spans) therefore resolve against the
/// caller's [`SourceMap`] without relying on `FileId(index)` coincidence.
pub fn load(
    root: &Path,
    fs: &mut dyn Fs,
    sources: &mut SourceMap,
    diags: &mut Diags,
) -> Result<Project, ErrorGuaranteed> {
    let (project, failed) = load_with_status(root, &[], None, fs, sources, diags);
    if failed {
        Err(diags.already_reported())
    } else {
        Ok(project)
    }
}

/// Like [`load`], but returns the (possibly partial) project together with a
/// `failed` flag instead of an error, so callers can still map the files read
/// before a bad `using`. `libraries` mounts each `(name, dir)` as the
/// top-level namespace `name` (`using name.X` loads `dir/X`). The build
/// folder `build_dir` (`<root>/build` when `None`) is not loaded when it
/// is in the project's tree.
pub fn load_with_status(
    root: &Path,
    libraries: &[(String, PathBuf)],
    build_dir: Option<&Path>,
    fs: &mut dyn Fs,
    sources: &mut SourceMap,
    diags: &mut Diags,
) -> (Project, bool) {
    let Ok(root_entries) = fs.list_dir(root) else {
        diags.emit(Diagnostic::error(
            codes::E025,
            format!("cannot read module folder `{}`", root.display()),
        ));
        return (
            Project {
                root: root.to_path_buf(),
                namespaces: Vec::new(),
                files: Vec::new(),
            },
            true,
        );
    };
    load_from(
        root,
        root_entries,
        None,
        None,
        Walk::Whole { build_dir },
        libraries,
        fs,
        sources,
        diags,
    )
}

/// Loads one source file as the root namespace's only file, plus everything
/// its `using`s reach (mounted libraries, or folders next to the file). The
/// file's siblings are not part of it; `name` is the path it was read from
/// and its name in diagnostics.
pub fn load_file(
    name: &str,
    src: &str,
    libraries: &[(String, PathBuf)],
    fs: &mut dyn Fs,
    sources: &mut SourceMap,
    diags: &mut Diags,
) -> (Project, bool) {
    let path = PathBuf::from(name);
    let root = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let file = RootFile {
        name,
        src,
        path: &path,
    };
    load_from(
        &root,
        Vec::new(),
        Some(file),
        None,
        Walk::Usings,
        libraries,
        fs,
        sources,
        diags,
    )
}

/// Which folders a load reaches besides its `using`s.
#[derive(Clone, Copy)]
enum Walk<'a> {
    /// None: a root file or a lone namespace.
    Usings,
    /// Every project folder but the build folder (`None`: `<root>/build`).
    Whole { build_dir: Option<&'a Path> },
}

/// A root file read before loading ([`load_file`]).
struct RootFile<'a> {
    name: &'a str,
    src: &'a str,
    path: &'a Path,
}

/// Loads one namespace (`Standard.Heap`) and everything it `using`s, with an
/// empty root namespace: how an editor checks a library folder that no
/// project uses. `root` still anchors absolute non-library `using`s.
pub fn load_namespace(
    root: &Path,
    namespace: &str,
    libraries: &[(String, PathBuf)],
    fs: &mut dyn Fs,
    sources: &mut SourceMap,
    diags: &mut Diags,
) -> (Project, bool) {
    load_from(
        root,
        Vec::new(),
        None,
        Some(namespace),
        Walk::Usings,
        libraries,
        fs,
        sources,
        diags,
    )
}

/// The `using` fixpoint, starting from the root namespace's `root_entries`
/// (or the already-read `root_file`) and, when given, the namespace `seed`.
/// With [`Walk::Whole`], every project folder but the build folder is
/// reached too (see the module docs). Reaching a namespace also reaches
/// its ancestors, so ancestor reach never depends on what else was
/// imported.
#[allow(clippy::too_many_arguments)]
fn load_from(
    root: &Path,
    root_entries: Vec<PathBuf>,
    root_file: Option<RootFile<'_>>,
    seed: Option<&str>,
    walk: Walk<'_>,
    libraries: &[(String, PathBuf)],
    fs: &mut dyn Fs,
    sources: &mut SourceMap,
    diags: &mut Diags,
) -> (Project, bool) {
    let (whole_root, build_folder) = match walk {
        Walk::Usings => (false, None),
        Walk::Whole { build_dir } => (
            root_entries.iter().any(|entry| is_lilac_file(entry)),
            build_dir.map(|dir| canonical_folder(fs, dir)),
        ),
    };
    // The root namespace holds the root folder only when it is loaded as a
    // folder: a root file (`load_file`) or a lone namespace (`load_namespace`)
    // leaves it empty, so a library mounted there is no second path to it.
    let root_folder = canonical_folder(fs, root);
    let mut folders = HashMap::new();
    if root_file.is_none() && seed.is_none() {
        folders.insert(root_folder.clone(), 0);
    }
    let library_folders = libraries
        .iter()
        .map(|(_, dir)| canonical_folder(fs, dir))
        .collect();
    let mut loader = Loader {
        root,
        libraries,
        root_folder,
        library_folders,
        build_folder,
        namespaces: vec![Namespace {
            path: String::new(),
            dir: root.to_path_buf(),
            files: Vec::new(),
        }],
        listings: vec![root_entries],
        ns_by_path: HashMap::from([(String::new(), 0)]),
        folders,
        queue: VecDeque::from([0]),
        again: HashSet::new(),
    };
    let mut files: Vec<FileUnit> = Vec::new();
    let mut failed = false;
    if let Some(seed) = seed {
        if loader.reach(seed, None, fs, diags) == Reach::Missing {
            diags.emit(Diagnostic::error(
                codes::E025,
                format!("cannot find module folder `{seed}`"),
            ));
            failed = true;
        }
    }

    if let Some(file) = root_file {
        let name = file.name.to_owned();
        failed |= loader.add_file(
            &mut files,
            0,
            name,
            file.path,
            file.src.to_owned(),
            fs,
            sources,
            diags,
        );
    }

    failed |= loader.drain(&mut files, fs, sources, diags);
    if whole_root {
        loader.reach_children("", fs, diags);
    }
    for (name, dir) in libraries {
        if loader.reach_tree(name, fs, diags) == Reach::Missing {
            diags.emit(Diagnostic::error(
                codes::E025,
                format!("cannot read library folder `{}`", dir.display()),
            ));
            failed = true;
        }
    }
    failed |= loader.drain(&mut files, fs, sources, diags);
    failed |= !loader.again.is_empty();
    (loader.project(files), failed)
}

/// Namespace bookkeeping for [`load_from`].
struct Loader<'a> {
    root: &'a Path,
    libraries: &'a [(String, PathBuf)],
    /// The root folder, canonical ([`Fs::canonicalize`]).
    root_folder: PathBuf,
    /// Each library's folder, canonical, in `libraries` order.
    library_folders: Vec<PathBuf>,
    /// The build folder, canonical, when the load names one (else it is
    /// `build` under the root).
    build_folder: Option<PathBuf>,
    namespaces: Vec<Namespace>,
    /// Directory listing per namespace.
    listings: Vec<Vec<PathBuf>>,
    ns_by_path: HashMap<String, usize>,
    /// The canonical folder of each loaded namespace ([`Fs::canonicalize`]).
    folders: HashMap<PathBuf, usize>,
    /// Namespaces whose files are still to be read.
    queue: VecDeque<usize>,
    /// The paths that reached a loaded folder again (`E025`, reported once
    /// each).
    again: HashSet<String>,
}

impl Loader<'_> {
    /// Adds one read file to namespace `ns` and reaches its `using`s.
    /// Returns whether a `using` target was missing (`E025`).
    #[allow(clippy::too_many_arguments)]
    fn add_file(
        &mut self,
        files: &mut Vec<FileUnit>,
        ns: usize,
        name: String,
        path: &Path,
        src: String,
        fs: &mut dyn Fs,
        sources: &mut SourceMap,
        diags: &mut Diags,
    ) -> bool {
        let display = display_name(&self.namespaces[ns].path, &name);
        let file_id = sources.add_file(&display, &src);
        let usings = scan_usings(&src, file_id, &self.namespaces[ns].path);
        let index = files.len();
        files.push(FileUnit {
            name,
            path: path.to_path_buf(),
            file: file_id,
            ns,
            src,
            usings: usings.clone(),
        });
        self.namespaces[ns].files.push(index);

        let mut failed = false;
        for Using {
            path: target_path,
            span,
            ..
        } in usings
        {
            if self.reach(&target_path, Some(span), fs, diags) == Reach::Missing {
                diags.emit(
                    Diagnostic::error(
                        codes::E025,
                        format!("cannot find module folder `{target_path}`"),
                    )
                    .label(span, true, "imported here"),
                );
                failed = true;
            }
        }
        failed
    }

    /// Queues the namespace at dotted `path` and its ancestors (each once).
    /// A missing ancestor folder (a library's parent) is skipped. Two cases
    /// are `E025`, reported here at `at` (the `using` that reached it, if
    /// one did), and load nothing: a folder of a library mounted inside
    /// the project, named by its path in the project ([`Self::mounted_at`]),
    /// and a folder another namespace already loaded (a second path to it,
    /// as through a link).
    fn reach(&mut self, path: &str, at: Option<Span>, fs: &mut dyn Fs, diags: &mut Diags) -> Reach {
        if self.ns_by_path.contains_key(path) {
            return Reach::Loaded;
        }
        if self.again.contains(path) {
            return Reach::Again;
        }
        let dir = resolve_dir(self.root, self.libraries, path);
        let Ok(entries) = fs.list_dir(&dir) else {
            return Reach::Missing;
        };
        let folder = canonical_folder(fs, &dir);
        if let Some((library, rest)) = self.mounted_at(path, &folder) {
            let link = self.through_link(path, fs);
            self.report_mounted(path, library, &rest, link, at, diags);
            return Reach::Again;
        }
        if let Some((parent, _)) = path.rsplit_once('.') {
            let _ = self.reach(parent, at, fs, diags);
        }
        if let Some(&first) = self.folders.get(&folder) {
            self.report_again(path, first, at, fs, diags);
            return Reach::Again;
        }
        self.folders.insert(folder, self.namespaces.len());
        self.ns_by_path
            .insert(path.to_owned(), self.namespaces.len());
        self.queue.push_back(self.namespaces.len());
        self.namespaces.push(Namespace {
            path: path.to_owned(),
            dir,
            files: Vec::new(),
        });
        self.listings.push(entries);
        Reach::Loaded
    }

    /// `E025` for `path`, whose folder namespace `first` already loaded.
    /// The headline says "through a link" only when one of the two paths
    /// passes through one ([`Self::through_link`]).
    fn report_again(
        &mut self,
        path: &str,
        first: usize,
        at: Option<Span>,
        fs: &mut dyn Fs,
        diags: &mut Diags,
    ) {
        let first_path = self.namespaces[first].path.clone();
        let link = self.through_link(path, fs) || self.through_link(&first_path, fs);
        let first = match first_path.as_str() {
            "" => "the project root".to_owned(),
            name => format!("`{name}`"),
        };
        let how = if link { " (through a link)" } else { "" };
        let mut diagnostic = Diagnostic::error(
            codes::E025,
            format!("folder `{path}` is {first} again{how}"),
        )
        .note(format!(
            "both paths lead to one folder, which loads once, as {first}"
        ));
        if let Some(at) = at {
            diagnostic = diagnostic.label(at, true, "imported here");
        }
        diags.emit(diagnostic);
        self.again.insert(path.to_owned());
    }

    /// `E025` for `path`, a folder (`rest` below its root) of library
    /// `library`, which is mounted inside the project: reached through a
    /// `link`, or named by its path in the project.
    fn report_mounted(
        &mut self,
        path: &str,
        library: usize,
        rest: &[String],
        link: bool,
        at: Option<Span>,
        diags: &mut Diags,
    ) {
        let name = &self.libraries[library].0;
        let mut mounted = name.clone();
        for segment in rest {
            mounted.push('.');
            mounted.push_str(segment);
        }
        let mut diagnostic = if link {
            Diagnostic::error(
                codes::E025,
                format!("folder `{path}` is `{mounted}` again (through a link)"),
            )
            .note(format!(
                "both paths lead to one folder, which loads once, as `{mounted}`"
            ))
        } else {
            let headline = if rest.is_empty() {
                format!("folder `{path}` is mounted as library `{name}`")
            } else {
                format!("folder `{path}` is `{mounted}`, in the library mounted as `{name}`")
            };
            Diagnostic::error(codes::E025, headline)
                .note(format!(
                    "a library loads once, under its mount name `{name}`"
                ))
                .help(format!("write `using {mounted}`"))
        };
        if let Some(at) = at {
            diagnostic = diagnostic.label(at, true, "imported here");
        }
        diags.emit(diagnostic);
        self.again.insert(path.to_owned());
    }

    /// The library whose folder holds `folder` (the canonical folder of
    /// dotted `path`), with the segments below that library's folder, when
    /// it is not `path`'s own library and is mounted inside `path`'s tree
    /// (the project root, or `path`'s own library): a vendored library. A
    /// library whose folder holds the project is not one.
    fn mounted_at(&self, path: &str, folder: &Path) -> Option<(usize, Vec<String>)> {
        let own = self.library_of(path);
        let base = own.map_or(&self.root_folder, |own| &self.library_folders[own]);
        self.library_folders
            .iter()
            .enumerate()
            .filter(|(index, library)| {
                Some(*index) != own && *library != base && library.starts_with(base)
            })
            .find_map(|(index, library)| {
                let rest = folder.strip_prefix(library).ok()?;
                let segments = rest
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    .collect();
                Some((index, segments))
            })
    }

    /// The index of the library dotted `path` starts at, if any.
    fn library_of(&self, path: &str) -> Option<usize> {
        let first = path.split('.').next().unwrap_or_default();
        self.libraries.iter().position(|(name, _)| name == first)
    }

    /// Whether the folder of dotted `path` is reached through a link: a
    /// segment below its base (the root, or its library's folder) whose
    /// canonical folder is not its canonical parent's entry of that name.
    fn through_link(&self, path: &str, fs: &mut dyn Fs) -> bool {
        if path.is_empty() {
            return false;
        }
        let segments = path.split('.').count() - usize::from(self.library_of(path).is_some());
        let mut dir = resolve_dir(self.root, self.libraries, path);
        for _ in 0..segments {
            let (Some(parent), Some(name)) = (dir.parent(), dir.file_name()) else {
                return false;
            };
            if canonical_folder(fs, parent).join(name) != canonical_folder(fs, &dir) {
                return true;
            }
            dir = parent.to_path_buf();
        }
        false
    }

    /// Reads the files of every queued namespace (reaching their `using`s).
    /// Returns whether anything failed (`E025`).
    fn drain(
        &mut self,
        files: &mut Vec<FileUnit>,
        fs: &mut dyn Fs,
        sources: &mut SourceMap,
        diags: &mut Diags,
    ) -> bool {
        let mut failed = false;
        while let Some(ns) = self.queue.pop_front() {
            let mut file_paths: Vec<PathBuf> = self.listings[ns]
                .iter()
                .filter(|path| is_lilac_file(path))
                .cloned()
                .collect();
            file_paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

            for path in file_paths {
                let Ok(src) = fs.read(&path) else {
                    diags.emit(Diagnostic::error(
                        codes::E025,
                        format!("cannot read module file `{}`", path.display()),
                    ));
                    self.queue.clear();
                    return true;
                };
                let name = file_name_str(&path);
                failed |= self.add_file(files, ns, name, &path, src, fs, sources, diags);
            }
        }
        failed
    }

    /// Reaches the namespace at `path` and every folder below it whose name
    /// is an identifier, so a mounted library loads whole. A folder reached
    /// again ([`Reach::Again`]) ends the walk there, so a link back up the
    /// tree is followed once.
    fn reach_tree(&mut self, path: &str, fs: &mut dyn Fs, diags: &mut Diags) -> Reach {
        let reached = self.reach(path, None, fs, diags);
        if reached == Reach::Loaded {
            self.reach_children(path, fs, diags);
        }
        reached
    }

    /// Reaches every identifier-named folder below the (reached) namespace
    /// at `path`, `""` being the project root. In the project's own tree,
    /// the build folder ([`Self::is_build_folder`]), a folder with its own
    /// `lilac.toml` and a library's mount name are skipped, and in any tree
    /// a folder of a library mounted inside it ([`Self::mounted_at`]).
    fn reach_children(&mut self, path: &str, fs: &mut dyn Fs, diags: &mut Diags) {
        let ns = self.ns_by_path[path];
        let in_project = path.is_empty() || !self.is_library(path);
        let mut children: Vec<String> = self.listings[ns]
            .iter()
            .filter(|entry| !is_lilac_file(entry))
            .map(|entry| file_name_str(entry))
            .filter(|name| is_identifier(name))
            .collect();
        children.sort();
        for child in children {
            let child_path = if path.is_empty() {
                child.clone()
            } else {
                format!("{path}.{child}")
            };
            if in_project && path.is_empty() && self.is_library(&child) {
                continue;
            }
            if in_project && self.is_build_folder(&child_path, fs) {
                continue;
            }
            if in_project && self.is_other_project(&child_path, fs) {
                continue;
            }
            // A library mounted inside this tree loads under its mount
            // name, not again here. (Reached through a link, it is the
            // link's `E025`.)
            let dir = resolve_dir(self.root, self.libraries, &child_path);
            if self
                .mounted_at(&child_path, &canonical_folder(fs, &dir))
                .is_some()
                && !self.through_link(&child_path, fs)
            {
                continue;
            }
            // A non-folder entry (a stray file) simply fails to list.
            let _ = self.reach_tree(&child_path, fs, diags);
        }
    }

    /// Whether the project folder at dotted `path` is the build folder: the
    /// one the load names, else `build` under the root.
    fn is_build_folder(&self, path: &str, fs: &mut dyn Fs) -> bool {
        match &self.build_folder {
            Some(folder) => {
                canonical_folder(fs, &resolve_dir(self.root, self.libraries, path)) == *folder
            }
            None => path == "build",
        }
    }

    /// Whether dotted `path` starts at a mounted library.
    fn is_library(&self, path: &str) -> bool {
        let first = path.split('.').next().unwrap_or_default();
        self.libraries.iter().any(|(name, _)| name == first)
    }

    /// Whether the folder at dotted `path` holds its own `lilac.toml`.
    fn is_other_project(&self, path: &str, fs: &mut dyn Fs) -> bool {
        let dir = resolve_dir(self.root, self.libraries, path);
        fs.list_dir(&dir).is_ok_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.file_name() == Some(OsStr::new(MANIFEST_FILE)))
        })
    }

    fn project(self, files: Vec<FileUnit>) -> Project {
        Project {
            root: self.root.to_path_buf(),
            namespaces: self.namespaces,
            files,
        }
    }
}

/// What [`Loader::reach`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    /// The namespace is loaded (now or before).
    Loaded,
    /// Its folder cannot be listed.
    Missing,
    /// Its folder is one another namespace already loaded (`E025`,
    /// reported).
    Again,
}

/// The canonical form of folder `dir` ([`Fs::canonicalize`]), or `dir`
/// itself when there is none: the folder was just listed, so that is rare,
/// and it is then told apart by the path that reached it.
fn canonical_folder(fs: &mut dyn Fs, dir: &Path) -> PathBuf {
    fs.canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
}

/// Lexes and parses every file of `project` (diagnostics into `diags`).
/// Returns each file's AST and whether any construct failed to parse. With
/// `stop_at_failure`, parsing stops after the first file that failed.
pub fn parse_all(
    project: &Project,
    diags: &mut Diags,
    stop_at_failure: bool,
) -> (Vec<(FileId, lilac_ast::Program)>, bool) {
    let mut programs = Vec::with_capacity(project.files.len());
    let mut failed = false;
    for unit in &project.files {
        let (tokens, lex_errors) = Lexer::lex_all(&unit.src, unit.file);
        for error in lex_errors {
            diags.emit(error);
        }
        let mut parser = Parser::new(tokens, unit.file);
        if let Ok(program) = parser.parse_program(diags) {
            programs.push((unit.file, program));
        }
        failed |= parser.had_errors();
        if failed && stop_at_failure {
            break;
        }
    }
    (programs, failed)
}

/// A one-file project: `src` is the whole root namespace (single-file mode, and
/// tests). Its `using`s are recorded but cannot load anything.
pub fn single_file(name: &str, src: &str, sources: &mut SourceMap) -> Project {
    let file = sources.add_file(name, src);
    let usings = scan_usings(src, file, "");
    Project {
        root: PathBuf::from("."),
        namespaces: vec![Namespace {
            path: String::new(),
            dir: PathBuf::from("."),
            files: vec![0],
        }],
        files: vec![FileUnit {
            name: name.to_owned(),
            path: PathBuf::from(name),
            file,
            ns: 0,
            src: src.to_owned(),
            usings,
        }],
    }
}

/// Whether a folder name can be a namespace segment (and a library's
/// name).
#[must_use]
pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `path` names a `*.lilac` source file.
fn is_lilac_file(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str) == Some("lilac")
}

/// The file name of `path` as a `String` (`""` if it has none).
fn file_name_str(path: &Path) -> String {
    path.file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_owned()
}

/// A file's name in diagnostics and stack traces: its namespace's folder path
/// from the root, `/`-separated, then the file name (`Std/Heap/alloc.lilac`
/// for namespace `Std.Heap`, a mounted library's included), so it reads the
/// same on every OS. A root-namespace file keeps `name`: its bare file name,
/// or in single-file mode the path it was given as.
fn display_name(ns_path: &str, name: &str) -> String {
    if ns_path.is_empty() {
        name.to_owned()
    } else {
        format!("{}/{name}", ns_path.replace('.', "/"))
    }
}

/// Resolves a dotted namespace path: below a mounted library when its first
/// segment names one (`Standard.C` -> `<lib>/C`), else below `root`.
fn resolve_dir(root: &Path, libraries: &[(String, PathBuf)], dotted: &str) -> PathBuf {
    let mut segments = dotted.split('.');
    let first = segments.next().unwrap_or_default();
    let mut dir = match libraries.iter().find(|(name, _)| name == first) {
        Some((_, dir)) => dir.clone(),
        None => root.join(first),
    };
    for segment in segments {
        dir.push(segment);
    }
    dir
}

/// Lexes and parses `src` far enough to collect its `using` paths (relative
/// ones joined to `ns_path`, the file's own namespace) with their spans.
fn scan_usings(src: &str, file: FileId, ns_path: &str) -> Vec<Using> {
    let (tokens, _lex_errors) = Lexer::lex_all(src, file);
    let mut ignore = Diags::new();
    let mut parser = Parser::new(tokens, file);
    let Ok(program) = parser.parse_program(&mut ignore) else {
        return Vec::new();
    };
    // A file with any failed construct must contribute no `using`s: before
    // item-level recovery a parse failure made this return early, so the loader
    // never followed such a file's imports. Gate on the parser's failure signal
    // (`parse_program` now always returns `Ok`) to keep strict loading identical.
    if parser.had_errors() {
        return Vec::new();
    }
    let mut usings = Vec::new();
    for item in &program.items {
        collect_usings(item, ns_path, true, &mut usings);
    }
    usings
}

/// The `using`s of `program`'s live items (its file in namespace
/// `ns_path`): conditional items are skipped, since the driver decides them
/// and splices the selected arm in before resolving.
#[must_use]
pub fn usings_of(program: &lilac_ast::Program, ns_path: &str) -> Vec<Using> {
    let mut usings = Vec::new();
    for item in &program.items {
        collect_usings(item, ns_path, false, &mut usings);
    }
    usings
}

/// Collects `using`s from `item`, descending into `private` blocks, and into
/// every arm of a conditional when `all_arms` is set (the loader loads what
/// any arm may name).
fn collect_usings(item: &Item, ns_path: &str, all_arms: bool, out: &mut Vec<Using>) {
    match &item.kind {
        ItemKind::Using {
            path,
            relative,
            alias,
            segments,
        } => out.push(Using {
            path: if *relative && !ns_path.is_empty() {
                format!("{ns_path}.{path}")
            } else {
                path.clone()
            },
            span: item.span,
            alias: alias.clone(),
            segments: segments.clone(),
        }),
        ItemKind::Private { items } => {
            for inner in items {
                collect_usings(inner, ns_path, all_arms, out);
            }
        }
        ItemKind::Conditional(conditional) if all_arms => {
            let arms = conditional.arms.iter().map(|arm| &arm.items);
            for inner in arms.chain(conditional.els.iter()).flatten() {
                collect_usings(inner, ns_path, all_arms, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The paths of `unit`'s `using`s.
    fn using_paths(unit: &FileUnit) -> Vec<&str> {
        unit.usings
            .iter()
            .map(|using| using.path.as_str())
            .collect()
    }

    struct MemFs {
        files: HashMap<PathBuf, String>,
        dirs: HashMap<PathBuf, Vec<PathBuf>>,
        /// Folder links: a link's path and the folder it leads to.
        links: Vec<(PathBuf, PathBuf)>,
    }

    impl MemFs {
        fn new() -> Self {
            Self {
                files: HashMap::new(),
                dirs: HashMap::new(),
                links: Vec::new(),
            }
        }

        fn add(&mut self, root: &Path, rel: &str, src: &str) {
            let path = join_rel(root, rel);
            self.add_entry(&path);
            self.files.insert(path, src.to_owned());
        }

        /// Makes `root/rel` a folder link to `root/target`.
        fn link(&mut self, root: &Path, rel: &str, target: &str) {
            let path = join_rel(root, rel);
            self.add_entry(&path);
            self.links.push((path, join_rel(root, target)));
        }

        /// Every ancestor folder of `path` lists its child, as a real
        /// directory does.
        fn add_entry(&mut self, path: &Path) {
            let mut child = path.to_path_buf();
            while let Some(parent) = child.parent().filter(|p| !p.as_os_str().is_empty()) {
                let entries = self.dirs.entry(parent.to_path_buf()).or_default();
                if !entries.contains(&child) {
                    entries.push(child.clone());
                }
                child = parent.to_path_buf();
            }
        }

        /// `path` with every link on it followed, as the OS resolves one.
        fn resolve(&self, path: &Path) -> PathBuf {
            let mut path = path.to_path_buf();
            loop {
                let followed = self.links.iter().find_map(|(link, target)| {
                    path.strip_prefix(link).ok().map(|rest| target.join(rest))
                });
                match followed {
                    Some(next) => path = next,
                    None => return path,
                }
            }
        }
    }

    impl Fs for MemFs {
        fn read(&mut self, path: &Path) -> io::Result<String> {
            self.files
                .get(&self.resolve(path))
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
        }

        fn list_dir(&mut self, dir: &Path) -> io::Result<Vec<PathBuf>> {
            let real = self.resolve(dir);
            let entries = self
                .dirs
                .get(&real)
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such directory"))?;
            // Entries are listed under the path asked for, links and all.
            Ok(entries
                .iter()
                .map(|entry| dir.join(entry.strip_prefix(&real).unwrap_or(entry)))
                .collect())
        }

        fn canonicalize(&mut self, path: &Path) -> io::Result<PathBuf> {
            let real = self.resolve(path);
            if self.dirs.contains_key(&real) || self.files.contains_key(&real) {
                Ok(real)
            } else {
                Err(io::Error::new(io::ErrorKind::NotFound, "no such path"))
            }
        }
    }

    fn join_rel(root: &Path, rel: &str) -> PathBuf {
        let mut path = root.to_path_buf();
        for segment in rel.split('/') {
            path.push(segment);
        }
        path
    }

    fn e025_count(diags: &Diags) -> usize {
        diags
            .diagnostics()
            .iter()
            .filter(|diag| diag.code == "E025")
            .count()
    }

    /// Test-local [`load`] with a throwaway [`SourceMap`] (most tests do not
    /// inspect span resolution).
    fn load(root: &Path, fs: &mut MemFs, diags: &mut Diags) -> Result<Project, ErrorGuaranteed> {
        let mut sources = SourceMap::new();
        super::load(root, fs, &mut sources, diags)
    }

    #[test]
    fn loads_root_and_used_namespace_sorted() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using util\n");
        fs.add(&root, "alpha.lilac", "get: function () nothing {}\n");
        fs.add(
            &root,
            "util/helper.lilac",
            "helper: function () nothing {}\n",
        );

        let mut diags = Diags::new();
        let project = load(&root, &mut fs, &mut diags).expect("load succeeds");

        assert!(!diags.has_errors(), "{diags:?}");
        assert_eq!(project.root, root);

        let root_ns = project.namespaces.first().expect("root namespace");
        assert_eq!(root_ns.path, "");
        let util_ns = project
            .namespaces
            .iter()
            .find(|ns| ns.path == "util")
            .expect("util namespace loaded");

        let root_names: Vec<&str> = root_ns
            .files
            .iter()
            .map(|index| project.files[*index].name.as_str())
            .collect();
        assert_eq!(root_names, ["alpha.lilac", "main.lilac"]);

        assert_eq!(util_ns.files.len(), 1);
        let helper = &project.files[util_ns.files[0]];
        assert_eq!(helper.name, "helper.lilac");
        assert_eq!(helper.ns, 1);

        let main = project
            .files
            .iter()
            .find(|file| file.name == "main.lilac")
            .expect("main file");
        assert_eq!(using_paths(main), ["util"]);
    }

    #[test]
    fn parse_broken_file_contributes_no_usings() {
        // A file with a failed item must not contribute its `using`s: before
        // item-level recovery a parse failure skipped scanning, so a broken
        // file's import of a missing folder stayed silent (no `E025`). Recovery
        // must preserve that strict-mode behavior.
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "broken: function (\nusing missing\n");

        let mut diags = Diags::new();
        let project = load(&root, &mut fs, &mut diags).expect("load succeeds");

        assert!(
            !diags.has_errors(),
            "the broken file's `using` must not be followed: {diags:?}"
        );
        let main = project
            .files
            .iter()
            .find(|file| file.name == "main.lilac")
            .expect("main file");
        assert!(main.usings.is_empty(), "{:?}", main.usings);
    }

    #[test]
    fn missing_using_emits_e025_and_err() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using missing\n");

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let result = super::load(&root, &mut fs, &mut sources, &mut diags);

        assert!(result.is_err());
        assert_eq!(e025_count(&diags), 1, "{diags:?}");
        let diagnostic = diags
            .diagnostics()
            .iter()
            .find(|diag| diag.code == "E025")
            .expect("E025 diagnostic");
        let label = diagnostic
            .labels
            .iter()
            .find(|label| label.primary)
            .expect("primary label");
        assert!(!label.span.is_empty());
        // The `using` span must resolve against the caller's `SourceMap`, so the
        // driver can render a `--> file:line` label (spec §10).
        assert_eq!(
            sources.span_text(label.span),
            Some("using missing"),
            "E025 label span does not resolve in the SourceMap"
        );
    }

    #[test]
    fn using_a_file_not_a_directory_is_e025() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using util\n");
        fs.add(&root, "util", "not a directory\n");

        let mut diags = Diags::new();
        let result = load(&root, &mut fs, &mut diags);

        assert!(result.is_err());
        assert_eq!(e025_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn cycles_load_each_folder_once() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using a\n");
        fs.add(&root, "a/a.lilac", "using b\n");
        fs.add(&root, "b/b.lilac", "using a\n");

        let mut diags = Diags::new();
        let project = load(&root, &mut fs, &mut diags).expect("load succeeds");

        assert!(!diags.has_errors(), "{diags:?}");
        let paths: Vec<&str> = project
            .namespaces
            .iter()
            .map(|ns| ns.path.as_str())
            .collect();
        assert_eq!(paths, ["", "a", "b"]);
    }

    #[test]
    fn nested_dotted_using_resolves_from_root() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using a.b\n");
        fs.add(&root, "a/b/deep.lilac", "deep: function () nothing {}\n");

        let mut diags = Diags::new();
        let project = load(&root, &mut fs, &mut diags).expect("load succeeds");

        assert!(!diags.has_errors(), "{diags:?}");
        let deep_ns = project
            .namespaces
            .iter()
            .find(|ns| ns.path == "a.b")
            .expect("a.b namespace loaded");
        assert_eq!(project.files[deep_ns.files[0]].name, "deep.lilac");
    }

    #[test]
    fn relative_using_resolves_from_the_importing_namespace() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using lib\n");
        fs.add(&root, "lib/box.lilac", "using *.heap\n");
        fs.add(
            &root,
            "lib/heap/alloc.lilac",
            "alloc: function () nothing {}\n",
        );

        let mut diags = Diags::new();
        let project = load(&root, &mut fs, &mut diags).expect("load succeeds");

        assert!(!diags.has_errors(), "{diags:?}");
        let paths: Vec<&str> = project
            .namespaces
            .iter()
            .map(|ns| ns.path.as_str())
            .collect();
        assert_eq!(paths, ["", "lib", "lib.heap"]);
        let boxed = project
            .files
            .iter()
            .find(|file| file.name == "box.lilac")
            .expect("box file");
        assert_eq!(using_paths(boxed), ["lib.heap"]);
    }

    #[test]
    fn mounted_library_resolves_outside_the_root() {
        let root = PathBuf::from("proj");
        let lib = PathBuf::from("libs").join("std");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using Std.Heap\n");
        fs.add(&lib, "Heap/alloc.lilac", "using *.Raw\n");
        fs.add(&lib, "Heap/Raw/raw.lilac", "raw: function () nothing {}\n");

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let libraries = [("Std".to_owned(), lib.clone())];
        let (project, failed) =
            super::load_with_status(&root, &libraries, None, &mut fs, &mut sources, &mut diags);
        assert!(!failed, "{diags:?}");
        let paths: Vec<&str> = project
            .namespaces
            .iter()
            .map(|ns| ns.path.as_str())
            .collect();
        assert_eq!(paths, ["", "Std", "Std.Heap", "Std.Heap.Raw"]);
    }

    #[test]
    fn a_library_namespace_loads_without_a_root_project() {
        let lib = PathBuf::from("libs").join("std");
        let mut fs = MemFs::new();
        fs.add(
            &lib,
            "Heap/alloc.lilac",
            "using *.Raw
",
        );
        fs.add(
            &lib,
            "Heap/Raw/raw.lilac",
            "raw: function () nothing {}
",
        );

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let libraries = [("Std".to_owned(), lib.clone())];
        let (project, failed) = super::load_namespace(
            Path::new("proj"),
            "Std.Heap",
            &libraries,
            &mut fs,
            &mut sources,
            &mut diags,
        );
        assert!(!failed, "{diags:?}");
        let paths: Vec<&str> = project
            .namespaces
            .iter()
            .map(|ns| ns.path.as_str())
            .collect();
        assert_eq!(paths, ["", "Std", "Std.Heap", "Std.Heap.Raw"]);
        assert_eq!(project.files.len(), 2);
    }

    #[test]
    fn reaching_a_namespace_loads_its_ancestors() {
        let root = PathBuf::from("proj");
        let lib = PathBuf::from("libs").join("std");
        let mut fs = MemFs::new();
        fs.add(
            &root,
            "main.lilac",
            "using Std.C
",
        );
        fs.add(
            &lib,
            "int.lilac",
            "Int: structure { data: @bit32 }
",
        );
        fs.add(
            &lib,
            "C/c_int.lilac",
            "CInt: structure { data: Int }
",
        );

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let libraries = [("Std".to_owned(), lib.clone())];
        let (project, failed) =
            super::load_with_status(&root, &libraries, None, &mut fs, &mut sources, &mut diags);
        assert!(!failed, "{diags:?}");
        let paths: Vec<&str> = project
            .namespaces
            .iter()
            .map(|ns| ns.path.as_str())
            .collect();
        assert_eq!(paths, ["", "Std", "Std.C"]);
    }

    /// A single file loads the libraries it `using`s, but not its siblings.
    #[test]
    fn single_file_loads_its_libraries_not_its_siblings() {
        let root = Path::new("/w");
        let mut fs = MemFs::new();
        fs.add(root, "app/main.lilac", "using Std.Io\n");
        fs.add(root, "app/sibling.lilac", "x: function () nothing {\n}\n");
        fs.add(root, "std/Io/io.lilac", "print: function () nothing {\n}\n");
        let libraries = vec![("Std".to_owned(), join_rel(root, "std"))];
        let main = join_rel(root, "app/main.lilac");
        let mut sources = SourceMap::default();
        let mut diags = Diags::default();
        let (project, failed) = load_file(
            &main.display().to_string(),
            "using Std.Io\n",
            &libraries,
            &mut fs,
            &mut sources,
            &mut diags,
        );
        assert!(!failed, "{:?}", diags.diagnostics());
        let names: Vec<&str> = project.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, [main.display().to_string().as_str(), "io.lilac"]);
    }

    /// A mounted library loads whole: folders no `using` names are read,
    /// and folders whose names are not identifiers are skipped.
    #[test]
    fn a_mounted_library_loads_every_folder() {
        let root = PathBuf::from("proj");
        let lib = PathBuf::from("libs").join("std");
        let mut fs = MemFs::new();
        fs.add(
            &root,
            "main.lilac",
            "main: function () nothing {}
",
        );
        fs.add(
            &lib,
            "Io/io.lilac",
            "print: function () nothing {}
",
        );
        fs.add(
            &lib,
            "Io/Raw/raw.lilac",
            "raw: function () nothing {}
",
        );
        fs.add(
            &lib,
            ".hidden/x.lilac",
            "x: function () nothing {}
",
        );
        fs.add(
            &lib,
            "notes.txt",
            "not a folder
",
        );

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let libraries = [("Std".to_owned(), lib.clone())];
        let (project, failed) =
            super::load_with_status(&root, &libraries, None, &mut fs, &mut sources, &mut diags);
        assert!(!failed, "{diags:?}");
        let paths: Vec<&str> = project
            .namespaces
            .iter()
            .map(|ns| ns.path.as_str())
            .collect();
        assert_eq!(paths, ["", "Std", "Std.Io", "Std.Io.Raw"]);
        assert_eq!(project.files.len(), 3);
    }

    /// A project loads whole too, except its build folder, a nested project
    /// and a folder named like a library.
    #[test]
    fn a_project_loads_every_folder() {
        let root = PathBuf::from("proj");
        let lib = PathBuf::from("libs").join("std");
        let mut fs = MemFs::new();
        fs.add(
            &root,
            "main.lilac",
            "main: function () nothing {}
",
        );
        fs.add(
            &root,
            "Geo/geo.lilac",
            "area: function () nothing {}
",
        );
        fs.add(
            &root,
            "Geo/Shapes/shapes.lilac",
            "s: function () nothing {}
",
        );
        fs.add(
            &root,
            "build/debug/stray.lilac",
            "b: function () nothing {}
",
        );
        fs.add(
            &root,
            "Other/lilac.toml",
            "{}
",
        );
        fs.add(
            &root,
            "Other/main.lilac",
            "o: function () nothing {}
",
        );
        fs.add(
            &root,
            "Std/shadow.lilac",
            "x: function () nothing {}
",
        );
        fs.add(
            &lib,
            "io.lilac",
            "print: function () nothing {}
",
        );

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let libraries = [("Std".to_owned(), lib.clone())];
        let (project, failed) =
            super::load_with_status(&root, &libraries, None, &mut fs, &mut sources, &mut diags);
        assert!(!failed, "{diags:?}");
        let paths: Vec<&str> = project
            .namespaces
            .iter()
            .map(|ns| ns.path.as_str())
            .collect();
        assert_eq!(paths, ["", "Geo", "Geo.Shapes", "Std"]);
        let names: Vec<&str> = project.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["main.lilac", "geo.lilac", "shapes.lilac", "io.lilac"]
        );
    }

    /// A root with no source of its own (a folder of projects) loads only
    /// what its `using`s reach.
    #[test]
    fn a_root_without_source_loads_no_folders() {
        let root = PathBuf::from("repo");
        let mut fs = MemFs::new();
        fs.add(
            &root,
            "app/main.lilac",
            "main: function () nothing {}
",
        );
        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let (project, failed) =
            super::load_with_status(&root, &[], None, &mut fs, &mut sources, &mut diags);
        assert!(!failed, "{diags:?}");
        assert_eq!(project.namespaces.len(), 1);
        assert!(project.files.is_empty());
    }

    /// A build folder the load names is skipped wherever it is in the
    /// tree, and a folder named `build` is then an ordinary namespace.
    #[test]
    fn a_named_build_folder_is_skipped() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        for rel in [
            "main.lilac",
            "build/tools.lilac",
            "out/debug/stray.lilac",
            "Geo/out/nested.lilac",
        ] {
            fs.add(&root, rel, "x: function () nothing {}\n");
        }
        let load = |fs: &mut MemFs, build_dir: Option<&Path>| {
            let mut diags = Diags::new();
            let (project, failed) = super::load_with_status(
                &root,
                &[],
                build_dir,
                fs,
                &mut SourceMap::new(),
                &mut diags,
            );
            assert!(!failed, "{diags:?}");
            project
                .namespaces
                .iter()
                .map(|ns| ns.path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            load(&mut fs, None),
            ["", "Geo", "Geo.out", "out", "out.debug"]
        );
        assert_eq!(
            load(&mut fs, Some(&root.join("out"))),
            ["", "Geo", "Geo.out", "build"]
        );
        assert_eq!(
            load(&mut fs, Some(&root.join("Geo").join("out"))),
            ["", "Geo", "build", "out", "out.debug"]
        );
        // A build folder outside the tree skips nothing.
        assert_eq!(
            load(&mut fs, Some(Path::new("elsewhere"))),
            ["", "Geo", "Geo.out", "build", "out", "out.debug"]
        );
    }

    #[test]
    fn a_missing_library_folder_is_e025() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(
            &root,
            "main.lilac",
            "main: function () nothing {}
",
        );

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let libraries = [("Std".to_owned(), PathBuf::from("missing"))];
        let (_, failed) =
            super::load_with_status(&root, &libraries, None, &mut fs, &mut sources, &mut diags);
        assert!(failed);
        assert_eq!(e025_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn non_lilac_files_are_ignored() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "get: function () nothing {}\n");
        fs.add(&root, "notes.txt", "ignore me\n");
        fs.add(&root, "README", "ignore me too\n");

        let mut diags = Diags::new();
        let project = load(&root, &mut fs, &mut diags).expect("load succeeds");

        assert!(!diags.has_errors(), "{diags:?}");
        let names: Vec<&str> = project
            .files
            .iter()
            .map(|file| file.name.as_str())
            .collect();
        assert_eq!(names, ["main.lilac"]);
    }

    #[test]
    fn using_inside_private_block_is_honored() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "private {\n    using util\n}\n");
        fs.add(
            &root,
            "util/helper.lilac",
            "helper: function () nothing {}\n",
        );

        let mut diags = Diags::new();
        let project = load(&root, &mut fs, &mut diags).expect("load succeeds");

        assert!(!diags.has_errors(), "{diags:?}");
        assert!(project.namespaces.iter().any(|ns| ns.path == "util"));
        let main = project
            .files
            .iter()
            .find(|file| file.name == "main.lilac")
            .expect("main file");
        assert_eq!(using_paths(main), ["util"]);
    }

    #[test]
    fn using_inside_private_block_missing_target_emits_e025() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "private {\n    using missing\n}\n");

        let mut diags = Diags::new();
        let result = load(&root, &mut fs, &mut diags);

        assert!(result.is_err());
        assert_eq!(e025_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn unreadable_root_folder_emits_e025() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();

        let mut diags = Diags::new();
        let result = load(&root, &mut fs, &mut diags);

        assert!(result.is_err());
        assert_eq!(e025_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn unreadable_file_emits_e025() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.dirs
            .insert(root.clone(), vec![join_rel(&root, "broken.lilac")]);

        let mut diags = Diags::new();
        let result = load(&root, &mut fs, &mut diags);

        assert!(result.is_err());
        assert_eq!(e025_count(&diags), 1, "{diags:?}");
    }

    #[test]
    fn file_unit_carries_read_path() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using util\n");
        fs.add(
            &root,
            "util/helper.lilac",
            "helper: function () nothing {}\n",
        );

        let mut diags = Diags::new();
        let project = load(&root, &mut fs, &mut diags).expect("load succeeds");

        let helper = project
            .files
            .iter()
            .find(|file| file.name == "helper.lilac")
            .expect("helper loaded");
        assert_eq!(helper.path, join_rel(&root, "util/helper.lilac"));
    }

    /// Source-map names are namespace paths, so same-named files in two
    /// folders stay distinguishable in diagnostics and stack traces.
    #[test]
    fn source_names_are_namespace_paths() {
        let root = PathBuf::from("proj");
        let lib = PathBuf::from("libs").join("std");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using util\nusing Std.Heap\n");
        fs.add(
            &root,
            "util/helper.lilac",
            "helper: function () nothing {}\n",
        );
        fs.add(&lib, "Heap/alloc.lilac", "alloc: function () nothing {}\n");

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let libraries = [("Std".to_owned(), lib.clone())];
        let (project, failed) =
            super::load_with_status(&root, &libraries, None, &mut fs, &mut sources, &mut diags);
        assert!(!failed, "{diags:?}");
        let mut names: Vec<&str> = project
            .files
            .iter()
            .map(|unit| sources.file(unit.file).expect("registered").name.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["Std/Heap/alloc.lilac", "main.lilac", "util/helper.lilac"]
        );
    }

    /// The headlines of every `E025`.
    fn e025_messages(diags: &Diags) -> Vec<String> {
        diags
            .diagnostics()
            .iter()
            .filter(|diag| diag.code == "E025")
            .map(|diag| diag.msg.to_string())
            .collect()
    }

    /// A folder link back to its parent (`A/B -> A`) is walked once: the
    /// second path to `A` is `E025`, and nothing below it loads.
    #[test]
    fn a_folder_link_cycle_is_e025_once() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using A\n");
        fs.add(&root, "A/a.lilac", "f: function () nothing {}\n");
        fs.link(&root, "A/B", "A");

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let (project, failed) =
            super::load_with_status(&root, &[], None, &mut fs, &mut sources, &mut diags);
        assert!(failed);
        assert_eq!(
            e025_messages(&diags),
            ["folder `A.B` is `A` again (through a link)"]
        );
        let paths: Vec<&str> = project
            .namespaces
            .iter()
            .map(|ns| ns.path.as_str())
            .collect();
        assert_eq!(paths, ["", "A"]);
        let names: Vec<&str> = project.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main.lilac", "a.lilac"]);
    }

    /// A `using` through the link reports at the `using`, and a link to the
    /// project's own root is the root again.
    #[test]
    fn a_using_through_a_link_is_e025_at_the_using() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "using A.B\nusing Up\n");
        fs.add(&root, "A/a.lilac", "f: function () nothing {}\n");
        fs.link(&root, "A/B", "A");
        fs.link(&root, "Up", "");

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let (project, failed) =
            super::load_with_status(&root, &[], None, &mut fs, &mut sources, &mut diags);
        assert!(failed);
        assert_eq!(
            e025_messages(&diags),
            [
                "folder `A.B` is `A` again (through a link)",
                "folder `Up` is the project root again (through a link)",
            ]
        );
        for diag in diags.diagnostics() {
            assert_eq!(diag.labels.len(), 1, "{diag:?}");
            let span = diag.labels[0].span;
            assert_eq!(sources.file(span.file).expect("a file").name, "main.lilac");
        }
        assert_eq!(project.files.len(), 2);
    }

    /// Two links to one folder: the first path loads it, the second is
    /// `E025`.
    #[test]
    fn a_second_link_to_a_folder_is_e025() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "main: function () nothing {}\n");
        fs.add(&root, "Shared/s.lilac", "s: function () nothing {}\n");
        fs.link(&root, "Alias", "Shared");

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let (project, failed) =
            super::load_with_status(&root, &[], None, &mut fs, &mut sources, &mut diags);
        assert!(failed);
        assert_eq!(
            e025_messages(&diags),
            ["folder `Shared` is `Alias` again (through a link)"]
        );
        assert_eq!(project.files.len(), 2);
    }

    /// A file argument's folder is not loaded as a folder, so a library
    /// mounted there is no second path to it.
    #[test]
    fn a_library_at_a_root_files_folder_loads() {
        let lib = PathBuf::from("lib");
        let mut fs = MemFs::new();
        fs.add(&lib, "io.lilac", "print: function () nothing {}\n");
        fs.add(
            &lib,
            "Heap/heap.lilac",
            "allocate: function () nothing {}\n",
        );

        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let libraries = [("Std".to_owned(), lib.clone())];
        let (project, failed) = super::load_file(
            "lib/io.lilac",
            "print: function () nothing {}\n",
            &libraries,
            &mut fs,
            &mut sources,
            &mut diags,
        );
        assert!(!failed, "{diags:?}");
        assert_eq!(project.namespaces.len(), 3);
    }

    /// The vendored-library project of the tests below: `proj/main.lilac`
    /// with `uses`, and `proj/vendor/Util` mounted as library `Util`.
    fn vendored(uses: &str) -> (PathBuf, MemFs, [(String, PathBuf); 1]) {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", uses);
        fs.add(
            &root,
            "vendor/Util/util.lilac",
            "helper: function () nothing {}\n",
        );
        fs.add(
            &root,
            "vendor/Util/Deep/deep.lilac",
            "deep: function () nothing {}\n",
        );
        let libraries = [("Util".to_owned(), root.join("vendor").join("Util"))];
        (root, fs, libraries)
    }

    /// A library vendored inside the project loads once, under its mount
    /// name: the project's own walk skips its folder, with no `E025`.
    #[test]
    fn a_vendored_library_loads_once() {
        let (root, mut fs, libraries) = vendored("using Util\n");
        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let (project, failed) =
            super::load_with_status(&root, &libraries, None, &mut fs, &mut sources, &mut diags);
        assert!(!failed, "{diags:?}");
        assert!(!diags.has_errors(), "{diags:?}");
        let paths: Vec<&str> = project
            .namespaces
            .iter()
            .map(|ns| ns.path.as_str())
            .collect();
        assert_eq!(paths, ["", "Util", "vendor", "Util.Deep"]);
        let names: Vec<&str> = project.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main.lilac", "util.lilac", "deep.lilac"]);
    }

    /// A `using` naming a vendored library's folder by its path in the
    /// project is `E025`, which names the mount and the `using` to write.
    #[test]
    fn using_a_vendored_library_by_its_folder_is_e025() {
        let (root, mut fs, libraries) = vendored("using vendor.Util\nusing vendor.Util.Deep\n");
        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let (project, failed) =
            super::load_with_status(&root, &libraries, None, &mut fs, &mut sources, &mut diags);
        assert!(failed);
        assert_eq!(
            e025_messages(&diags),
            [
                "folder `vendor.Util` is mounted as library `Util`",
                "folder `vendor.Util.Deep` is `Util.Deep`, in the library mounted as `Util`",
            ]
        );
        let helps: Vec<String> = diags
            .diagnostics()
            .iter()
            .flat_map(|diag| diag.helps.iter().map(|help| help.text.clone()))
            .collect();
        assert_eq!(helps, ["write `using Util`", "write `using Util.Deep`"]);
        let names: Vec<&str> = project.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["main.lilac", "util.lilac", "deep.lilac"]);
    }

    /// A link to a vendored library is still a second path to it.
    #[test]
    fn a_link_to_a_vendored_library_is_e025() {
        let (root, mut fs, libraries) = vendored("main: function () nothing {}\n");
        fs.link(&root, "Shortcut", "vendor/Util");
        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let (_, failed) =
            super::load_with_status(&root, &libraries, None, &mut fs, &mut sources, &mut diags);
        assert!(failed);
        assert_eq!(
            e025_messages(&diags),
            ["folder `Shortcut` is `Util` again (through a link)"]
        );
    }

    /// Two libraries mounted at one folder: a second path with no link, so
    /// the headline says none.
    #[test]
    fn a_folder_mounted_twice_is_e025_without_a_link() {
        let root = PathBuf::from("proj");
        let mut fs = MemFs::new();
        fs.add(&root, "main.lilac", "main: function () nothing {}\n");
        let lib = PathBuf::from("lib");
        fs.add(&lib, "io.lilac", "print: function () nothing {}\n");
        let libraries = [
            ("First".to_owned(), lib.clone()),
            ("Second".to_owned(), lib.clone()),
        ];
        let mut sources = SourceMap::new();
        let mut diags = Diags::new();
        let (_, failed) =
            super::load_with_status(&root, &libraries, None, &mut fs, &mut sources, &mut diags);
        assert!(failed);
        assert_eq!(e025_messages(&diags), ["folder `Second` is `First` again"]);
    }
}
