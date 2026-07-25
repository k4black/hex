//! Worktree isolation: the git mechanics (lease/reuse/reclaim/reattach) and one
//! end-to-end run whose workspace is a leased slot.

use std::path::{Path, PathBuf};
use std::process::Command;

use hex_runtime::config::Config;
use hex_runtime::worktree;
use hex_runtime::{Isolation, Runtime, Workers};
use hex_worker::CommandWorker;

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
    let root = std::env::temp_dir().join(format!(
        "hex-wt-{tag}-{}-{}",
        std::process::id(),
        hex_runtime::journal::now_ms()
    ));
    std::fs::create_dir_all(&root).unwrap();
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

fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
}

#[test]
fn resolve_base_returns_head_sha_and_rejects_unknown_refs() {
    let root = temp_repo("resolve");
    let (base_ref, sha) = worktree::resolve_base(&root, None).expect("HEAD resolves");
    assert_eq!(base_ref, "HEAD");
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
    let root = temp_repo("reuse");
    let (base_ref, sha) = worktree::resolve_base(&root, None).unwrap();

    let first = worktree::lease(&root, "hex/r1", &base_ref, &sha).unwrap();
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
    let second = worktree::lease(&root, "hex/r2", &base_ref, &sha).unwrap();
    assert_eq!(second.dir, root.join(".hex/worktrees/0"), "reused slot 0");
    assert!(!second.warmup_needed, "clean reuse skips warmup");
    assert!(second.reclaimed.is_none());
    assert_eq!(git(&second.dir, &["branch", "--show-current"]), "hex/r2");
}

#[test]
fn parallel_lease_grows_the_pool() {
    let root = temp_repo("parallel");
    let (base_ref, sha) = worktree::resolve_base(&root, None).unwrap();
    // Hold the first lease while leasing again → a second slot must be created.
    let a = worktree::lease(&root, "hex/a", &base_ref, &sha).unwrap();
    let b = worktree::lease(&root, "hex/b", &base_ref, &sha).unwrap();
    assert_eq!(a.dir, root.join(".hex/worktrees/0"));
    assert_eq!(b.dir, root.join(".hex/worktrees/1"));
}

#[test]
fn dirty_slot_is_reclaimed_and_deps_survive() {
    let root = temp_repo("reclaim");
    let (base_ref, sha) = worktree::resolve_base(&root, None).unwrap();

    let first = worktree::lease(&root, "hex/r1", &base_ref, &sha).unwrap();
    let slot = first.dir.clone();
    // Leave the slot dirty: a modified tracked file, a stray untracked file, and
    // a gitignored "dependency" that must survive the reclaim.
    std::fs::write(slot.join("file.txt"), "uncommitted edit\n").unwrap();
    std::fs::write(slot.join("stray.txt"), "junk\n").unwrap();
    std::fs::create_dir_all(slot.join("deps")).unwrap();
    std::fs::write(slot.join("deps/lib"), "warm\n").unwrap();
    drop(first);

    let second = worktree::lease(&root, "hex/r2", &base_ref, &sha).unwrap();
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
    let root = temp_repo("reattach");
    let (base_ref, sha) = worktree::resolve_base(&root, None).unwrap();
    let leased = worktree::lease(&root, "hex/r1", &base_ref, &sha).unwrap();
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
    let root = temp_repo("warmup");
    let (base_ref, sha) = worktree::resolve_base(&root, None).unwrap();
    let slot = worktree::lease(&root, "hex/r1", &base_ref, &sha).unwrap();
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
    const GRAPH: &str = r#"
version: 1
name: wt
entry: work
defaults: { budget: { attempts: 4 } }
nodes:
  work:
    agent: { worker: w, prompt: "go", may_propose: [ready] }
    on: { ready: done }
  done:
    terminal: succeeded
accept: { require: [] }
"#;
    let root = temp_repo("e2e");
    let gdir = root.join(".hex/graphs");
    std::fs::create_dir_all(&gdir).unwrap();
    std::fs::write(gdir.join("wt.yaml"), GRAPH).unwrap();

    // The worker records its cwd (the slot) as the result, then emits `ready`.
    let mut workers = Workers::new();
    workers.insert(
        "w",
        Box::new(
            CommandWorker::new(
                "w",
                sh("pwd > \"$HEX_RESULT_FILE\"; printf ready > \"$HEX_EMIT_FILE\""),
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
        hex_runtime::Disposition::Succeeded
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
