//! `read_inbox` — messages addressed to the caller.

use rmcp::model::CallToolResult;
use rmcp::schemars;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::context::AxelContext;

/// Input parameters for the `read_inbox` tool.
/// Messages with `handled: true` already reached the caller as prompts or
/// notes on their thread: they're history, not requests.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Params {
    #[schemars(description = "How many of the newest messages to return (default 20, at most 100)")]
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Messages returned when the caller names no limit: a screenful.
const MESSAGES_DEFAULT: usize = 20;
/// The most messages one call returns, whatever the caller asks, so a reply
/// stays a size an agent can read in one go.
const MESSAGES_MAX: usize = 100;

pub fn run(params: Params, ctx: &dyn AxelContext) -> Result<CallToolResult, rmcp::ErrorData> {
    let limit = params.limit.unwrap_or(MESSAGES_DEFAULT).clamp(1, MESSAGES_MAX);
    match ctx.inbox(limit) {
        Ok(messages) => super::json_success(&messages),
        Err(e) => Ok(super::tool_error(e)),
    }
}
