//! `hex feedback` — record a note about hex to the user-global feedback log.
//!
//! Any operator or agent can run `hex feedback "<message>"` to log an issue or a
//! missing capability. It appends one JSON line to `~/.hex/feedback.jsonl` and
//! auto-captures the context the runtime injected into the agent's environment
//! (`HEX_RUN_ID`/`HEX_NODE_ID`/`HEX_GRAPH`/`HEX_AGENT`/`HEX_PROJECT_ROOT`) plus
//! the working directory and the time. It needs no `.hex/` project and no
//! Runtime — only `$HOME`, the environment and the cwd — so it works the same
//! from inside a worktree slot, from another project, or from a bare shell.
//!
//! The schema is fixed: every line carries the same keys, with `null` where the
//! context was absent (a call made outside a run), so a later consumer can read
//! the log with one shape and tell "no run" from "empty run id".

use std::process::ExitCode;

/// A non-empty environment variable, or `None` — an unset *or* empty var both
/// mean "no context".
fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Append one feedback line to `~/.hex/feedback.jsonl`, creating the directory
/// and file if needed.
///
/// # Errors
/// Fails if the message is empty, `$HOME` is unset, or the file cannot be
/// created/appended to.
pub fn record(message: &str, kind: Option<&str>) -> Result<ExitCode, String> {
    if message.trim().is_empty() {
        return Err("feedback message is empty".to_owned());
    }
    let path = hex_runtime::local_log::path("feedback.jsonl")?;

    let ts_ms = hex_runtime::journal::now_ms();
    let cwd = std::env::current_dir().unwrap_or_default();
    // `location` is the durable place to debug from: the real project root
    // inside a run, else the cwd. `workdir` is where the attempt physically ran
    // — under worktree isolation the reclaimable slot, which must not stand in
    // for the real location. Both canonicalized so a reader gets a resolvable
    // absolute path, not a symlink or a relative fragment.
    let location = env_opt("HEX_PROJECT_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| cwd.clone());
    let location = hex_runtime::local_log::canonical(&location);
    let workdir = hex_runtime::local_log::canonical(&cwd);
    let project = std::path::Path::new(&location)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());

    let entry = serde_json::json!({
        "ts_ms": ts_ms,
        "hex_version": env!("CARGO_PKG_VERSION"),
        "kind": kind,
        "message": message,
        "project": project,
        // The real project root — `cd` here to debug.
        "location": location,
        // Where the attempt ran (a worktree slot under isolation, else = location).
        "workdir": workdir,
        // For a worktree run, the branch holding the code: `git checkout` it in
        // `location`. `null` for a shared-workspace run.
        "branch": env_opt("HEX_WORKTREE_BRANCH"),
        "run_id": env_opt("HEX_RUN_ID"),
        "node": env_opt("HEX_NODE_ID"),
        "graph": env_opt("HEX_GRAPH"),
        "agent": env_opt("HEX_AGENT"),
    });
    // One shared append helper with the usage `stats` log: create the dir, one
    // `write_all` under `O_APPEND`. Feedback surfaces a failure; stats ignores
    // one, because telemetry must never fail a command.
    hex_runtime::local_log::append("feedback.jsonl", &entry)
        .map_err(|e| format!("could not write feedback: {e}"))?;

    // Diagnostic to stderr; stdout stays clean for a machine caller.
    eprintln!("recorded feedback → {}", path.display());
    Ok(ExitCode::SUCCESS)
}
