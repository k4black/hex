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

/// One entry in the worker registry. A typed `kind` (`codex`/`claude`/
/// `opencode`) selects a built-in adapter that encapsulates that agent's argv,
/// output parsing, and permission policy — config only overrides its `model`.
/// The default `command` kind is the generic escape hatch: an explicit argv
/// template (`{prompt}`/`{result}` tokens) plus how to capture its result.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerSpec {
    /// Which adapter drives this worker.
    #[serde(default)]
    pub kind: WorkerKind,
    /// Model override for a typed kind (e.g. `gpt-5`, `claude-...`).
    #[serde(default)]
    pub model: Option<String>,
    /// Argv template for `kind: command` (executed directly, never a shell).
    #[serde(default)]
    pub command: Vec<String>,
    /// Result capture for `kind: command` (typed kinds set their own).
    #[serde(default)]
    pub result: Option<ResultKind>,
}

/// Which adapter a worker uses.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerKind {
    /// Generic argv template (default) — see [`WorkerSpec::command`].
    #[default]
    Command,
    /// OpenAI Codex (`codex exec`).
    Codex,
    /// Claude Code headless (`claude -p`).
    Claude,
    /// opencode (`opencode run`).
    Opencode,
}

/// Result-capture strategy in config form (maps to `hex_worker::ResultCapture`).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    /// The worker wrote its final message to the `{result}` file.
    File,
    /// stdout is a single JSON object; take `.result` (honoring `.is_error`).
    JsonResult,
    /// stdout is JSONL; take the last `type == "text"` line's `part.text`.
    JsonlLastText,
}

impl From<ResultKind> for hex_worker::ResultCapture {
    fn from(kind: ResultKind) -> Self {
        match kind {
            ResultKind::File => Self::File,
            ResultKind::JsonResult => Self::JsonResult,
            ResultKind::JsonlLastText => Self::JsonlLastText,
        }
    }
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
        let typed = |kind| WorkerSpec {
            kind,
            model: None,
            command: Vec::new(),
            result: None,
        };
        let mut workers = BTreeMap::new();
        workers.insert("codex".to_owned(), typed(WorkerKind::Codex));
        workers.insert("claude".to_owned(), typed(WorkerKind::Claude));
        workers.insert("opencode".to_owned(), typed(WorkerKind::Opencode));
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
        Ok(yaml_serde::from_str(&text)?)
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
                kind: WorkerKind::Command,
                model: None,
                command: vec!["my-codex".to_owned()],
                result: None,
            },
        );
        base.merge(over);
        assert_eq!(base.workers["codex"].kind, WorkerKind::Command);
        assert_eq!(base.workers["codex"].command, vec!["my-codex".to_owned()]);
        // Untouched entries survive.
        assert!(base.workers.contains_key("claude"));
    }
}
