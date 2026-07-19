//! `hex-mcp` — Model Context Protocol transport (later).
//!
//! A thin [`hex_runtime::RuntimeClient`] client, peer of the CLI: it maps MCP
//! tool calls onto protocol [`Command`]s and streams events back out. A
//! transport/projection, **never** orchestration — it contains no scheduling
//! or acceptance logic and must never duplicate the kernel. Same verbs as the
//! CLI; MCP callers can start and control runs with per-actor scoped
//! authority.
//!
//! Living in its own crate keeps the MCP server SDK dependency out of the
//! kernel and CLI, and structurally guarantees this layer can only reach the
//! runtime through `RuntimeClient` — never kernel internals.
//!
//! Status: stub. The MCP server SDK is added when this is actually built.

use hex_runtime::Command;

/// Placeholder mapping from an MCP tool name to a protocol [`Command`].
///
/// The real transport validates arguments, scopes authority per caller, and
/// forwards to a [`hex_runtime::RuntimeClient`].
#[must_use]
pub fn map_tool_call(tool: &str) -> Option<Command> {
    match tool {
        "status" => Some(Command::Status),
        "pause" => Some(Command::Pause),
        "resume" => Some(Command::Resume),
        "cancel" => Some(Command::Cancel),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_tool() {
        assert_eq!(map_tool_call("status"), Some(Command::Status));
    }

    #[test]
    fn rejects_unknown_tool() {
        assert_eq!(map_tool_call("definitely-not-a-tool"), None);
    }
}
