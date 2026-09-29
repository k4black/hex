//! Helpers shared by this crate's unit tests (`#[cfg(test)]` only).

/// A fresh, unique temp directory for one unit test. Deliberately kept
/// afterwards — a failed test's files are the evidence you debug from; the OS
/// clears its temp dir (same policy as the integration tests' `temp_root`).
pub(crate) fn temp_dir(tag: &str) -> std::path::PathBuf {
    tempfile::Builder::new()
        .prefix(&format!("hex-{tag}-"))
        .tempdir()
        .expect("mkdir temp dir")
        .keep()
}
