//! Helpers shared by hex-runtime's integration tests.
//!
//! Each `tests/*.rs` is its own binary, so anything two of them need has to live
//! in a module both declare (`mod common;`) — this file is that module, in place
//! of a copy per test binary. Not every binary uses every helper.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// A fresh temp project root for one test, unique per call. `tag` names the test
/// so a leftover directory is identifiable.
///
/// Deliberately **kept** afterwards: a failed run's `.hex/runs/<id>` journal and
/// captured agent output are exactly the evidence you debug from, and a drop
/// guard would take them away at the moment they matter. The OS clears its temp
/// dir.
pub fn temp_root(tag: &str) -> PathBuf {
    tempfile::Builder::new()
        .prefix(&format!("hex-it-{tag}-"))
        .tempdir()
        .expect("mkdir root")
        .keep()
}

/// Write `source` as the project graph `.hex/graphs/<name>.yaml`.
pub fn write_graph(root: &Path, name: &str, source: &str) {
    let dir = root.join(".hex").join("graphs");
    std::fs::create_dir_all(&dir).expect("mkdir graphs");
    std::fs::write(dir.join(format!("{name}.yaml")), source).expect("write graph");
}

/// `sh -c <script>` as an argv.
pub fn sh(script: &str) -> Vec<String> {
    vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
}
