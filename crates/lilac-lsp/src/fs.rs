//! In-memory filesystem that overlays open documents on disk.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lilac_module::Fs;

/// Filesystem whose `read` prefers an open document's text.
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
        fs::read_to_string(path)
    }

    fn list_dir(&mut self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(dir)? {
            entries.push(entry?.path());
        }
        Ok(entries)
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
}
