//! `hex` — a thin, deterministic control plane for agentic loops and graphs.
//!
//! This binary is one operator surface: argument parsing and rendering over
//! the [`hex_runtime::RuntimeClient`] trait, nothing more. A human at a TTY
//! and an agent (via injected `hex emit` or MCP tool hooks) share the same
//! control protocol; every action becomes a protocol event.
//!
//! Status: scaffold — prints the planned command surface. Argument parsing
//! (clap) and the verbs themselves land in the first real implementation.

use hex_runtime::{Command, InProcess, PROTOCOL_VERSION, RuntimeClient};

fn main() {
    // Touch the client layer so the full inward dependency graph stays
    // compiler-enforced even while everything is a stub.
    let mut client = InProcess::default();
    client.submit(Command::Status);
    debug_assert_eq!(client.pending().len(), 1);

    println!(
        "hex {} (protocol v{PROTOCOL_VERSION}) — scaffold",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("planned commands (full roadmap in TODO.md):");
    for line in [
        "  hex validate <graph>       check schema, references, bounded cycles",
        "  hex graph <graph>          render the graph (ascii/mermaid/dot)",
        "  hex run <graph>            start a new run",
        "  hex resume <run>           continue the same run from its journal",
        "  hex pause <run>            stop scheduling new attempts",
        "  hex cancel <run>           cancel the run",
        "  hex status <run>           projected run status",
        "  hex watch <run>            stream events (ndjson with --json)",
        "  hex logs <run>             attempt output and diagnostics",
        "  hex emit <event>           agent-side scoped structured control",
        "  hex respond <run>          answer a human request / steer a session",
    ] {
        println!("{line}");
    }
}
