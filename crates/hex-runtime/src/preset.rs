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

/// Names of the graphs shipped in the binary.
const BUILTINS: &[&str] = &["critique-loop"];

/// A resolved graph source plus a label describing where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The YAML source text.
    pub source: String,
    /// Human-readable origin (path or `built-in:<name>`), for diagnostics.
    pub origin: String,
}

/// A graph available to run, and where it resolves from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name to pass to `hex run`.
    pub name: String,
    /// Where it resolves from (a path, or `built-in`).
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

    // Same layers + extensions `list` scans, in precedence order, so a graph
    // that `hex list` shows as the winner is exactly what `hex run` executes.
    let mut candidates = Vec::new();
    let project = project_root.join(".hex").join("graphs");
    let user = user_graphs_dir();
    for ext in ["yaml", "yml"] {
        candidates.push(project.join(format!("{reference}.{ext}")));
    }
    if let Some(dir) = &user {
        for ext in ["yaml", "yml"] {
            candidates.push(dir.join(format!("{reference}.{ext}")));
        }
    }
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

/// List every runnable graph across the three layers, deduped by name with the
/// winning layer's origin (project > user > built-in — the same precedence
/// [`resolve`] uses). Sorted by name.
#[must_use]
pub fn list(project_root: &Path) -> Vec<Entry> {
    use std::collections::BTreeMap;
    // Insert lowest precedence first so higher layers overwrite the origin.
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    for name in BUILTINS {
        found.insert((*name).to_owned(), "built-in".to_owned());
    }
    if let Some(dir) = user_graphs_dir() {
        collect_yaml(&dir, &mut found);
    }
    collect_yaml(&project_root.join(".hex").join("graphs"), &mut found);
    found
        .into_iter()
        .map(|(name, origin)| Entry { name, origin })
        .collect()
}

/// Record each `*.yaml`/`*.yml` file in `dir` as `<stem> -> <path>` (overwriting
/// lower-precedence origins). A missing directory is simply skipped.
fn collect_yaml(dir: &Path, found: &mut std::collections::BTreeMap<String, String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_yaml = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "yaml" || e == "yml");
        // Only real files are runnable — a directory named `foo.yaml` is not.
        if is_yaml
            && path.is_file()
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            found.insert(stem.to_owned(), path.display().to_string());
        }
    }
}

fn user_graphs_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("hex")
            .join("graphs"),
    )
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

    #[test]
    fn a_yml_graph_is_both_listed_and_resolvable() {
        // Regression: `list` and `resolve` must agree on extensions, so a graph
        // shown by `hex list` is exactly what `hex run` executes.
        let root = std::env::temp_dir().join(format!("hex-yml-{}", std::process::id()));
        let graphs = root.join(".hex").join("graphs");
        std::fs::create_dir_all(&graphs).expect("mkdir");
        std::fs::write(graphs.join("only-yml.yml"), "version: 1").expect("write");

        assert!(list(&root).iter().any(|e| e.name == "only-yml"), "listed");
        let r = resolve("only-yml", &root).expect("a listed .yml graph must resolve");
        assert!(r.origin.ends_with("only-yml.yml"));
    }

    #[test]
    fn list_includes_builtins_and_project_graphs() {
        let root = std::env::temp_dir().join(format!("hex-list-{}", std::process::id()));
        let graphs = root.join(".hex").join("graphs");
        std::fs::create_dir_all(&graphs).expect("mkdir");
        std::fs::write(graphs.join("mine.yaml"), "version: 1").expect("write");

        let entries = list(&root);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"critique-loop"), "built-in listed");
        assert!(names.contains(&"mine"), "project graph listed");
        // Project origin is a real path, not the built-in label.
        let mine = entries.iter().find(|e| e.name == "mine").unwrap();
        assert!(mine.origin.contains("mine.yaml"));
    }
}
