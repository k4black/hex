//! `hex-mcp` — Model Context Protocol adapter (later / after-demand).
//!
//! MCP must be a *thin transport* over the same control protocol every other
//! surface uses: it maps MCP tool calls onto [`hex_proto::Command`] and streams
//! [`hex_proto::Event`]s back out. It contains **no** orchestration logic and
//! must never duplicate the engine — that is the explicit lesson from the
//! reference tools (AWS CAO's MCP-as-front-end, Ruflo's "one machine API behind
//! both CLI and MCP").
//!
//! Living in its own crate keeps the MCP server SDK dependency out of the core
//! and CLI, and structurally guarantees this layer can only reach for
//! [`hex_proto`] — never engine internals.
//!
//! Status: stub. The MCP server SDK is added when this is actually built.

use hex_proto::Command;

/// Placeholder mapping from an MCP tool name to a protocol [`Command`].
///
/// The real adapter validates arguments and scopes authority per caller.
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
