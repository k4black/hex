//! Helpers shared by hex-runtime's integration tests.
//!
//! Each `tests/*.rs` is its own binary, so anything two of them need has to live
//! in a module both declare (`mod common;`) — this file is that module, in place
//! of a copy per test binary.

use std::path::PathBuf;

/// A fresh temp project root for one test, unique per call. `tag` names the test
/// so a leftover directory is identifiable.
///
/// Deliberately **not** removed afterwards: a failed run's `.hex/runs/<id>`
/// journal and captured agent output are exactly the evidence you debug from, and
/// a drop guard would take them away at the moment they matter (it would also
/// have to replace the plain `PathBuf` every caller passes around). The OS clears
/// its temp dir; every other temp helper in the workspace leaks for the same
/// reason.
pub fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hex-it-{tag}-{}-{}",
        std::process::id(),
        hex_runtime::journal::now_ms()
    ));
    std::fs::create_dir_all(&root).expect("mkdir root");
    root
}
