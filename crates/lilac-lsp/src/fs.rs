//! In-memory filesystem that overlays open documents on disk.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use lilac_module::Fs;

/// Filesystem whose `read` prefers an open document's text. A path the
/// loader built (`D:\lib\x.lilac` from a library setting) can spell the
/// same file differently from the editor (`d:\lib\x.lilac`), so a miss is
/// retried with the canonical path.
#[derive(Debug)]
pub struct OverlayFs<'a> {
    /// Open documents: path to current text.
    pub overlay: &'a HashMap<PathBuf, String>,
}

impl Fs for OverlayFs<'_> {
    fn read(&mut self, path: &Path) -> io::Result<String> {
        if let Some(text) = self.overlay.get(path) {
            return Ok(text.clone());
        }
        if let Some(text) = fs::canonicalize(path)
            .ok()
            .and_then(|canonical| self.overlay.get(&canonical))
        {
            return Ok(text.clone());
        }
        fs::read_to_string(path)
    }

    fn list_dir(&mut self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(dir)? {
            entries.push(entry?.path());
        }
        Ok(entries)
    }

    /// The disk's canonical path. A path only open documents give (an
    /// unsaved file, or a folder holding one) is not on disk: it is the
    /// nearest folder that is, canonical, then the rest as written (`.`
    /// and `..` taken lexically), so two spellings of it still agree.
    fn canonicalize(&mut self, path: &Path) -> io::Result<PathBuf> {
        let error = match fs::canonicalize(path) {
            Ok(canonical) => return Ok(canonical),
            Err(error) => error,
        };
        let resolved = beyond_disk(path)?;
        let open = self.overlay.keys().any(|document| {
            beyond_disk(document).is_ok_and(|document| document.starts_with(&resolved))
        });
        if open {
            Ok(resolved)
        } else {
            Err(error)
        }
    }
}

/// `path` made absolute and lexically normal, its longest prefix on disk
/// replaced by that prefix's canonical form.
fn beyond_disk(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normal = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    let mut rest = Vec::new();
    let mut base = normal.as_path();
    loop {
        if let Ok(mut canonical) = fs::canonicalize(base) {
            canonical.extend(rest.iter().rev());
            return Ok(canonical);
        }
        let (Some(name), Some(parent)) = (base.file_name(), base.parent()) else {
            // Not even the root is on disk: the normal path is all there is.
            return Ok(normal);
        };
        rest.push(name);
        base = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn manifest() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
    }

    #[test]
    fn overlay_text_wins_over_disk() {
        let mut overlay = HashMap::new();
        overlay.insert(manifest(), "overlay".to_owned());
        let mut fs = OverlayFs { overlay: &overlay };
        assert_eq!(fs.read(&manifest()).expect("read"), "overlay");
    }

    #[test]
    fn reads_disk_when_not_overlaid() {
        let overlay = HashMap::new();
        let mut fs = OverlayFs { overlay: &overlay };
        let text = fs.read(&manifest()).expect("read");
        assert!(text.contains("lilac-lsp"), "{text}");
    }

    #[test]
    fn missing_path_is_error() {
        let overlay = HashMap::new();
        let mut fs = OverlayFs { overlay: &overlay };
        assert!(fs.read(&PathBuf::from("does/not/exist.lilac")).is_err());
    }

    #[test]
    fn list_dir_includes_manifest() {
        let overlay = HashMap::new();
        let mut fs = OverlayFs { overlay: &overlay };
        let entries = fs
            .list_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR")))
            .expect("list");
        assert!(
            entries
                .iter()
                .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("Cargo.toml")),
            "{entries:?}"
        );
    }

    #[test]
    fn canonicalize_matches_the_disk() {
        let overlay = HashMap::new();
        let mut fs = OverlayFs { overlay: &overlay };
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let spelled = dir.join("src").join("..");
        assert_eq!(
            fs.canonicalize(&spelled).expect("on disk"),
            fs::canonicalize(&dir).expect("on disk")
        );
    }

    /// An open document not yet saved, in a folder not on disk: its folder
    /// canonicalizes below the disk's canonical parent, the same however it
    /// is spelled.
    #[test]
    fn canonicalize_an_unsaved_documents_folder() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = dir.join("not-on-disk");
        let mut overlay = HashMap::new();
        overlay.insert(folder.join("new.lilac"), "x".to_owned());
        let mut fs = OverlayFs { overlay: &overlay };
        let expected = fs::canonicalize(&dir).expect("on disk").join("not-on-disk");
        assert_eq!(fs.canonicalize(&folder).expect("open"), expected);
        let spelled = dir.join("src").join("..").join(".").join("not-on-disk");
        assert_eq!(fs.canonicalize(&spelled).expect("open"), expected);
        assert_eq!(
            fs.canonicalize(&folder.join("new.lilac")).expect("open"),
            expected.join("new.lilac")
        );
    }

    #[test]
    fn canonicalize_a_missing_path_is_an_error() {
        let overlay = HashMap::new();
        let mut fs = OverlayFs { overlay: &overlay };
        let missing = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("not-on-disk");
        assert!(fs.canonicalize(&missing).is_err());
    }
}
