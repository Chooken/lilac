//! Open-document store for the language server.
//!
//! The client sends changes incrementally (`TextDocumentSyncKind::INCREMENTAL`):
//! each replaces a range of the text. A document keeps its line starts for
//! the text it holds, and the edits that made it, each with the version it
//! produced, so a request answered from an older compile maps the cursor
//! back through exactly the edits made since ([`map_offset`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{DidChangeTextDocumentParams, TextDocumentItem, Url};

use crate::convert::LineIndex;

/// One open document.
#[derive(Clone, Debug)]
pub struct Document {
    /// Current full text.
    pub text: String,
    /// Client-reported version.
    pub version: i32,
    /// Filesystem path derived from the document URI.
    pub path: PathBuf,
    /// [`lilac_span::line_starts`] of `text`.
    lines: Vec<usize>,
    /// The edits since it opened (or since the oldest compile that may
    /// still answer for it, [`Documents::forget_edits`]), oldest first.
    edits: Vec<Edit>,
}

/// One change to a document: the bytes `start..end` of the text before it
/// became `len` bytes, making version `version`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edit {
    /// Where the replaced text starts.
    pub start: usize,
    /// Where the replaced text ended, before the edit.
    pub end: usize,
    /// The replacement's length.
    pub len: usize,
    /// The document version the edit made.
    pub version: i32,
}

impl Edit {
    /// The edit turning `old` into `new` when a change gives the whole
    /// text: the characters between their common prefix and suffix.
    #[must_use]
    pub fn between(old: &str, new: &str, version: i32) -> Self {
        let (old_bytes, new_bytes) = (old.as_bytes(), new.as_bytes());
        let mut prefix = old_bytes
            .iter()
            .zip(new_bytes)
            .take_while(|(a, b)| a == b)
            .count();
        // The common bytes are the same in both, so one boundary test
        // serves both texts.
        while !old.is_char_boundary(prefix) {
            prefix -= 1;
        }
        let room = old.len().min(new.len()) - prefix;
        let mut suffix = old_bytes
            .iter()
            .rev()
            .zip(new_bytes.iter().rev())
            .take(room)
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(old.len() - suffix) {
            suffix -= 1;
        }
        Self {
            start: prefix,
            end: old.len() - suffix,
            len: new.len() - suffix - prefix,
            version,
        }
    }
}

/// Maps `byte` in the text after `edits` (oldest first) to the matching
/// offset in the text before them: unchanged before an edit, shifted
/// after it, and an offset inside replaced text maps to the
/// replacement's start.
#[must_use]
pub fn map_offset(edits: &[Edit], byte: usize) -> usize {
    edits.iter().rev().fold(byte, |byte, edit| {
        if byte <= edit.start {
            byte
        } else if byte >= edit.start + edit.len {
            byte - edit.len + (edit.end - edit.start)
        } else {
            edit.start
        }
    })
}

impl Document {
    /// A document holding `text` at `version`, with no edits yet.
    #[must_use]
    pub fn new(text: String, version: i32, path: PathBuf) -> Self {
        Self {
            lines: lilac_span::line_starts(&text),
            text,
            version,
            path,
            edits: Vec::new(),
        }
    }

    /// Positions in the current text.
    #[must_use]
    pub fn lines(&self) -> LineIndex<'_> {
        LineIndex::with_starts(&self.text, &self.lines)
    }

    /// The edits made after `version` (every recorded edit for `None`: a
    /// compile that did not see the document open read the text it opened
    /// with).
    #[must_use]
    pub fn edits_after(&self, version: Option<i32>) -> &[Edit] {
        let first = version.map_or(0, |version| {
            self.edits.partition_point(|edit| edit.version <= version)
        });
        &self.edits[first..]
    }

    /// Replaces `start..end` (bytes) with `text` as `version`.
    fn replace(&mut self, start: usize, end: usize, text: &str, version: i32) {
        self.text.replace_range(start..end, text);
        self.lines = lilac_span::line_starts(&self.text);
        self.edits.push(Edit {
            start,
            end,
            len: text.len(),
            version,
        });
    }
}

/// All open documents, keyed by URI.
#[derive(Clone, Debug, Default)]
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
            Document::new(item.text.clone(), item.version, path),
        );
    }

    /// Applies a change notification's edits, in order: each range is in
    /// the text the previous one left, and a change with no range
    /// replaces the whole text.
    pub fn change(&mut self, params: &DidChangeTextDocumentParams) {
        let Some(document) = self.map.get_mut(&params.text_document.uri) else {
            return;
        };
        let version = params.text_document.version;
        for change in &params.content_changes {
            let ranged = change.range.and_then(|range| {
                let lines = document.lines();
                let (start, end) = (lines.offset(range.start)?, lines.offset(range.end)?);
                (start <= end).then_some((start, end))
            });
            let (start, end, replacement) = match (change.range, ranged) {
                (Some(_), Some((start, end))) => (start, end, change.text.as_str()),
                // A range not in the text: the client and server disagree,
                // so assume nothing (the edit covers it all).
                (Some(_), None) => (0, document.text.len(), change.text.as_str()),
                (None, _) => {
                    let edit = Edit::between(&document.text, &change.text, version);
                    let replacement = &change.text[edit.start..edit.start + edit.len];
                    (edit.start, edit.end, replacement)
                }
            };
            document.replace(start, end, replacement, version);
        }
        document.version = version;
    }

    /// Drops `uri`'s edits up to `version`: no compile older than the one
    /// that saw `version` will answer for it again.
    pub fn forget_edits(&mut self, uri: &Url, version: i32) {
        if let Some(document) = self.map.get_mut(uri) {
            let keep = document.edits_after(Some(version)).len();
            let drop = document.edits.len() - keep;
            document.edits.drain(..drop);
        }
    }

    /// Removes a closed document.
    pub fn close(&mut self, uri: &Url) {
        self.map.remove(uri);
    }

    /// Snapshot of open documents: path to current text, under both the
    /// document's own path and its canonical form (see `OverlayFs`).
    #[must_use]
    pub fn overlay(&self) -> HashMap<PathBuf, String> {
        let mut overlay = HashMap::new();
        for document in self.map.values() {
            if let Ok(canonical) = std::fs::canonicalize(&document.path) {
                overlay.insert(canonical, document.text.clone());
            }
            overlay.insert(document.path.clone(), document.text.clone());
        }
        overlay
    }

    /// The filesystem path of an open document.
    #[must_use]
    pub fn path_of(&self, uri: &Url) -> Option<&Path> {
        self.map.get(uri).map(|document| document.path.as_path())
    }

    /// The open document at `uri`.
    #[must_use]
    pub fn get(&self, uri: &Url) -> Option<&Document> {
        self.map.get(uri)
    }

    /// Iterates open documents as `(uri, document)`.
    pub fn iter(&self) -> impl Iterator<Item = (&Url, &Document)> {
        self.map.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        DidChangeTextDocumentParams, Position, Range, TextDocumentContentChangeEvent,
        TextDocumentItem, Url, VersionedTextDocumentIdentifier,
    };

    fn open_item(uri: &Url, text: &str) -> TextDocumentItem {
        TextDocumentItem {
            uri: uri.clone(),
            language_id: "lilac".to_owned(),
            version: 1,
            text: text.to_owned(),
        }
    }

    /// A change: its range's start and end `(line, character)`s, if any,
    /// and its text.
    type Change<'a> = (Option<((u32, u32), (u32, u32))>, &'a str);

    fn change(uri: &Url, version: i32, changes: &[Change]) -> DidChangeTextDocumentParams {
        DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version,
            },
            content_changes: changes
                .iter()
                .map(|(range, text)| TextDocumentContentChangeEvent {
                    range: range.map(|((a, b), (c, d))| {
                        Range::new(Position::new(a, b), Position::new(c, d))
                    }),
                    range_length: None,
                    text: (*text).to_owned(),
                })
                .collect(),
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

        docs.change(&change(&uri, 2, &[(None, "two")]));
        assert_eq!(docs.overlay().get(&path), Some(&"two".to_owned()));

        docs.close(&uri);
        assert!(docs.overlay().is_empty());
        assert!(docs.path_of(&uri).is_none());
    }

    #[test]
    fn ranged_changes_apply_in_order() {
        let mut docs = Documents::new();
        let uri = Url::parse("file:///c:/proj/main.lilac").expect("uri");
        docs.open(&open_item(&uri, "ab\ncd\n"));
        // The second range is in the text the first left.
        docs.change(&change(
            &uri,
            2,
            &[
                (Some(((1, 0), (1, 1))), "XY"),
                (Some(((0, 2), (0, 2))), "é"),
            ],
        ));
        let document = docs.get(&uri).expect("open");
        assert_eq!(document.text, "abé\nXYd\n");
        assert_eq!(document.lines().position(6), Position::new(1, 1));
        assert_eq!(
            document.edits_after(Some(1)),
            [
                Edit {
                    start: 3,
                    end: 4,
                    len: 2,
                    version: 2
                },
                Edit {
                    start: 2,
                    end: 2,
                    len: 2,
                    version: 2
                },
            ]
        );
        assert!(document.edits_after(Some(2)).is_empty());
    }

    #[test]
    fn offsets_map_back_through_each_edit() {
        // "one two three" -> "ONE two THREE": two edits, not one.
        let edits = [
            Edit {
                start: 0,
                end: 3,
                len: 3,
                version: 2,
            },
            Edit {
                start: 8,
                end: 13,
                len: 5,
                version: 3,
            },
        ];
        assert_eq!(map_offset(&edits, 4), 4);
        assert_eq!(map_offset(&edits, 1), 0); // inside the first: its start
                                              // An insertion shifts what follows it.
        let typed = [Edit {
            start: 2,
            end: 2,
            len: 3,
            version: 2,
        }];
        assert_eq!(map_offset(&typed, 1), 1);
        assert_eq!(map_offset(&typed, 2), 2);
        assert_eq!(map_offset(&typed, 4), 2);
        assert_eq!(map_offset(&typed, 5), 2);
        assert_eq!(map_offset(&typed, 7), 4);
        // A deletion.
        let deleted = [Edit {
            start: 2,
            end: 5,
            len: 0,
            version: 2,
        }];
        assert_eq!(map_offset(&deleted, 3), 6);
    }

    #[test]
    fn a_whole_text_change_is_its_smallest_edit() {
        assert_eq!(
            Edit::between("ab\ncd\n", "ab\ncXd\n", 2),
            Edit {
                start: 4,
                end: 4,
                len: 1,
                version: 2
            }
        );
        assert_eq!(
            Edit::between("aaa", "aa", 2),
            Edit {
                start: 2,
                end: 3,
                len: 0,
                version: 2
            }
        );
        // 'é' and 'è' share a first byte; the edit is the whole character.
        assert_eq!(
            Edit::between("aéb", "aèb", 2),
            Edit {
                start: 1,
                end: 3,
                len: 2,
                version: 2
            }
        );
    }

    #[test]
    fn edits_are_forgotten_once_compiled() {
        let mut docs = Documents::new();
        let uri = Url::parse("file:///c:/proj/main.lilac").expect("uri");
        docs.open(&open_item(&uri, "a"));
        docs.change(&change(&uri, 2, &[(Some(((0, 1), (0, 1))), "b")]));
        docs.change(&change(&uri, 3, &[(Some(((0, 2), (0, 2))), "c")]));
        docs.forget_edits(&uri, 2);
        let document = docs.get(&uri).expect("open");
        assert_eq!(document.edits_after(None).len(), 1);
        assert_eq!(document.edits_after(None)[0].version, 3);
    }
}
