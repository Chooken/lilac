//! Quick fixes: `textDocument/codeAction` over the fixes a published
//! diagnostic carries in its `data` (`convert::fixes`), so no compile is
//! consulted.

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, TextEdit, Url, WorkspaceEdit,
};

/// One quick fix per fix of each diagnostic in the request's context.
#[must_use]
pub fn code_actions(params: &CodeActionParams) -> Vec<CodeActionOrCommand> {
    let wanted = params.context.only.as_ref().is_none_or(|only| {
        only.iter()
            .any(|kind| CodeActionKind::QUICKFIX.as_str().starts_with(kind.as_str()))
    });
    if !wanted {
        return Vec::new();
    }
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
            actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Change to `{}`", edit.new_text),
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
