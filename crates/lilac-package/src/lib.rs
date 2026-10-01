//! `lilac.zon` manifest types: pure data, no I/O (execution lands in M6).

/// Package manifest (`name/version/paths/deps/link`).
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    /// Package name.
    pub name: String,
    /// Package version.
    pub version: String,
    /// Native libraries to link (`link = ["c"]`).
    pub link: Vec<String>,
}

impl Manifest {
    /// Empty manifest.
    pub fn new() -> Self {
        Self::default()
    }
}
