//! Appending one JSON line to a user-global `~/.hex/` log.
//!
//! `hex feedback` and the usage `stats` log share this: create the directory,
//! append one line under `O_APPEND`, and let the caller decide whether a failure
//! matters. Nothing here needs a [`crate::Runtime`] — only `$HOME` — so a client
//! calls it from any project, worktree slot, or bare shell.

use std::io::Write;

/// The canonical (symlink-resolved, absolute) form of `path`, or the path as-is
/// when it cannot be resolved — recording a log line must never fail just
/// because a path could not be canonicalized.
#[must_use]
pub fn canonical(path: &std::path::Path) -> String {
    std::fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// Append `value` as one JSON line to `~/.hex/<file>`, creating the directory.
///
/// # Errors
/// Fails if `$HOME` is unset, or the directory/file cannot be created or
/// appended to. Best-effort by design: telemetry must never fail a command, so
/// both the stats writer and `feedback` only warn on stderr.
///
/// ponytail: one growing file, one `write_all` per line, no rotation and no
/// lock. That is atomic for the short lines these logs produce; add rotation
/// only when folding a log is measurably slow (see `hex stats`).
pub fn append(file: &str, value: &serde_json::Value) -> Result<(), String> {
    let path = path(file)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let mut line = serde_json::to_string(value).map_err(|e| e.to_string())?;
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(())
}

/// The absolute path of `~/.hex/<file>`, for a reader that only needs to locate
/// the log (and to return a clean "nothing recorded yet" when it is absent).
///
/// # Errors
/// Fails if `$HOME` is unset.
pub fn path(file: &str) -> Result<std::path::PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set, cannot locate ~/.hex")?;
    Ok(std::path::Path::new(&home).join(".hex").join(file))
}
