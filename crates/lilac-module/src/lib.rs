//! Folder loader: root discovery plus a `using` fixpoint over an injected
//! [`Fs`].
//!
//! A directory is the root namespace; the `*.lilac` files directly inside it
//! are read and parsed only far enough to collect `using` directives. Each
//! `using A.B` resolves root-relative to `<root>/A/B`; newly-reached folders
//! are read and scanned, repeating until no new folder appears. Every folder is
//! visited once, so cycles terminate. A `using` inside a `private { ... }`
//! block is honored (it still loads the folder).
//!
//! `E025` is the module-load error: a `using` whose target is missing or is not
//! a directory, an unreadable root folder, or an unreadable file. Each is
//! reported at the most specific span available (the `using` span where there
//! is one, a dummy span otherwise, with the path named in the headline) and
//! fails the load.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use lilac_ast::{Item, ItemKind};
use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_lex::Lexer;
use lilac_parse::Parser;
use lilac_span::{FileId, SourceMap, Span};

/// Injected filesystem: the loader never touches `std::fs` directly.
pub trait Fs {
    /// Reads a whole file to a string.
    fn read(&mut self, path: &Path) -> io::Result<String>;
    /// Lists a directory's entries (files and subdirectories).
    fn list_dir(&mut self, dir: &Path) -> io::Result<Vec<PathBuf>>;
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
    /// Dotted `using` paths, in source order.
    pub usings: Vec<String>,
    /// Span of each `using` directive, parallel to [`FileUnit::usings`]; the
    /// `E029` ambiguity diagnostic labels these import sites.
    pub using_spans: Vec<Span>,
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
    let (project, failed) = load_with_status(root, fs, sources, diags);
    if failed {
        Err(ErrorGuaranteed::new())
    } else {
        Ok(project)
    }
}

/// Like [`load`], but returns the (possibly partial) project together with a
/// `failed` flag instead of an error, so callers can still map the files read
/// before a bad `using`.
pub fn load_with_status(
    root: &Path,
    fs: &mut dyn Fs,
    sources: &mut SourceMap,
    diags: &mut Diags,
) -> (Project, bool) {
    let Ok(root_entries) = fs.list_dir(root) else {
        diags.emit(Diagnostic::error(
            "E025",
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

    let mut namespaces = vec![Namespace {
        path: String::new(),
        dir: root.to_path_buf(),
        files: Vec::new(),
    }];
    let mut listings = vec![root_entries];
    let mut files: Vec<FileUnit> = Vec::new();
    let mut ns_by_path: HashMap<String, usize> = HashMap::new();
    ns_by_path.insert(String::new(), 0);
    let mut queue = VecDeque::new();
    queue.push_back(0_usize);
    let mut failed = false;

    while let Some(ns) = queue.pop_front() {
        let mut file_paths: Vec<PathBuf> = listings[ns]
            .iter()
            .filter(|path| is_lilac_file(path))
            .cloned()
            .collect();
        file_paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        for path in file_paths {
            let Ok(src) = fs.read(&path) else {
                diags.emit(Diagnostic::error(
                    "E025",
                    format!("cannot read module file `{}`", path.display()),
                ));
                return (
                    Project {
                        root: root.to_path_buf(),
                        namespaces,
                        files,
                    },
                    true,
                );
            };
            let name = file_name_str(&path);
            let file_id = sources.add_file(&name, &src);
            let usings = scan_usings(&src, file_id);
            let index = files.len();
            files.push(FileUnit {
                name,
                path: path.clone(),
                file: file_id,
                ns,
                src,
                usings: usings.iter().map(|(path, _)| path.clone()).collect(),
                using_spans: usings.iter().map(|(_, span)| *span).collect(),
            });
            namespaces[ns].files.push(index);

            for (target_path, span) in usings {
                if ns_by_path.contains_key(&target_path) {
                    continue;
                }
                let target_dir = resolve_dir(root, &target_path);
                if let Ok(entries) = fs.list_dir(&target_dir) {
                    let new_ns = namespaces.len();
                    namespaces.push(Namespace {
                        path: target_path.clone(),
                        dir: target_dir,
                        files: Vec::new(),
                    });
                    listings.push(entries);
                    ns_by_path.insert(target_path, new_ns);
                    queue.push_back(new_ns);
                } else {
                    diags.emit(
                        Diagnostic::error(
                            "E025",
                            format!("cannot find module folder `{target_path}`"),
                        )
                        .label(span, true, "imported here"),
                    );
                    failed = true;
                }
            }
        }
    }

    (
        Project {
            root: root.to_path_buf(),
            namespaces,
            files,
        },
        failed,
    )
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

/// Resolves a dotted namespace path below `root`.
fn resolve_dir(root: &Path, dotted: &str) -> PathBuf {
    let mut dir = root.to_path_buf();
    for segment in dotted.split('.') {
        dir.push(segment);
    }
    dir
}

/// Lexes and parses `src` far enough to collect `using` paths with their spans.
fn scan_usings(src: &str, file: FileId) -> Vec<(String, Span)> {
    let (tokens, _lex_errors) = Lexer::lex_all(src, file);
    let mut ignore = Diags::new();
    let mut parser = Parser::new(tokens, file);
    let Ok(program) = parser.parse_program(&mut ignore) else {
        return Vec::new();
    };
    let mut usings = Vec::new();
    for item in &program.items {
        collect_usings(item, &mut usings);
    }
    usings
}

/// Collects `using` paths from `item`, descending into `private` blocks.
fn collect_usings(item: &Item, out: &mut Vec<(String, Span)>) {
    match &item.kind {
        ItemKind::Using { path } => out.push((path.clone(), item.span)),
        ItemKind::Private { items } => {
            for inner in items {
                collect_usings(inner, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemFs {
        files: HashMap<PathBuf, String>,
        dirs: HashMap<PathBuf, Vec<PathBuf>>,
    }

    impl MemFs {
        fn new() -> Self {
            Self {
                files: HashMap::new(),
                dirs: HashMap::new(),
            }
        }

        fn add(&mut self, root: &Path, rel: &str, src: &str) {
            let path = join_rel(root, rel);
            if let Some(parent) = path.parent() {
                self.dirs
                    .entry(parent.to_path_buf())
                    .or_default()
                    .push(path.clone());
            }
            self.files.insert(path, src.to_owned());
        }
    }

    impl Fs for MemFs {
        fn read(&mut self, path: &Path) -> io::Result<String> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file"))
        }

        fn list_dir(&mut self, dir: &Path) -> io::Result<Vec<PathBuf>> {
            self.dirs
                .get(dir)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such directory"))
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
        assert_eq!(main.usings, ["util"]);
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
        assert_eq!(main.usings, ["util"]);
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
}
