//! Lilac language server: lifecycle and message loop.

use std::error::Error;

use lsp_server::Connection;
use lsp_types::{
    CodeActionKind, CodeActionOptions, CodeActionProviderCapability, CompletionOptions,
    HoverProviderCapability, InitializeParams, OneOf, RenameOptions, SemanticTokenModifier,
    SemanticTokenType, SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensServerCapabilities, ServerCapabilities, ServerInfo, SignatureHelpOptions,
    TextDocumentSyncCapability, TextDocumentSyncKind, TypeDefinitionProviderCapability,
    WorkDoneProgressOptions,
};

/// Legend token types, in the index order shared by the encoder
/// (`handlers::semantic_tokens`) and the `VSCode` `semanticTokenScopes`.
pub(crate) const TOKEN_TYPES: [SemanticTokenType; 7] = [
    SemanticTokenType::FUNCTION,
    SemanticTokenType::TYPE,
    SemanticTokenType::ENUM,
    SemanticTokenType::PROPERTY,
    SemanticTokenType::ENUM_MEMBER,
    SemanticTokenType::VARIABLE,
    SemanticTokenType::PARAMETER,
];

/// Legend token modifiers (index 0 is `defaultLibrary`).
pub(crate) const TOKEN_MODIFIERS: [SemanticTokenModifier; 1] =
    [SemanticTokenModifier::DEFAULT_LIBRARY];

pub mod actions;
pub mod compile;
pub mod complete;
pub mod convert;
pub mod documents;
pub mod fs;
pub mod handlers;
pub mod hints;
pub mod index;
pub mod lir;
pub mod navigate;
pub mod roots;
pub mod server;

/// Runs the server over `connection` until the client sends `exit`.
///
/// Returns `Ok(())` when `exit` follows a `shutdown`, and an error when `exit`
/// arrives first (the client violated the protocol).
pub fn run(connection: Connection) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (id, params) = connection.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(params)?;

    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_owned(), "@".to_owned()]),
            ..CompletionOptions::default()
        }),
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_owned(), ",".to_owned()]),
            retrigger_characters: None,
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
            code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
            ..CodeActionOptions::default()
        })),
        definition_provider: Some(OneOf::Left(true)),
        type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
        references_provider: Some(OneOf::Left(true)),
        document_highlight_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        workspace_symbol_provider: Some(OneOf::Left(true)),
        rename_provider: Some(OneOf::Right(RenameOptions {
            prepare_provider: Some(true),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        })),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                legend: SemanticTokensLegend {
                    token_types: TOKEN_TYPES.to_vec(),
                    token_modifiers: TOKEN_MODIFIERS.to_vec(),
                },
                range: Some(false),
                full: Some(SemanticTokensFullOptions::Bool(true)),
            },
        )),
        ..ServerCapabilities::default()
    };
    let server_info = ServerInfo {
        name: "lilac-lsp".to_owned(),
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
    };
    connection.initialize_finish(
        id,
        serde_json::json!({ "capabilities": capabilities, "serverInfo": server_info }),
    )?;

    let workspace = roots::root_from_params(&params);
    let libraries = roots::libraries_from_params(&params, workspace.as_deref());
    let watch_files = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.did_change_watched_files.as_ref())
        .and_then(|watched| watched.dynamic_registration)
        .unwrap_or(false);
    let refresh_tokens = params
        .capabilities
        .workspace
        .as_ref()
        .and_then(|workspace| workspace.semantic_tokens.as_ref())
        .and_then(|tokens| tokens.refresh_support)
        .unwrap_or(false);
    let mut server = server::Server::new(connection, workspace, libraries, refresh_tokens);
    if watch_files {
        server.watch_files();
    }
    server.main_loop()
}
