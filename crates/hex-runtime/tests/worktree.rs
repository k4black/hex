//! Worktree isolation: the git mechanics (lease/reuse/reclaim/reattach) and one
//! end-to-end run whose workspace is a leased slot.

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{sh, temp_root, write_graph};
use hex_runtime::config::Config;
use hex_runtime::worktree;
use hex_runtime::{Isolation, Runtime, Workers};
use hex_worker::CommandWorker;

mod common;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A fresh temp git repo with one commit and a `.gitignore` ignoring `deps/`.
fn temp_repo(tag: &str) -> PathBuf {
    let root = temp_root(&format!("wt-{tag}"));
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "t@t.test"]);
    git(&root, &["config", "user.name", "t"]);
    git(&root, &["config", "commit.gpgsign", "false"]);
    std::fs::write(root.join("file.txt"), "base\n").unwrap();
    std::fs::write(root.join(".gitignore"), "deps/\n.hex/\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-qm", "init"]);
    root
}

/// Serializes every test that asserts *which* pool slot is leased: `lease` takes
/// the first slot it can lock, so "slot 0 is reused" only holds when no sibling
/// test is leasing concurrently. Not root-caused, and not a product bug — see
/// AGENTS.md gotcha 26.
fn pool_shape_gate() -> std::sync::MutexGuard<'static, ()> {
    static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GATE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[test]
fn resolve_base_returns_head_sha_and_rejects_unknown_refs() {
    let root = temp_repo("resolve");
    let sha = worktree::resolve_base(&root, None).expect("HEAD resolves");
    assert_eq!(sha.len(), 40, "full sha: {sha}");
    assert!(worktree::resolve_base(&root, Some("no-such-ref")).is_err());
}

#[test]
fn ensure_gitignored_is_idempotent() {
    let root = temp_repo("ignore");
    worktree::ensure_gitignored(&root, ".hex/worktrees/").unwrap();
    worktree::ensure_gitignored(&root, ".hex/worktrees/").unwrap();
    let body = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert_eq!(body.matches(".hex/worktrees/").count(), 1, "{body}");
}

#[test]
fn lease_creates_then_reuses_the_same_slot_when_clean() {
    let _gate = pool_shape_gate();
    let root = temp_repo("reuse");
    let sha = worktree::resolve_base(&root, None).unwrap();

    let first = worktree::lease(&root, "hex/r1", &sha).unwrap();
    assert!(first.warmup_needed, "fresh slot wants warmup");
    assert!(first.reclaimed.is_none());
    assert_eq!(first.dir, root.join(".hex/worktrees/0"));
    assert!(first.dir.join("file.txt").exists(), "base checked out");
    assert!(
        git(&root, &["rev-parse", "--verify", "hex/r1"]).len() == 40,
        "branch created"
    );
    drop(first); // releases the slot lock

    // A clean slot is reused in place on the next run: same dir, new branch, no
    // warmup (deps would still be warm).
    let second = worktree::lease(&root, "hex/r2", &sha).unwrap();
    assert_eq!(second.dir, root.join(".hex/worktrees/0"), "reused slot 0");
    assert!(!second.warmup_needed, "clean reuse skips warmup");
    assert!(second.reclaimed.is_none());
    assert_eq!(git(&second.dir, &["branch", "--show-current"]), "hex/r2");
}

#[test]
fn parallel_lease_grows_the_pool() {
    let _gate = pool_shape_gate();
    let root = temp_repo("parallel");
    let sha = worktree::resolve_base(&root, None).unwrap();
    // Hold the first lease while leasing again → a second slot must be created.
    let a = worktree::lease(&root, "hex/a", &sha).unwrap();
    let b = worktree::lease(&root, "hex/b", &sha).unwrap();
    assert_eq!(a.dir, root.join(".hex/worktrees/0"));
    assert_eq!(b.dir, root.join(".hex/worktrees/1"));
}

#[test]
fn dirty_slot_is_reclaimed_and_deps_survive() {
    let _gate = pool_shape_gate();
    let root = temp_repo("reclaim");
    let sha = worktree::resolve_base(&root, None).unwrap();

    let first = worktree::lease(&root, "hex/r1", &sha).unwrap();
    let slot = first.dir.clone();
    // Leave the slot dirty: a modified tracked file, a stray untracked file, and
    // a gitignored "dependency" that must survive the reclaim.
    std::fs::write(slot.join("file.txt"), "uncommitted edit\n").unwrap();
    std::fs::write(slot.join("stray.txt"), "junk\n").unwrap();
    std::fs::create_dir_all(slot.join("deps")).unwrap();
    std::fs::write(slot.join("deps/lib"), "warm\n").unwrap();
    drop(first);

    let second = worktree::lease(&root, "hex/r2", &sha).unwrap();
    assert_eq!(second.dir, slot, "same slot reclaimed");
    assert!(second.warmup_needed, "reclaim re-warms");
    assert!(
        second.reclaimed.as_deref().is_some_and(|r| !r.is_empty()),
        "discarded diff reported: {:?}",
        second.reclaimed
    );
    assert_eq!(
        std::fs::read_to_string(slot.join("file.txt")).unwrap(),
        "base\n",
        "tracked change reverted to base"
    );
    assert!(!slot.join("stray.txt").exists(), "untracked source removed");
    assert!(slot.join("deps/lib").exists(), "gitignored deps kept warm");
}

#[test]
fn reattach_recreates_a_missing_checkout_from_the_branch() {
    let _gate = pool_shape_gate();
    let root = temp_repo("reattach");
    let sha = worktree::resolve_base(&root, None).unwrap();
    let leased = worktree::lease(&root, "hex/r1", &sha).unwrap();
    let dir = leased.dir.clone();
    drop(leased);

    // Simulate a deleted checkout dir; the branch still exists.
    std::fs::remove_dir_all(&dir).unwrap();
    let _lock = worktree::reattach(&root, &dir, "hex/r1").expect("recreates from branch");
    assert!(dir.join(".git").exists(), "worktree recreated");

    // Branch gone too → fail closed.
    drop(_lock);
    std::fs::remove_dir_all(&dir).ok();
    git(&root, &["worktree", "prune"]); // clear stale metadata so the branch frees
    git(&root, &["branch", "-D", "hex/r1"]);
    assert!(worktree::reattach(&root, &dir, "hex/r1").is_err());
}

#[test]
fn run_warmup_runs_argv_and_propagates_failure() {
    let _gate = pool_shape_gate();
    let root = temp_repo("warmup");
    let sha = worktree::resolve_base(&root, None).unwrap();
    let slot = worktree::lease(&root, "hex/r1", &sha).unwrap();
    let logdir = root.join(".hex");
    std::fs::create_dir_all(&logdir).unwrap();
    worktree::run_warmup(&slot.dir, &["git".into(), "--version".into()], &logdir).expect("ok");
    assert!(
        worktree::run_warmup(&slot.dir, &["false".into()], &logdir).is_err(),
        "non-zero exit fails closed"
    );
}

#[test]
fn end_to_end_run_executes_in_the_leased_worktree() {
    let _gate = pool_shape_gate();
    const GRAPH: &str = r#"
version: 1
name: wt
entry: work
nodes:
  work:
    agent: { worker: w, prompt: "go", may_propose: [ready] }
    on: { ready: done }
  done:
    terminal: succeeded
accept: { require: [] }
"#;
    let root = temp_repo("e2e");
    write_graph(&root, "wt", GRAPH);

    // The worker records its cwd (the slot) as the result, then reports its
    // verdict — both in the captured message, since `result: file` reads only
    // that file.
    let mut workers = Workers::new();
    workers.insert(
        "w",
        Box::new(
            CommandWorker::new(
                "w",
                sh("pwd > \"$HEX_RESULT_FILE\"; echo 'VERDICT: ready' >> \"$HEX_RESULT_FILE\""),
            )
            .with_result_capture(Some(hex_worker::ResultCapture::File)),
        ),
    );
    let runtime = Runtime::with_workers(root.clone(), Config::builtin(), workers);
    let report = runtime
        .start(
            "wt",
            None,
            None,
            &Isolation::Worktree {
                base: None,
                init: vec![],
            },
        )
        .expect("run");
    assert!(matches!(
        report.disposition,
        Some(hex_runtime::Disposition::Succeeded)
    ));

    // The slot was created and the agent ran inside it.
    let slot = root.join(".hex/worktrees/0");
    assert!(slot.exists(), "slot created");
    let logs = runtime.logs(&report.run_id).unwrap();
    let cwd = logs
        .iter()
        .find_map(|l| l.result.clone())
        .unwrap_or_default();
    assert!(
        cwd.contains(".hex/worktrees/0"),
        "agent ran in the slot: {cwd}"
    );

    // The branch exists and `.hex/worktrees/` was gitignored.
    assert_eq!(
        git(
            &root,
            &["rev-parse", "--verify", &format!("hex/{}", report.run_id)]
        )
        .len(),
        40
    );
    let ignore = std::fs::read_to_string(root.join(".gitignore")).unwrap();
    assert!(ignore.contains(".hex/worktrees/"), "{ignore}");
}

/// `release_slot` deletes only a slot still on the releasing run's own branch:
/// slots are pooled, so a newer (possibly paused, hence unlocked) run may have
/// reclaimed it since the pruned run recorded it — its uncommitted work must
/// survive another run's prune.
#[test]
fn release_slot_keeps_a_slot_reused_by_another_branch() {
    let _gate = pool_shape_gate();
    let root = temp_repo("release-branch");
    let base_sha = worktree::resolve_base(&root, None).expect("base");
    let slot = worktree::lease(&root, "hex/old-run", &base_sha).expect("lease");
    let dir = slot.dir.clone();
    drop(slot); // the lease lock is gone, as after a pause or a finished run

    // Another branch now owns the checkout (a reclaim by a newer run).
    git(&dir, &["checkout", "-qb", "hex/new-run"]);

    assert!(
        !worktree::release_slot(&root, &dir, "hex/old-run"),
        "a reused slot must not be deleted"
    );
    assert!(dir.exists(), "the newer run's checkout survives");
    assert!(
        worktree::release_slot(&root, &dir, "hex/new-run"),
        "the owning branch may release it"
    );
    assert!(!dir.exists());
}
