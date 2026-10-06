//! Folder loader: root discovery plus a `using` fixpoint over an injected
//! [`Fs`].
//!
//! A directory is the root namespace; the `*.lilac` files directly inside it
//! are read and parsed only far enough to collect `using` directives. Each
//! `using A.B` resolves root-relative to `<root>/A/B`; newly-reached folders
//! are read and scanned, repeating until no new folder appears. Every folder is
//! visited once, so cycles terminate. A `using` whose target is missing or is
//! not a directory emits `E025` at the `using` span and fails the load.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use lilac_ast::ItemKind;
use lilac_error::{Diagnostic, Diags, ErrorGuaranteed};
use lilac_lex::Lexer;
use lilac_parse::Parser;
use lilac_span::{FileId, Span};

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
    /// Owning namespace (index into [`Project::namespaces`]).
    pub ns: usize,
    /// Raw source text.
    pub src: String,
    /// Dotted `using` paths, in source order.
    pub usings: Vec<String>,
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
/// sorted by file name. Files are assigned ids equal to their index in
/// [`Project::files`], so a caller that registers the sources in the same order
/// sees matching spans.
pub fn load(root: &Path, fs: &mut dyn Fs, diags: &mut Diags) -> Result<Project, ErrorGuaranteed> {
    let Ok(root_entries) = fs.list_dir(root) else {
        return Err(ErrorGuaranteed::new());
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
        let mut sources: Vec<PathBuf> = listings[ns]
            .iter()
            .filter(|path| is_lilac_file(path))
            .cloned()
            .collect();
        sources.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        for path in sources {
            let Ok(src) = fs.read(&path) else {
                return Err(ErrorGuaranteed::new());
            };
            let file_id = FileId(u32::try_from(files.len()).unwrap_or(u32::MAX));
            let usings = scan_usings(&src, file_id);
            let index = files.len();
            files.push(FileUnit {
                name: file_name_str(&path),
                ns,
                src,
                usings: usings.iter().map(|(path, _)| path.clone()).collect(),
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
                    diags.emit(Diagnostic::error("E025", "unresolved `using`").label(
                        span,
                        true,
                        format!("cannot find folder `{target_path}`"),
                    ));
                    failed = true;
                }
            }
        }
    }

    if failed {
        return Err(ErrorGuaranteed::new());
    }
    Ok(Project {
        root: root.to_path_buf(),
        namespaces,
        files,
    })
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
    program
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Using { path } => Some((path.clone(), item.span)),
            _ => None,
        })
        .collect()
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

        let mut diags = Diags::new();
        let result = load(&root, &mut fs, &mut diags);

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
}
