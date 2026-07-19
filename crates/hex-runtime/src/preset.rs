//! Resolve a graph reference to its YAML source.
//!
//! A reference is either a filesystem path (used directly) or a bare preset
//! name resolved through three layers, most specific first:
//!
//! 1. `<project>/.hex/graphs/<name>.yaml`
//! 2. `~/.config/hex/graphs/<name>.yaml`
//! 3. built-in presets shipped in the binary (currently `critique-loop`).

use std::path::{Path, PathBuf};

use crate::error::{HexError, Result};

/// The built-in critique loop, embedded so a fresh checkout can run it.
const CRITIQUE_LOOP: &str = include_str!("presets/critique-loop.yaml");

/// A resolved graph source plus a label describing where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The YAML source text.
    pub source: String,
    /// Human-readable origin (path or `built-in:<name>`), for diagnostics.
    pub origin: String,
}

/// Resolve `reference` (a path or a preset name) to graph YAML.
///
/// # Errors
/// Fails if a path does not exist/read, or a name matches no layer.
pub fn resolve(reference: &str, project_root: &Path) -> Result<Resolved> {
    // An explicit existing path wins and is used verbatim.
    let as_path = Path::new(reference);
    if as_path.is_file() {
        let source = std::fs::read_to_string(as_path)?;
        return Ok(Resolved {
            source,
            origin: as_path.display().to_string(),
        });
    }
    // Anything that looks like a path but does not exist is a hard error, not a
    // silent fall-through to a preset name.
    if reference.contains('/') || reference.ends_with(".yaml") || reference.ends_with(".yml") {
        return Err(HexError::new(format!("graph file not found: {reference}")));
    }

    let candidates = [
        project_root.join(".hex").join("graphs").join(format!("{reference}.yaml")),
        user_graphs_dir().map(|d| d.join(format!("{reference}.yaml"))).unwrap_or_default(),
    ];
    if let Some(path) = candidates.iter().find(|p| p.is_file()) {
        let source = std::fs::read_to_string(path)?;
        return Ok(Resolved {
            source,
            origin: path.display().to_string(),
        });
    }

    if let Some(source) = builtin(reference) {
        return Ok(Resolved {
            source: source.to_owned(),
            origin: format!("built-in:{reference}"),
        });
    }

    Err(HexError::new(format!(
        "no graph named `{reference}` (looked in .hex/graphs, ~/.config/hex/graphs, and built-ins)"
    )))
}

fn builtin(name: &str) -> Option<&'static str> {
    match name {
        "critique-loop" => Some(CRITIQUE_LOOP),
        _ => None,
    }
}

fn user_graphs_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config").join("hex").join("graphs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_builtin_critique_loop() {
        let r = resolve("critique-loop", Path::new("/nonexistent")).expect("resolves");
        assert_eq!(r.origin, "built-in:critique-loop");
        assert!(r.source.contains("name: critique-loop"));
    }

    #[test]
    fn unknown_name_errors() {
        assert!(resolve("nope", Path::new("/nonexistent")).is_err());
    }

    #[test]
    fn missing_path_is_a_hard_error() {
        assert!(resolve("./missing.yaml", Path::new("/nonexistent")).is_err());
    }
}
