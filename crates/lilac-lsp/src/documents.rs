//! Open-document store for the language server.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{DidChangeTextDocumentParams, TextDocumentItem, Url};

/// One open document.
#[derive(Clone, Debug)]
pub struct Document {
    /// Current full text.
    pub text: String,
    /// Client-reported version.
    pub version: i32,
    /// Filesystem path derived from the document URI.
    pub path: PathBuf,
}

/// All open documents, keyed by URI.
#[derive(Debug, Default)]
pub struct Documents {
    map: HashMap<Url, Document>,
}

impl Documents {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records an opened document (ignored when the URI has no file path).
    pub fn open(&mut self, item: &TextDocumentItem) {
        let Ok(path) = item.uri.to_file_path() else {
            return;
        };
        self.map.insert(
            item.uri.clone(),
            Document {
                text: item.text.clone(),
                version: item.version,
                path,
            },
        );
    }

    /// Applies a full-text change to an open document.
    pub fn change(&mut self, params: &DidChangeTextDocumentParams) {
        let Some(document) = self.map.get_mut(&params.text_document.uri) else {
            return;
        };
        if let Some(change) = params.content_changes.last() {
            document.text.clone_from(&change.text);
        }
        document.version = params.text_document.version;
    }

    /// Removes a closed document.
    pub fn close(&mut self, uri: &Url) {
        self.map.remove(uri);
    }

    /// Snapshot of open documents: path to current text.
    #[must_use]
    pub fn overlay(&self) -> HashMap<PathBuf, String> {
        self.map
            .values()
            .map(|document| (document.path.clone(), document.text.clone()))
            .collect()
    }

    /// The filesystem path of an open document.
    #[must_use]
    pub fn path_of(&self, uri: &Url) -> Option<&Path> {
        self.map.get(uri).map(|document| document.path.as_path())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        DidChangeTextDocumentParams, TextDocumentContentChangeEvent, TextDocumentItem, Url,
        VersionedTextDocumentIdentifier,
    };

    fn open_item(uri: &Url, text: &str) -> TextDocumentItem {
        TextDocumentItem {
            uri: uri.clone(),
            language_id: "lilac".to_owned(),
            version: 1,
            text: text.to_owned(),
        }
    }

    #[test]
    fn open_change_close_track_text() {
        let mut docs = Documents::new();
        let uri = Url::parse("file:///c:/proj/main.lilac").expect("uri");
        let path = uri.to_file_path().expect("path");

        docs.open(&open_item(&uri, "one"));
        assert_eq!(docs.overlay().get(&path), Some(&"one".to_owned()));
        assert_eq!(docs.path_of(&uri), Some(path.as_path()));

        docs.change(&DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: 2,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: "two".to_owned(),
            }],
        });
        assert_eq!(docs.overlay().get(&path), Some(&"two".to_owned()));

        docs.close(&uri);
        assert!(docs.overlay().is_empty());
        assert!(docs.path_of(&uri).is_none());
    }
}
