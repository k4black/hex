//! `hex init`: the project's `.hex/` layout, created idempotently.
//!
//! The layout is the runtime's knowledge (it reads `.hex/config.yaml`, writes
//! `.hex/runs/`, leases `.hex/worktrees/`), so it is created here and a client
//! only renders what was made.

use std::path::Path;

use crate::error::{HexError, Result};

/// The starter `.hex/config.yaml`. Every key is commented out: the built-in layer
/// (`hex-runtime/src/defaults.yaml`) already supplies working workers and roles,
/// so an uncommented copy of them here would freeze this machine's defaults into
/// the repository and stop deep-merge doing its job.
const CONFIG_TEMPLATE: &str = "\
# hex project configuration.
#
# This is the last of three layers: the built-in defaults (embedded in the `hex`
# binary), then `~/.config/hex/config.yaml`, then this file. Layers deep-merge per
# key, so setting `roles.reviewer.model` here keeps the built-in worker, effort,
# read_only and prompt. Run `hex doctor` to see what the merged result resolves to.

# Project checks: name → argv. **Deliberately empty.**
#
# What \"green\" means is your decision, so hex autodetects nothing and ships no
# commands. Declare a check here and a graph can gate on it as
# `command: { check: test }`; naming an undeclared check is refused before the run
# starts, rather than passing silently. Two built-in presets (`tdd`,
# `implement-until-green`) are a gate, so they need `test` declared.
#
#   checks:
#     test: [cargo, test, --workspace]
#     lint: [cargo, clippy, --workspace, --all-targets]
checks: {}

# Roles are what a graph names (`role: reviewer`). Each binds a worker CLI to a
# model, a reasoning effort, a read-only policy, and a prompt preamble. Override
# only what should differ from the built-in layer; `prompt_append` extends the
# inherited preamble, `prompt` replaces it.
#
#   roles:
#     reviewer:
#       prompt_append: |
#         This repository's invariants are in AGENTS.md — read it before judging a
#         design choice.

# Workers are the CLI adapters behind a role — internal plumbing a graph never
# names directly. `kind` picks the adapter: codex | claude | opencode | command.
#
#   workers:
#     codex:
#       kind: codex
";

/// What `hex init` made and what was already there, as root-relative names
/// (`.hex/`, `.hex/config.yaml`, `.gitignore:.hex/runs/`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct InitReport {
    /// Entries this call created.
    pub created: Vec<String>,
    /// Entries that already existed and were left untouched.
    pub existed: Vec<String>,
}

/// Set `root` up for hex.
///
/// Idempotent by construction: every step reports `created` or `existed` and an
/// existing `.hex/config.yaml` is never rewritten — the operator's checks and role
/// overrides are exactly the content a second `hex init` must not be able to lose.
///
/// # Errors
/// Fails if a directory, the config, or `.gitignore` cannot be read or written.
pub fn init(root: &Path) -> Result<InitReport> {
    let mut report = InitReport::default();
    for name in [".hex/", ".hex/graphs/"] {
        let dir = root.join(name);
        if dir.is_dir() {
            report.existed.push(name.to_owned());
        } else {
            std::fs::create_dir_all(&dir)
                .map_err(|e| HexError::new(format!("cannot create {name}: {e}")))?;
            report.created.push(name.to_owned());
        }
    }

    let config = ".hex/config.yaml";
    if root.join(config).exists() {
        report.existed.push(config.to_owned());
    } else {
        std::fs::write(root.join(config), CONFIG_TEMPLATE)
            .map_err(|e| HexError::new(format!("cannot write {config}: {e}")))?;
        report.created.push(config.to_owned());
    }

    // A run's journal and a worktree slot are machine-local working state, not
    // source.
    let lines = [".hex/runs/", ".hex/worktrees/"];
    let added = gitignore(root, &lines)?;
    for line in lines {
        let list = if added.contains(&line) {
            &mut report.created
        } else {
            &mut report.existed
        };
        list.push(format!(".gitignore:{line}"));
    }
    Ok(report)
}

/// Append each of `lines` missing from `<root>/.gitignore`, returning the ones
/// added. Shared by `init` and worktree isolation.
///
/// Bytes, not a `String`, and a read failure is fatal rather than "empty".
/// Treating an unreadable file as empty and then writing our lines over it
/// deletes whatever it held — a `.gitignore` with one non-UTF-8 byte in a
/// comment, or one we lack permission to read, was silently truncated. Appending
/// raw bytes also preserves the original exactly, and the file is only written
/// when something is missing, so a second call leaves it byte-for-byte (mtime
/// included) as it was.
///
/// # Errors
/// Fails if `.gitignore` exists but cannot be read, or cannot be written.
pub(crate) fn gitignore<'a>(root: &Path, lines: &[&'a str]) -> Result<Vec<&'a str>> {
    let path = root.join(".gitignore");
    let mut current = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(HexError::new(format!("cannot read .gitignore: {e}"))),
    };
    let needed: Vec<&str> = {
        let existing = String::from_utf8_lossy(&current);
        lines
            .iter()
            .copied()
            .filter(|line| !existing.lines().any(|l| l.trim() == *line))
            .collect()
    };
    for line in &needed {
        // A file whose last line has no terminator would otherwise get our entry
        // glued onto it, silently ignoring both patterns.
        if !current.is_empty() && !current.ends_with(b"\n") {
            current.push(b'\n');
        }
        current.extend_from_slice(line.as_bytes());
        current.push(b'\n');
    }
    if !needed.is_empty() {
        std::fs::write(&path, &current)
            .map_err(|e| HexError::new(format!("cannot write .gitignore: {e}")))?;
    }
    Ok(needed)
}
