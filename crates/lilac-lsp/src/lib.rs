//! Lilac language server: lifecycle and message loop.

use std::error::Error;

use lsp_server::Connection;
use lsp_types::{
    HoverProviderCapability, InitializeParams, OneOf, ServerCapabilities, ServerInfo,
    TextDocumentSyncCapability, TextDocumentSyncKind,
};

pub mod convert;
pub mod documents;
pub mod fs;
pub mod handlers;
pub mod index;
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
        definition_provider: Some(OneOf::Left(true)),
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

    let root = roots::root_from_params(&params);
    let mut server = server::Server::new(connection, root);
    server.main_loop()
}
