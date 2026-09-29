//! The usage `stats` log — a user-global, cross-repo record of what hex was
//! asked to do.
//!
//! Per-run detail (usage, spend, per-attempt evidence) deliberately does NOT
//! live here: that is the run journal's job, and `hex status`/`logs` already fold
//! it. The journal is per-project and `hex prune` deletes it, though, so it
//! cannot answer "which graphs and verbs do I use across my repos". That — and
//! only that — is what this file holds.
//!
//! Append-only JSONL, folded on read by [`fold`]. No counters, no database: a
//! derived number stored in a second place is a number that can disagree with
//! the first (the journal-is-authoritative rule).

use std::collections::BTreeMap;
use std::path::Path;

use crate::Disposition;

/// The log file's name under `~/.hex/`.
pub const FILE: &str = "stats.jsonl";

/// A failed stats write warns on stderr and nothing more — telemetry must never
/// fail a command.
fn warn_on_err(result: Result<(), String>) {
    if let Err(e) = result {
        eprintln!("warning: could not record stats: {e}");
    }
}

/// Append one `run` line. Called at `RunFinished`, so it is written whether or
/// not any client ever observes the run. A write failure warns on stderr:
/// telemetry must never fail a command.
pub fn record_run(
    project_root: &Path,
    graph: &str,
    origin: &str,
    branch: Option<&str>,
    disposition: Disposition,
    state: &hex_kernel::RunState,
) {
    let value = serde_json::json!({
        "kind": "run",
        "ts_ms": crate::journal::now_ms(),
        "hex_version": env!("CARGO_PKG_VERSION"),
        "repo": crate::local_log::canonical(project_root),
        "graph": graph,
        // `built-in:<name>` or a path — the resolved origin, so stats can split
        // shipped presets from the project's own graphs.
        "origin": origin,
        // A branch is present only for a worktree run, so it *is* the isolation.
        "isolation": if branch.is_some() { "worktree" } else { "shared" },
        "branch": branch,
        "disposition": disposition.as_str(),
        "attempts": state.attempts_total,
        // Which nodes/roles ran, and how many times — the one journal-shaped fact
        // worth keeping across repos, at one number per node.
        "visits": state.visits,
    });
    warn_on_err(crate::local_log::append(FILE, &value));
}

/// Append one `cli` line for a verb invocation. `source` is the caller class:
/// `subgraph` (called from inside an attempt), `interactive` (a terminal), or
/// `non-interactive` (an agent or script).
pub fn record_cli(verb: &str, source: &str, repo: &Path) {
    let value = serde_json::json!({
        "kind": "cli",
        "ts_ms": crate::journal::now_ms(),
        "hex_version": env!("CARGO_PKG_VERSION"),
        "verb": verb,
        "source": source,
        "repo": crate::local_log::canonical(repo),
    });
    warn_on_err(crate::local_log::append(FILE, &value));
}

/// How many times a graph ran, and how many of those were a project graph rather
/// than a shipped preset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct GraphUse {
    /// Total runs of this graph.
    pub runs: u64,
    /// Runs whose origin was not `built-in:`.
    pub custom: u64,
}

/// Everything `hex stats` shows, folded from the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Aggregate {
    /// Lines successfully parsed.
    pub lines: u64,
    /// `cli` lines by verb.
    pub verbs: BTreeMap<String, u64>,
    /// Runs by graph name.
    pub graphs: BTreeMap<String, GraphUse>,
    /// Lines (cli and run) by repository.
    pub repos: BTreeMap<String, u64>,
    /// Finished runs by disposition.
    pub dispositions: BTreeMap<String, u64>,
    /// Attempts by node — "which roles ran, and how often".
    pub nodes: BTreeMap<String, u64>,
    /// Runs under worktree isolation, and runs in the shared workspace.
    pub worktree_runs: u64,
    /// See [`Aggregate::worktree_runs`].
    pub shared_runs: u64,
}

/// Fold `~/.hex/stats.jsonl` (or a test path) into an [`Aggregate`].
///
/// A missing file is an empty aggregate, not an error — a fresh machine has no
/// stats. A malformed line is skipped: one bad write must not hide every other
/// line.
///
/// # Errors
/// Fails only if the file exists and cannot be read.
pub fn fold(path: &Path) -> Result<Aggregate, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Aggregate::default()),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let mut agg = Aggregate::default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        agg.lines += 1;
        if let Some(repo) = v["repo"].as_str() {
            *agg.repos.entry(repo.to_owned()).or_default() += 1;
        }
        match v["kind"].as_str() {
            Some("cli") => {
                if let Some(verb) = v["verb"].as_str() {
                    *agg.verbs.entry(verb.to_owned()).or_default() += 1;
                }
            }
            Some("run") => {
                if let Some(graph) = v["graph"].as_str() {
                    let use_ = agg.graphs.entry(graph.to_owned()).or_default();
                    use_.runs += 1;
                    if !v["origin"]
                        .as_str()
                        .unwrap_or_default()
                        .starts_with("built-in:")
                    {
                        use_.custom += 1;
                    }
                }
                if let Some(d) = v["disposition"].as_str() {
                    *agg.dispositions.entry(d.to_owned()).or_default() += 1;
                }
                if v["isolation"].as_str() == Some("worktree") {
                    agg.worktree_runs += 1;
                } else {
                    agg.shared_runs += 1;
                }
                if let Some(visits) = v["visits"].as_object() {
                    for (node, n) in visits {
                        *agg.nodes.entry(node.clone()).or_default() += n.as_u64().unwrap_or(0);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(agg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique file path for one test (`test_support::temp_dir` + a filename).
    fn temp(tag: &str) -> std::path::PathBuf {
        crate::test_support::temp_dir(&format!("stats-{tag}")).join("stats.jsonl")
    }

    #[test]
    fn a_missing_file_folds_to_an_empty_aggregate() {
        let agg = fold(&temp("missing")).expect("missing is not an error");
        assert_eq!(agg.lines, 0);
        assert!(agg.verbs.is_empty());
    }

    #[test]
    fn fold_counts_verbs_graphs_visits_and_isolation() {
        let path = temp("fold");
        let cli = serde_json::json!({"kind":"cli","verb":"run","repo":"/r"});
        let run = serde_json::json!({
            "kind":"run","repo":"/r","graph":"checklist","origin":"built-in:checklist",
            "isolation":"worktree","disposition":"failed","visits":{"implement":3,"review":2},
        });
        let custom = serde_json::json!({
            "kind":"run","repo":"/other","graph":"mine","origin":"/p/mine.yaml",
            "isolation":"shared","disposition":"succeeded","visits":{"implement":1},
        });
        let body = format!("{cli}\n{run}\n{custom}\n");
        std::fs::write(&path, body).expect("write");
        let agg = fold(&path).expect("folds");
        assert_eq!(agg.lines, 3);
        assert_eq!(agg.verbs.get("run"), Some(&1));
        assert_eq!(agg.graphs["checklist"].runs, 1);
        assert_eq!(agg.graphs["checklist"].custom, 0, "built-in is not custom");
        assert_eq!(agg.graphs["mine"].custom, 1);
        assert_eq!(agg.nodes.get("implement"), Some(&4));
        assert_eq!(agg.worktree_runs, 1);
        assert_eq!(agg.shared_runs, 1);
        assert_eq!(agg.dispositions.get("failed"), Some(&1));
        assert_eq!(agg.repos.get("/r"), Some(&2));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_malformed_line_is_skipped_not_fatal() {
        let path = temp("bad");
        std::fs::write(&path, "not json\n{\"kind\":\"cli\",\"verb\":\"status\"}\n").expect("write");
        let agg = fold(&path).expect("folds");
        assert_eq!(agg.lines, 1, "the bad line is skipped");
        assert_eq!(agg.verbs.get("status"), Some(&1));
        let _ = std::fs::remove_file(&path);
    }
}
