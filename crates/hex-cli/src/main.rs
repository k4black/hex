//! `hex` — a thin, deterministic control plane for agentic loops and graphs.
//!
//! This binary is the operator surface. A human at a TTY and an orchestrating
//! agent share the same commands; every action becomes a protocol event.
//!
//! Status: scaffold — prints the planned command surface. Argument parsing
//! (clap) and the commands themselves land in the first real implementation.

use hex_backend::Backend;
use hex_backend::mock::MockBackend;
use hex_core::protocol_version;
use hex_engine::Scheduler;
use hex_proto::PROTOCOL_VERSION;

fn main() {
    // Touch each layer so the full dependency graph is compiler-enforced even
    // while everything is a stub.
    let _scheduler = Scheduler::new();
    let _capabilities = MockBackend.capabilities();
    debug_assert_eq!(protocol_version(), PROTOCOL_VERSION);

    println!(
        "hex {} (protocol v{PROTOCOL_VERSION}) — scaffold",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("example commands (planned — full roadmap in TODO.md):");
    for line in [
        "  hex validate <graph>       check schema, references, bounded cycles",
        "  hex graph <graph>          render the graph (ascii/mermaid/dot)",
        "  hex run <graph>            execute a run",
        "  hex status <run>           projected run status",
        "  hex watch <run>            stream events (ndjson with --json)",
        "  hex pause|resume|cancel    operator control",
        "  hex step <run>             run exactly one ready attempt",
        "  hex emit <event>           agent-side scoped structured control",
        "  hex approve|reject <req>   human decisions on blocking gates",
    ] {
        println!("{line}");
    }
}
