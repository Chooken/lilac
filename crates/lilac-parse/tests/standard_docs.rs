//! Every `///` comment in `lib/Standard` documents a declaration: the
//! parser gives it to an item, member, field or variant. A `///` it drops
//! (above a statement, or apart from what follows) would never show in an
//! editor, so it must be a plain `//`.

use std::path::{Path, PathBuf};

use lilac_ast::{Item, ItemKind};
use lilac_error::Diags;
use lilac_lex::{CommentKind, Lexer};
use lilac_parse::Parser;
use lilac_span::{FileId, Span};

fn lilac_files(folder: &Path, files: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(folder)
        .expect("read folder")
        .map(|entry| entry.expect("entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            lilac_files(&path, files);
        } else if path
            .extension()
            .is_some_and(|extension| extension == "lilac")
        {
            files.push(path);
        }
    }
}

/// The spans of the docs the parser attached in `items`.
fn attached(items: &[Item], docs: &mut Vec<Span>) {
    for item in items {
        if let Some(doc) = &item.doc {
            docs.push(doc.span);
        }
        match &item.kind {
            ItemKind::Object { members, .. } | ItemKind::Private { items: members } => {
                attached(members, docs);
            }
            ItemKind::Enum {
                variants, members, ..
            } => {
                docs.extend(
                    variants
                        .iter()
                        .filter_map(|variant| variant.doc.as_ref().map(|doc| doc.span)),
                );
                attached(members, docs);
            }
            ItemKind::Conditional(conditional) => {
                for arm in &conditional.arms {
                    attached(&arm.items, docs);
                }
                if let Some(els) = &conditional.els {
                    attached(els, docs);
                }
            }
            _ => {}
        }
    }
}

#[test]
fn every_standard_doc_comment_documents_a_declaration() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lib/Standard");
    let mut files = Vec::new();
    lilac_files(&root, &mut files);
    assert!(!files.is_empty(), "no files under {}", root.display());
    let mut stray = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path).expect("read file");
        let mut lexer = Lexer::new(&text, FileId(0));
        let tokens: Vec<_> = lexer.by_ref().filter_map(Result::ok).collect();
        let comments = lexer.take_comments();
        let mut diags = Diags::new();
        let program = Parser::new(tokens, FileId(0))
            .parse_program(&mut diags)
            .expect("parses");
        let mut docs = Vec::new();
        attached(&program.items, &mut docs);
        for comment in comments
            .iter()
            .filter(|comment| comment.kind == CommentKind::Doc)
        {
            let inside = docs
                .iter()
                .any(|doc| doc.lo.0 <= comment.span.lo.0 && comment.span.hi.0 <= doc.hi.0);
            if !inside {
                let line = text[..comment.span.lo.0 as usize].matches('\n').count() + 1;
                stray.push(format!("{}:{line}", path.display()));
            }
        }
    }
    assert!(
        stray.is_empty(),
        "`///` documenting nothing:\n{}",
        stray.join("\n")
    );
}
