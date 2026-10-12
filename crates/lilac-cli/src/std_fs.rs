//! Production [`Fs`] over `std::fs`.
//!
//! The driver owns no file I/O; the CLI supplies this concrete filesystem to
//! `Session::compile_project` so a folder project can be loaded.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lilac_module::Fs;

/// Reads the real filesystem.
#[derive(Debug, Default)]
pub struct StdFs;

impl Fs for StdFs {
    fn read(&mut self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn list_dir(&mut self, dir: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(dir)? {
            entries.push(entry?.path());
        }
        Ok(entries)
    }

    fn canonicalize(&mut self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }
}
