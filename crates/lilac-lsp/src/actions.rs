//! Quick fixes: `textDocument/codeAction` over the fixes a published
//! diagnostic carries in its `data` (`convert::fixes`), so no compile is
//! consulted.

use std::collections::HashMap;
use std::path::PathBuf;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, TextEdit, Url, WorkspaceEdit,
};

use crate::navigate::library_of;
use crate::roots::url_to_path;

/// One quick fix per fix of each diagnostic in the request's context. A fix
/// that edits a mounted library other than the one holding the document
/// (`libraries`) is not offered.
#[must_use]
pub fn code_actions(
    params: &CodeActionParams,
    libraries: &[(String, PathBuf)],
) -> Vec<CodeActionOrCommand> {
    let wanted = params.context.only.as_ref().is_none_or(|only| {
        only.iter()
            .any(|kind| CodeActionKind::QUICKFIX.as_str().starts_with(kind.as_str()))
    });
    if !wanted {
        return Vec::new();
    }
    let library = |uri: &Url| url_to_path(uri).and_then(|path| library_of(&path, libraries));
    let home = library(&params.text_document.uri);
    let mut actions = Vec::new();
    for diagnostic in &params.context.diagnostics {
        let Some(fixes) = diagnostic
            .data
            .as_ref()
            .and_then(|data| data.get("fixes"))
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for fix in fixes {
            let Some(uri) = fix
                .get("uri")
                .and_then(serde_json::Value::as_str)
                .and_then(|uri| Url::parse(uri).ok())
            else {
                continue;
            };
            let Some(edit) = fix
                .get("edit")
                .and_then(|edit| serde_json::from_value::<TextEdit>(edit.clone()).ok())
            else {
                continue;
            };
            // The fix is for the document the diagnostic was published on.
            let uri = if crate::roots::canonical_file_uri(&uri)
                == crate::roots::canonical_file_uri(&params.text_document.uri)
            {
                params.text_document.uri.clone()
            } else {
                uri
            };
            if library(&uri).is_some_and(|library| home.as_ref() != Some(&library)) {
                continue;
            }
            let title = fix
                .get("title")
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| format!("Change to `{}`", edit.new_text), str::to_owned);
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title,
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(vec![diagnostic.clone()]),
                edit: Some(WorkspaceEdit {
                    changes: Some(HashMap::from([(uri, vec![edit])])),
                    ..WorkspaceEdit::default()
                }),
                is_preferred: Some(true),
                ..CodeAction::default()
            }));
        }
    }
    actions
}
