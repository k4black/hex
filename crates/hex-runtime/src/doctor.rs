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
//!   node's whole visit bound on false evidence.
//!
//! `hex doctor` reports both, and [`preflight`] refuses to start a run whose
//! workers are missing. (A `self` probe of `hex` on the agent's `PATH` died with
//! the `hex emit` channel — the agent no longer runs `hex`, so do not re-add it.)

use std::collections::BTreeMap;

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

/// Probe every configured worker and check, then every worker's credentials.
#[must_use]
pub fn report(workers: &Workers, checks: &BTreeMap<String, Vec<String>>) -> Report {
    let mut findings: Vec<Finding> = Vec::new();
    findings.extend(
        workers
            .entries()
            .map(|(name, worker)| probe("worker", name, worker.program())),
    );
    findings.extend(checks.iter().map(|(name, argv)| {
        probe("check", name, argv.first().map(String::as_str)).with_argv(argv)
    }));
    // Auth after presence: a missing binary already has a row, and running its
    // probe would only add noise. Identical argvs are probed once (roles alias
    // workers), keyed under the first name that produced them.
    let mut seen: BTreeMap<Vec<String>, ()> = BTreeMap::new();
    for (name, worker) in workers.entries() {
        let Some(argv) = worker.auth_probe() else {
            continue;
        };
        if seen.insert(argv.clone(), ()).is_some()
            || argv
                .first()
                .is_some_and(|program| which::which(program).is_err())
        {
            continue;
        }
        findings.push(auth_probe(name, &argv));
    }
    Report { findings }
}

/// Run one worker's credential probe: exit 0 with no `not_ready` in the output
/// means the credentials are usable, all without buying a completion.
///
/// ponytail: no timeout — every current probe is a local credential check; add
/// a bound if one is ever seen hanging.
fn auth_probe(name: &str, argv: &[String]) -> Finding {
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output();
    let (ok, detail) = match out {
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let ok = out.status.success() && !text.contains("not_ready");
            // First non-empty line is the human summary either way.
            let line = text
                .lines()
                .find(|l| l.chars().any(char::is_alphanumeric))
                .unwrap_or("no output")
                .trim()
                .to_owned();
            (ok, format!("{} ({line})", argv.join(" ")))
        }
        Err(e) => (false, format!("{} failed to start: {e}", argv.join(" "))),
    };
    Finding {
        kind: "auth",
        name: name.to_owned(),
        program: argv.first().cloned(),
        ok,
        detail,
    }
}

/// Probe one named entry's executable.
fn probe(kind: &'static str, name: &str, program: Option<&str>) -> Finding {
    let Some(program) = program else {
        return Finding {
            kind,
            name: name.to_owned(),
            program: None,
            // A worker that spawns nothing (the mock) is fine; it just isn't a
            // binary we can check.
            ok: true,
            detail: "no executable to probe".to_owned(),
        };
    };
    match which::which(program).ok() {
        Some(path) => Finding {
            kind,
            name: name.to_owned(),
            program: Some(program.to_owned()),
            detail: path.display().to_string(),
            ok: true,
        },
        None => Finding {
            kind,
            name: name.to_owned(),
            program: Some(program.to_owned()),
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
            && which::which(program).is_err()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_flags_a_missing_check_and_passes_a_real_one() {
        let checks = BTreeMap::from([
            ("good".to_owned(), vec!["sh".to_owned(), "-c".to_owned()]),
            ("bad".to_owned(), vec!["nope-xyz-not-real".to_owned()]),
        ]);
        let report = report(&Workers::default(), &checks);
        assert!(!report.ok());
        // Whether `hex` is installed depends on the machine running the tests, so
        // this asserts about checks only.
        assert!(report.broken().contains(&"bad"), "{:?}", report.broken());
        assert!(!report.broken().contains(&"good"));
        let good = report
            .findings
            .iter()
            .find(|f| f.name == "good")
            .expect("good check probed");
        assert!(good.detail.starts_with("sh -c ("), "argv shown: {good:?}");
    }

    /// Nothing configured, nothing probed — and so nothing broken.
    #[test]
    fn report_probes_every_configured_worker() {
        let report = report(&Workers::default(), &BTreeMap::new());
        assert!(
            report.findings.is_empty(),
            "nothing configured, nothing probed"
        );
        assert!(report.ok(), "and therefore nothing is broken");
    }
}
