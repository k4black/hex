//! Preflight: is this machine actually able to run what the graph asks for?
//!
//! Two failure modes motivated this module, both of which used to cost a real
//! run before showing themselves:
//!
//! - an agent CLI that is not installed — discovered only after the run dir,
//!   worktree lease and journal were created, as a failed first attempt;
//! - a `checks:` command that cannot start (`cargo` in a Python repo) — which
//!   exits non-zero and is therefore indistinguishable from a genuine test
//!   failure, so the loop routes `failed` back to the implementer and burns the
//!   whole attempt budget on false evidence.
//!
//! `hex doctor` reports both, and [`preflight`] refuses to start a run whose
//! workers are missing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use hex_kernel::graph::{Graph, NodeSpec};

use crate::error::{HexError, Result};
use crate::workers::Workers;

/// One preflight finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// What was probed: `worker` or `check`.
    pub kind: &'static str,
    /// Registry/check name as configured.
    pub name: String,
    /// The executable that would be spawned, if the entry names one.
    pub program: Option<String>,
    /// Where it resolved on `PATH`, when found.
    pub found: Option<PathBuf>,
    /// Whether this entry is usable.
    pub ok: bool,
    /// Human-readable detail (why not, or what was found).
    pub detail: String,
}

/// The full preflight report, in a stable order (workers, then checks).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Every probed entry.
    pub findings: Vec<Finding>,
}

impl Report {
    /// Whether every probed entry is usable.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.findings.iter().all(|f| f.ok)
    }

    /// The names of unusable entries, for a one-line summary.
    #[must_use]
    pub fn broken(&self) -> Vec<&str> {
        self.findings
            .iter()
            .filter(|f| !f.ok)
            .map(|f| f.name.as_str())
            .collect()
    }
}

/// Probe every configured worker and check.
#[must_use]
pub fn report(workers: &Workers, checks: &BTreeMap<String, Vec<String>>) -> Report {
    let mut findings: Vec<Finding> = workers
        .entries()
        .map(|(name, worker)| probe("worker", name, worker.program()))
        .collect();
    findings.extend(checks.iter().map(|(name, argv)| {
        probe("check", name, argv.first().map(String::as_str)).with_argv(argv)
    }));
    Report { findings }
}

/// Probe one named entry's executable.
fn probe(kind: &'static str, name: &str, program: Option<&str>) -> Finding {
    let Some(program) = program else {
        return Finding {
            kind,
            name: name.to_owned(),
            program: None,
            found: None,
            // A worker that spawns nothing (the mock) is fine; it just isn't a
            // binary we can check.
            ok: true,
            detail: "no executable to probe".to_owned(),
        };
    };
    match which(program) {
        Some(path) => Finding {
            kind,
            name: name.to_owned(),
            program: Some(program.to_owned()),
            detail: path.display().to_string(),
            found: Some(path),
            ok: true,
        },
        None => Finding {
            kind,
            name: name.to_owned(),
            program: Some(program.to_owned()),
            found: None,
            ok: false,
            detail: format!("`{program}` not found on PATH"),
        },
    }
}

impl Finding {
    /// Note the full argv in the detail line, so a check reads as what it runs.
    fn with_argv(mut self, argv: &[String]) -> Self {
        if self.ok {
            self.detail = format!("{} ({})", argv.join(" "), self.detail);
        }
        self
    }
}

/// Refuse to start a run whose graph needs a worker that is not installed.
///
/// Only *workers* are fatal. A missing check is reported by `hex doctor` but not
/// fatal here: an unconfigured check is a legitimate state ("by default, no
/// checks"), and a configured-but-missing one still fails as an infrastructure
/// error rather than a false `failed` verdict.
///
/// # Errors
/// Fails when a worker used by `graph` names an executable that is not on `PATH`.
pub fn preflight(graph: &Graph, workers: &Workers) -> Result<()> {
    let mut missing = Vec::new();
    for node in graph.nodes.values() {
        if let NodeSpec::Agent { worker, .. } = &node.spec
            && let Some(adapter) = workers.get(worker)
            && let Some(program) = adapter.program()
            && which(program).is_none()
        {
            missing.push(format!(
                "`{program}` (worker `{worker}`, node `{}`)",
                node.id
            ));
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    missing.sort();
    missing.dedup();
    Err(HexError::new(format!(
        "cannot start: {} not on PATH\nrun `hex doctor` to see what is configured",
        missing.join(", ")
    )))
}

/// Resolve `program` the way a shell would: a path with a separator is used
/// as-is, otherwise each `PATH` entry is tried.
///
/// `std::process::Command` does this internally but offers no way to ask
/// *whether* it would succeed without spawning, which is the whole point here.
#[must_use]
pub fn which(program: &str) -> Option<PathBuf> {
    if program.contains(std::path::MAIN_SEPARATOR) || program.contains('/') {
        let path = Path::new(program);
        return executable(path).then(|| path.to_path_buf());
    }
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths).find_map(|dir| {
        let candidate = dir.join(program);
        executable(&candidate).then_some(candidate)
    })
}

/// Whether `path` is a file we could execute.
fn executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_a_real_binary_and_misses_a_fake_one() {
        assert!(which("sh").is_some(), "sh should be on PATH");
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn which_handles_an_explicit_path() {
        let sh = which("sh").expect("sh on PATH");
        assert_eq!(which(&sh.display().to_string()), Some(sh));
        assert!(which("./definitely-not-here-xyz").is_none());
    }

    #[test]
    fn a_directory_is_not_executable() {
        // Guards the `is_file` check: PATH dirs contain subdirectories.
        assert!(!executable(Path::new("/")));
    }

    #[test]
    fn report_flags_a_missing_check_and_passes_a_real_one() {
        let checks = BTreeMap::from([
            ("good".to_owned(), vec!["sh".to_owned(), "-c".to_owned()]),
            ("bad".to_owned(), vec!["nope-xyz-not-real".to_owned()]),
        ]);
        let report = report(&Workers::default(), &checks);
        assert!(!report.ok());
        assert_eq!(report.broken(), vec!["bad"]);
        let good = report
            .findings
            .iter()
            .find(|f| f.name == "good")
            .expect("good check probed");
        assert!(good.detail.starts_with("sh -c ("), "argv shown: {good:?}");
    }
}
