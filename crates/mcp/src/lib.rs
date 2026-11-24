//! MCP server for Axel — agent and thread management tools.
//!
//! This crate follows the same architecture as `pay-mcp`: a thin MCP
//! dispatch layer that delegates to shared library code (`axel-store`,
//! `axel-identity`, `axel-config`).  Both the CLI (`axel agent new`) and
//! the MCP tools (`create_agent`) call the same store and identity
//! functions.
//!
//! # Usage
//!
//! ```text
//! axel mcp          # starts the MCP server on stdio
//! ```
//!
//! Or add to your MCP client config:
//!
//! ```json
//! { "command": "axel", "args": ["mcp"] }
//! ```

pub mod context;
mod server;
mod tools;

pub use context::{AxelContext, BlockedKind, CallScope, Delivery, Grant, InboxMessage, LocalContext, MessageKind, Outcome, SendError, SendReport, StatusState};
pub use server::AxelMcp;
pub use rmcp::ErrorData;

use rmcp::ServiceExt;
use rmcp::transport::async_rw::AsyncRwTransport;

/// The longest JSON-RPC line read from stdin; a longer one is dropped (and
/// logged by rmcp) without being buffered. 16 MiB is rmcp's own default
/// (`DEFAULT_MAX_LINE_LENGTH`), named here so the bound is visible: the
/// largest request we take is a `send` whose text is a prompt, orders of
/// magnitude smaller.
const REQUEST_LINE_BYTES_MAX: usize = 16 * 1024 * 1024;

/// Start the MCP server on stdio, backed by the local store.
pub async fn run_server() -> Result<(), String> {
    run_server_with(std::sync::Arc::new(LocalContext)).await
}

/// Start the MCP server on stdio with `context` (the `axel` binary passes
/// one that delivers messages and records status like the CLI).
pub async fn run_server_with(context: std::sync::Arc<dyn AxelContext>) -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!("Starting axel MCP server");

    let transport = AsyncRwTransport::new(tokio::io::stdin(), tokio::io::stdout())
        .with_max_line_length(REQUEST_LINE_BYTES_MAX);
    let service = AxelMcp::with_context(context)
        .serve(transport)
        .await
        .inspect_err(|e| {
            tracing::error!("serving error: {:?}", e);
        })
        .map_err(|e| e.to_string())?;

    service
        .waiting()
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}
