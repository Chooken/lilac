//! Lilac language server: lifecycle and message loop.

use std::error::Error;

use lsp_server::{Connection, Message, Response};
use lsp_types::{
    InitializeParams, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind,
};

/// JSON-RPC code for an unknown method.
const METHOD_NOT_FOUND: i32 = -32601;

/// Runs the server over `connection` until the client sends `exit`.
///
/// Returns `Ok(())` when `exit` follows a `shutdown`, and an error when `exit`
/// arrives first (the client violated the protocol).
pub fn run(connection: Connection) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (id, params) = connection.initialize_start()?;
    let _params: InitializeParams = serde_json::from_value(params)?;

    let capabilities = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
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

    let mut shutdown = false;
    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if request.method == "shutdown" {
                    shutdown = true;
                    connection.sender.send(Message::Response(Response::new_ok(
                        request.id,
                        serde_json::Value::Null,
                    )))?;
                } else {
                    let method = request.method;
                    connection.sender.send(Message::Response(Response::new_err(
                        request.id,
                        METHOD_NOT_FOUND,
                        format!("method not found: {method}"),
                    )))?;
                }
            }
            Message::Notification(notification) => {
                if notification.method == "exit" {
                    return if shutdown {
                        Ok(())
                    } else {
                        Err("exit received before shutdown".into())
                    };
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(())
}
