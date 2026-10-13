//! The workspace's `lilac.toml`, read as the CLI reads it
//! ([`lilac_manifest::Manifest::read`]), with its diagnostics converted
//! for publishing on the file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lilac_error::Diags;
use lilac_manifest::{Manifest, LIB_DIR_VAR, MANIFEST_FILE};
use lilac_span::SourceMap;
use lsp_types::{Diagnostic as LspDiagnostic, Url};

use crate::convert::to_lsp_diagnostic;
use crate::fs::OverlayFs;

/// A workspace's `lilac.toml` as read from disk.
#[derive(Clone, Debug)]
pub struct ManifestFile {
    /// The manifest, `None` when it is not TOML Lilac reads (`E075`). A
    /// setting with an error (`E076`) is left out of it.
    pub manifest: Option<Manifest>,
    /// The file.
    pub uri: Url,
    /// Its diagnostics, to publish on [`ManifestFile::uri`].
    pub diagnostics: Vec<LspDiagnostic>,
}

/// Reads `workspace/lilac.toml`; `None` when there is none.
#[must_use]
pub fn read(workspace: &Path) -> Option<ManifestFile> {
    let overlay = HashMap::new();
    let mut fs = OverlayFs { overlay: &overlay };
    let bundled = lilac_manifest::bundled_dir(
        std::env::var_os(LIB_DIR_VAR).map(PathBuf::from),
        std::env::current_exe()
            .ok()
            .as_deref()
            .and_then(Path::parent),
    );
    let mut sources = SourceMap::new();
    let mut diags = Diags::new();
    let read = Manifest::read(
        workspace,
        bundled.as_deref(),
        &mut fs,
        &mut sources,
        &mut diags,
    )?;
    let uri = Url::from_file_path(workspace.join(MANIFEST_FILE)).ok()?;
    // The manifest is the only file in `sources`.
    let uri_of = |_| Some(uri.clone());
    let diagnostics = diags
        .diagnostics()
        .iter()
        .filter_map(|diagnostic| to_lsp_diagnostic(diagnostic, &sources, &uri_of))
        .collect();
    Some(ManifestFile {
        manifest: read.ok(),
        uri,
        diagnostics,
    })
}
