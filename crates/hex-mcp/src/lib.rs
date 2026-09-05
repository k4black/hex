//! `hex-mcp` — Model Context Protocol transport (later).
//!
//! A thin [`hex_runtime::Runtime`] client, peer of the CLI: it maps MCP
//! tool calls onto protocol [`hex_runtime::Command`]s and streams events back
//! out. A transport/projection, **never** orchestration — it contains no
//! scheduling or acceptance logic and must never duplicate the kernel. Same
//! verbs as the CLI; MCP callers can start and control runs with per-actor
//! scoped authority.
//!
//! Living in its own crate keeps the MCP server SDK dependency out of the
//! kernel and CLI, and structurally guarantees this layer can only reach the
//! runtime through the `Runtime` API — never kernel internals.
//!
//! Status: stub. The MCP server SDK — and this crate's first line of real
//! code — are added when this is actually built.
