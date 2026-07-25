//! Per-run git worktree isolation (Phase 2, thin slice). A run opted into
//! `worktree` executes on a fresh branch `hex/<run-id>` inside a **pooled**,
//! reusable slot under `.hex/worktrees/<n>/`, so built (gitignored) deps stay
//! warm across runs. Slots are leased with the same `fs4` advisory lock the run
//! uses, so parallel runs never collide. No auto-merge: the branch is left for
//! manual integration. See `docs/design/2026-07-21-worktree-isolation.md`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{HexError, Result};

/// How a run's workspace is isolated. `Shared` runs in the project root (today's
/// behavior); `Worktree` leases a pooled git worktree.
#[derive(Debug, Clone, Default)]
pub enum Isolation {
    /// Run in the project root (default).
    #[default]
    Shared,
    /// Run in a per-run git worktree branched from `base` (or HEAD), optionally
    /// primed by the `init` warmup argv.
    Worktree {
        /// Base ref to branch from; `None` = current `HEAD`.
        base: Option<String>,
        /// Optional warmup command (argv) run in a fresh/reclaimed slot.
        init: Vec<String>,
    },
}

/// A leased worktree slot: the checkout dir plus the held advisory lock (kept
/// alive for the run's duration; the OS releases it on crash).
pub struct Slot {
    /// The worktree checkout directory (becomes the run's workdir).
    pub dir: PathBuf,
    /// The run's branch, `hex/<run-id>`.
    pub branch: String,
    /// The base ref requested (`HEAD` or a branch name), recorded for resume.
    pub base_ref: String,
    /// Whether the slot needs warmup (freshly created or reclaimed — not a clean
    /// warm reuse).
    pub warmup_needed: bool,
    /// If the slot was reclaimed, the discarded diffstat (for logging).
    pub reclaimed: Option<String>,
    _lock: File,
}

/// The relative path (from the project root) hex keeps worktree slots under.
pub const WORKTREES_DIR: &str = ".hex/worktrees";

/// Whether `root` is inside a git working tree.
#[must_use]
pub fn is_git_repo(root: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Run `git -C root <args>`, returning trimmed stdout on success.
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|e| HexError::new(format!("failed to run git: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(HexError::new(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Resolve the base ref name and its commit sha. `base` = `None` means `HEAD`.
///
/// # Errors
/// Fails if the ref does not resolve to a commit.
pub fn resolve_base(root: &Path, base: Option<&str>) -> Result<(String, String)> {
    let base_ref = base.unwrap_or("HEAD");
    let sha = git(
        root,
        &["rev-parse", "--verify", &format!("{base_ref}^{{commit}}")],
    )
    .map_err(|_| {
        HexError::new(format!(
            "base ref `{base_ref}` does not resolve to a commit"
        ))
    })?;
    Ok((base_ref.to_owned(), sha))
}

/// Ensure `entry` is present in `<root>/.gitignore`, appending it if missing so a
/// worktree checkout under it never pollutes the main tree's status.
///
/// # Errors
/// Propagates IO failures reading/writing `.gitignore`.
pub fn ensure_gitignored(root: &Path, entry: &str) -> Result<()> {
    let path = root.join(".gitignore");
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current.lines().any(|l| l.trim() == entry) {
        return Ok(());
    }
    let mut next = current;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(entry);
    next.push('\n');
    std::fs::write(&path, next)?;
    Ok(())
}

/// Lease a pooled slot for `branch`, branching from `base_sha`. Walks slot
/// indices, lock-gating each: the first index whose lock we can hold is used —
/// reused clean, reclaimed if dirty, or freshly created if it doesn't exist yet.
///
/// # Errors
/// Fails on git or IO errors, or if the pool is implausibly exhausted.
pub fn lease(root: &Path, branch: &str, base_ref: &str, base_sha: &str) -> Result<Slot> {
    let pool = root.join(WORKTREES_DIR);
    std::fs::create_dir_all(&pool)?;
    for n in 0..1024u32 {
        let Some(lock) = crate::try_lock_file(&pool.join(format!("{n}.lock")))? else {
            continue; // busy — a live run holds this slot
        };
        let dir = pool.join(n.to_string());
        let (warmup_needed, reclaimed) = if !dir.exists() {
            // Fresh slot: cold deps, warmup wanted. `worktree add` checks the
            // branch out for us.
            git(
                root,
                &[
                    "worktree",
                    "add",
                    "-b",
                    branch,
                    &dir.to_string_lossy(),
                    base_sha,
                ],
            )?;
            (true, None)
        } else {
            // Existing slot we now own. One `status` decides clean-vs-dirty and
            // doubles as the discarded-untracked report on reclaim.
            let status = git(&dir, &["status", "--porcelain"])?;
            let reclaimed = (!status.is_empty()).then(|| {
                let diffstat = git(&dir, &["diff", "--stat"]).unwrap_or_default();
                let report = format!("{diffstat}\n{status}").trim().to_owned();
                git(&dir, &["reset", "--hard"])?;
                git(&dir, &["clean", "-fd"])?; // keeps gitignored deps warm
                Ok::<_, HexError>(report)
            });
            let reclaimed = reclaimed.transpose()?;
            git(&dir, &["checkout", "-B", branch, base_sha])?;
            (reclaimed.is_some(), reclaimed)
        };
        return Ok(Slot {
            dir,
            branch: branch.to_owned(),
            base_ref: base_ref.to_owned(),
            warmup_needed,
            reclaimed,
            _lock: lock,
        });
    }
    Err(HexError::new(
        "worktree pool exhausted (1024 slots all busy)",
    ))
}

/// Reattach to a run's recorded slot on resume. Recreates the checkout from the
/// branch tip if the directory is gone; fails closed if the branch is also gone.
///
/// # Errors
/// Fails if the slot is locked by another process or cannot be recreated.
pub fn reattach(root: &Path, dir: &Path, branch: &str) -> Result<File> {
    let pool = root.join(WORKTREES_DIR);
    let n = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let lock = crate::try_lock_file(&pool.join(format!("{n}.lock")))?
        .ok_or_else(|| HexError::new("this run's worktree is locked by another process"))?;
    if dir.join(".git").exists() {
        return Ok(lock); // checkout intact
    }
    // Checkout gone: recover from the branch tip if the branch still exists.
    if git(
        root,
        &["rev-parse", "--verify", &format!("{branch}^{{commit}}")],
    )
    .is_err()
    {
        return Err(HexError::new(format!(
            "worktree and branch `{branch}` are both gone — cannot resume"
        )));
    }
    let _ = git(root, &["worktree", "prune"]);
    git(root, &["worktree", "add", &dir.to_string_lossy(), branch])?;
    Ok(lock)
}

/// Run the warmup `argv` in `dir`, capturing output under `log_dir`.
///
/// # Errors
/// Fails if the command cannot start or exits non-zero (the environment isn't
/// ready, so the run should not proceed).
pub fn run_warmup(dir: &Path, argv: &[String], log_dir: &Path) -> Result<()> {
    let Some((program, args)) = argv.split_first() else {
        return Ok(());
    };
    let log = std::fs::File::create(log_dir.join("worktree-init.log"))?;
    let err = log.try_clone()?;
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(err))
        .status()
        .map_err(|e| HexError::new(format!("worktree init `{program}` failed to start: {e}")))?;
    if !status.success() {
        return Err(HexError::new(format!(
            "worktree init `{}` exited with {} (see worktree-init.log)",
            argv.join(" "),
            status.code().unwrap_or(-1)
        )));
    }
    Ok(())
}
