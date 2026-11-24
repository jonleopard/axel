//! MCP tool modules — one per tool, each with a `Params` struct and a `run()` function.

pub mod create_agent;
pub mod get_agent;
pub mod list_agents;
pub mod read_inbox;
pub mod send;
pub mod set_status;

/// Build a `CallToolResult` that carries an error flag the MCP client can
/// inspect, while still rendering the message as visible text.
pub(crate) fn tool_error(message: impl Into<String>) -> rmcp::model::CallToolResult {
    rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(message.into())])
}

/// `value` as pretty JSON. The tool results are plain structs of strings,
/// numbers and lists, so this only fails on a bug; it is still reported as
/// an internal error rather than sent as an empty `{}`.
pub(crate) fn to_json(value: &impl serde::Serialize) -> Result<String, rmcp::ErrorData> {
    serde_json::to_string_pretty(value)
        .map_err(|e| rmcp::ErrorData::internal_error(format!("serializing the result: {e}"), None))
}

/// A successful result whose text is `value` as pretty JSON.
pub(crate) fn json_success(value: &impl serde::Serialize) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
    Ok(rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(to_json(value)?)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_error_returns_visible_error_content() {
        let result = tool_error("visible failure");

        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.content[0].as_text().unwrap().text, "visible failure");
    }
}
