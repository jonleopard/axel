//! `get_agent` — full details for a single agent.

use rmcp::model::CallToolResult;
use rmcp::schemars;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::context::CallScope;

/// Input parameters for the `get_agent` tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Params {
    /// Agent name or base58 public key.
    #[schemars(
        description = "Agent name (e.g. \"atlas\") or base58-encoded Ed25519 public key."
    )]
    pub agent: String,
}

/// Full agent details returned to the MCP client.
#[derive(Debug, Serialize, JsonSchema)]
pub struct AgentDetails {
    /// Agent display name.
    #[schemars(description = "Agent display name")]
    pub name: String,
    /// Base58-encoded Ed25519 public key.
    #[schemars(description = "Base58-encoded Ed25519 public key")]
    pub pubkey: String,
    /// Harness this agent uses.
    #[schemars(description = "Harness")]
    pub harness: String,
    /// Model override, if configured.
    #[schemars(description = "Model override")]
    pub model: Option<String>,
    /// Reasoning effort, if configured: low, medium, high, xhigh, or max.
    #[schemars(description = "Reasoning effort; null means the harness's default")]
    pub effort: Option<String>,
    /// Enabled skill names.
    #[schemars(description = "Enabled skill names")]
    pub skills: Vec<String>,
    /// System prompt.
    #[schemars(description = "System prompt")]
    pub prompt: Option<String>,
    /// TUI colour.
    #[schemars(description = "TUI colour")]
    pub color: Option<String>,
    /// Runtime mode.
    #[schemars(description = "Runtime mode")]
    pub runtime_mode: String,
    /// Whether the signing key is present in this machine's keystore.
    #[schemars(description = "True if the agent's signing key is in this machine's keystore")]
    pub key_in_keystore: bool,
    /// Whether the owner attestation is valid.
    #[schemars(description = "Owner attestation validity: \"valid\", \"invalid\", or \"none\"")]
    pub attestation: Attestation,
    /// Owner public key from the attestation, if present.
    #[schemars(description = "Owner public key from the attestation tag")]
    pub attestation_owner: Option<String>,
    /// Whether the agent is archived.
    #[schemars(description = "True if the agent is archived")]
    pub archived: bool,
    /// Unix timestamp when the agent was created.
    #[schemars(description = "Unix timestamp of agent creation")]
    pub created_at: i64,
}

/// What the agent's owner attestation proves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Attestation {
    /// Signed by the owner it names, for this agent's key.
    Valid,
    /// Present, but unreadable or its signature doesn't check out.
    Invalid,
    /// The agent carries no attestation.
    None,
}

pub fn run(params: Params, scope: &CallScope) -> Result<CallToolResult, rmcp::ErrorData> {
    let query = params.agent.trim();
    let store_error = |e: axel_store::Error| rmcp::ErrorData::internal_error(format!("store error: {e}"), None);

    // Try by name first, then by pubkey.
    let agent = match scope.store.agent_by_name(query).map_err(store_error)? {
        Some(agent) => Some(agent),
        None => scope.store.agent_by_pubkey(query).map_err(store_error)?,
    };

    let Some(agent) = agent else {
        return Ok(super::tool_error(format!(
            "No agent found matching \"{query}\"."
        )));
    };

    let key_in_keystore = key_in_keystore(scope, &agent.pubkey);
    let (attestation, attestation_owner) = attestation_of(&agent);

    let details = AgentDetails {
        name: agent.name,
        pubkey: agent.pubkey,
        harness: agent.harness.to_string(),
        model: agent.model,
        effort: agent.effort.map(|e| e.to_string()),
        skills: agent.skills,
        prompt: agent.prompt,
        color: agent.color.map(|c| c.to_string()),
        runtime_mode: agent.runtime_mode.to_string(),
        key_in_keystore,
        attestation,
        attestation_owner,
        archived: agent.archived_at.is_some(),
        created_at: agent.created_at,
    };

    super::json_success(&details)
}

/// Whether this machine's keystore holds the signing key for `pubkey`.
/// A report, not a gate: a pubkey that doesn't parse can't be in the
/// keystore, and a keystore that can't be read (a locked keychain, say)
/// reads as not holding it rather than failing the whole lookup.
fn key_in_keystore(scope: &CallScope, pubkey: &str) -> bool {
    let Ok(pubkey) = pubkey.parse() else { return false };
    matches!(scope.keystore.agent(&pubkey), Ok(Some(_)))
}

/// The agent's attestation checked against its key, and the owner it names.
fn attestation_of(agent: &axel_store::Agent) -> (Attestation, Option<String>) {
    let Some(tag) = &agent.auth_tag else { return (Attestation::None, None) };
    let Ok(auth_tag) = axel_identity::AuthTag::from_tag(tag) else { return (Attestation::Invalid, None) };
    let owner = auth_tag.owner.to_string();
    // A pubkey that doesn't parse can't be the key the owner signed.
    let is_valid = match agent.pubkey.parse() {
        Ok(pubkey) => axel_identity::verify_attestation(&auth_tag, &pubkey).is_ok(),
        Err(_) => false,
    };
    let attestation = if is_valid { Attestation::Valid } else { Attestation::Invalid };
    (attestation, Some(owner))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_deserializes() {
        let json = r#"{"agent": "atlas"}"#;
        let params: Params = serde_json::from_str(json).unwrap();
        assert_eq!(params.agent, "atlas");
    }
}
