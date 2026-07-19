//! Layered configuration: `~/.config/hex/config.yaml` (user) then
//! `.hex/config.yaml` (project). Project wins on conflict.
//!
//! Config holds the **worker registry** — how to invoke each external agent —
//! plus fallback defaults used only when a graph omits its own. Built-in
//! registry entries for `codex` and `claude` mean the flagship critique loop
//! runs out of the box; config entries override them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Merged configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Worker registry: name → how to invoke it.
    pub workers: BTreeMap<String, WorkerSpec>,
    /// Fallback defaults when a graph omits its own.
    pub defaults: DefaultsSpec,
}

/// How to invoke one external agent CLI.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerSpec {
    /// Argv template, executed directly (never a shell string). A `{prompt}`
    /// token is replaced with the node's prompt; without one, the prompt is
    /// piped to stdin.
    pub command: Vec<String>,
}

/// Global fallback defaults.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DefaultsSpec {
    /// Default worker name for agent nodes.
    pub worker: Option<String>,
    /// Default context policy (`fresh`/`continue`).
    pub context: Option<String>,
}

impl Config {
    /// Load and merge user then project config, layered over the built-in
    /// registry. Missing files are not an error.
    ///
    /// # Errors
    /// Fails only if a present config file is unreadable or malformed.
    pub fn load(project_root: &Path) -> Result<Self> {
        let mut config = Self::builtin();
        if let Some(user) = user_config_path()
            && user.exists()
        {
            config.merge(Self::read(&user)?);
        }
        let project = project_root.join(".hex").join("config.yaml");
        if project.exists() {
            config.merge(Self::read(&project)?);
        }
        Ok(config)
    }

    /// Built-in registry so a fresh checkout can run `critique-loop`.
    #[must_use]
    pub fn builtin() -> Self {
        let mut workers = BTreeMap::new();
        workers.insert(
            "codex".to_owned(),
            WorkerSpec {
                command: vec!["codex".to_owned(), "exec".to_owned(), "{prompt}".to_owned()],
            },
        );
        workers.insert(
            "claude".to_owned(),
            WorkerSpec {
                command: vec!["claude".to_owned(), "-p".to_owned(), "{prompt}".to_owned()],
            },
        );
        Self {
            workers,
            defaults: DefaultsSpec {
                worker: Some("codex".to_owned()),
                context: Some("fresh".to_owned()),
            },
        }
    }

    fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }

    /// Merge `other` over `self` (other wins), for the user→project layering.
    fn merge(&mut self, other: Self) {
        for (name, spec) in other.workers {
            self.workers.insert(name, spec);
        }
        if other.defaults.worker.is_some() {
            self.defaults.worker = other.defaults.worker;
        }
        if other.defaults.context.is_some() {
            self.defaults.context = other.defaults.context;
        }
    }
}

fn user_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("hex")
            .join("config.yaml"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_has_codex_and_claude() {
        let c = Config::builtin();
        assert!(c.workers.contains_key("codex"));
        assert!(c.workers.contains_key("claude"));
    }

    #[test]
    fn merge_lets_project_override() {
        let mut base = Config::builtin();
        let mut over = Config::default();
        over.workers.insert(
            "codex".to_owned(),
            WorkerSpec {
                command: vec!["my-codex".to_owned()],
            },
        );
        base.merge(over);
        assert_eq!(base.workers["codex"].command, vec!["my-codex".to_owned()]);
        // Untouched entries survive.
        assert!(base.workers.contains_key("claude"));
    }
}
