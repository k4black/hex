//! Helpers shared by this binary crate's unit tests (`#[cfg(test)]` only).

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
