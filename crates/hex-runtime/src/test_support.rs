//! Helpers shared by this crate's unit tests (`#[cfg(test)]` only).

/// A per-call unique suffix for temp dirs. PIDs are recycled, so a name keyed on
/// the PID alone can collide with a *previous* test run's leftovers and read stale
/// files (some of these tests assert exact log contents).
pub(crate) fn unique() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
        .wrapping_add(N.fetch_add(1, Ordering::Relaxed))
}

/// A fresh, unique temp directory for one unit test. Deliberately not removed
/// afterwards — a failed test's files are the evidence you debug from; the OS
/// clears its temp dir (same policy as the integration tests' `temp_root`).
pub(crate) fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("hex-{tag}-{}-{}", std::process::id(), unique()));
    std::fs::create_dir_all(&dir).expect("mkdir temp dir");
    dir
}
