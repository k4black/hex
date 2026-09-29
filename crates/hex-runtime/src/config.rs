//! Layered configuration: built-in defaults, then
//! `~/.config/hex/config.yaml` (user), then `.hex/config.yaml` (project).
//! Layers **deep-merge per key**, so a project overrides only what it names.
//!
//! Config holds three things: the **worker registry** (how to invoke each agent
//! CLI — internal plumbing), the **roles** a graph actually names
//! (implementer/reviewer/planner/researcher, each binding a worker to a model,
//! effort, read-only policy and prompt preamble), and the project's **checks**.
//!
//! The built-in layer is [`DEFAULTS`] — a real YAML file embedded in the binary
//! and parsed through this same loader, not a hardcoded Rust value. One format,
//! one code path, nothing to drift.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::Result;

/// The built-in configuration layer, embedded and parsed like any other.
const DEFAULTS: &str = include_str!("defaults.yaml");

/// Merged configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Worker registry: name → how to invoke that agent CLI. Internal plumbing;
    /// graphs name a [`RoleSpec`], not a worker.
    pub workers: BTreeMap<String, WorkerSpec>,
    /// Roles a graph can name (`role: reviewer`): a worker plus the model,
    /// effort, read-only policy and prompt preamble that define the job.
    pub roles: BTreeMap<String, RoleSpec>,
    /// Project checks: name → argv, referenced by a graph as
    /// `command: { check: <name> }`.
    ///
    /// **Empty by default, deliberately** — what "green" means is a per-project
    /// decision, so no built-in preset gates on a check. A graph that *does* name
    /// a check must have it declared here, or the run is refused before it
    /// starts: a check that silently passed would let a run report `succeeded`
    /// having verified nothing.
    pub checks: BTreeMap<String, Vec<String>>,
    /// Fallback defaults when a graph omits its own.
    pub defaults: DefaultsSpec,
}

/// One entry in the worker registry. A typed `kind` (`codex`/`claude`/
/// `opencode`) selects a built-in adapter that encapsulates that agent's argv,
/// output parsing, and permission policy — config only overrides its `model`.
/// The default `command` kind is the generic escape hatch: an explicit argv
/// template (`{prompt}`/`{result}` tokens) plus how to capture its result.
// `deny_unknown_fields` like `Config` and `RoleSpec`: without it a typo such as
// `argv:` for `command:` parsed cleanly, `hex doctor` called the worker "ok",
// and the mistake only surfaced as a failed first attempt reading
// "worker `x` has an empty command".
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub result: Option<hex_worker::ResultCapture>,
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
    /// Pi coding agent (`pi -p`).
    Pi,
}

/// One role: the job a graph names, bound to a worker.
///
/// Every field is optional so a higher layer can override one of them and
/// inherit the rest — see this type's private `merge`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RoleSpec {
    /// Which worker (CLI adapter) runs this role.
    pub worker: Option<String>,
    /// Model override passed to the worker.
    pub model: Option<String>,
    /// Reasoning effort, where the agent supports one (e.g. `high`).
    pub effort: Option<String>,
    /// Whether this role must not modify the workspace (a reviewer/planner).
    pub read_only: Option<bool>,
    /// Prompt preamble prepended to the node's own prompt. Setting this
    /// **replaces** any inherited preamble.
    pub prompt: Option<String>,
    /// Text appended to the inherited preamble instead of replacing it — the
    /// common case for a project tightening a shipped role ("only flag security
    /// issues", "fix at most 50% of the bugs").
    pub prompt_append: Option<String>,
}

impl RoleSpec {
    /// Merge `other` (a higher layer) over `self`, per key.
    ///
    /// `prompt` replaces; `prompt_append` accumulates onto whatever prompt
    /// survives, so a project can extend a built-in role's preamble without
    /// restating it.
    fn merge(&mut self, other: Self) {
        self.worker = other.worker.or(self.worker.take());
        self.model = other.model.or(self.model.take());
        self.effort = other.effort.or(self.effort.take());
        self.read_only = other.read_only.or(self.read_only.take());
        if other.prompt.is_some() {
            // An explicit prompt replaces the inherited one, and also discards
            // any appendix inherited with it — otherwise a redefinition would
            // silently keep text the author meant to drop.
            self.prompt = other.prompt;
            self.prompt_append = None;
        }
        if let Some(extra) = other.prompt_append {
            let appended = match self.prompt_append.take() {
                Some(existing) => format!("{}\n\n{extra}", existing.trim_end()),
                None => extra,
            };
            self.prompt_append = Some(appended);
        }
    }

    /// The effective preamble: the prompt plus any appended text.
    #[must_use]
    pub fn preamble(&self) -> Option<String> {
        match (self.prompt.as_deref(), self.prompt_append.as_deref()) {
            (None, None) => None,
            (Some(p), None) => Some(p.trim_end().to_owned()),
            (None, Some(a)) => Some(a.trim_end().to_owned()),
            (Some(p), Some(a)) => Some(format!("{}\n\n{}", p.trim_end(), a.trim_end())),
        }
    }
}

/// Global fallback defaults.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DefaultsSpec {
    /// Default role for agent nodes that name none.
    pub role: Option<String>,
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

    /// The built-in layer, parsed from the embedded [`DEFAULTS`] YAML.
    ///
    /// # Panics
    /// Panics only if the embedded defaults are malformed, which a unit test
    /// pins — it is a build-time bug, never a user-facing failure.
    #[must_use]
    pub fn builtin() -> Self {
        yaml_serde::from_str(DEFAULTS).expect("embedded defaults.yaml is valid")
    }

    fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(yaml_serde::from_str(&text)?)
    }

    /// Merge `other` over `self` (other wins), for the user→project layering.
    ///
    /// Public so callers can compose layers explicitly (and so tests can assert
    /// the merge rules that decide which role a graph actually gets).
    pub fn merge(&mut self, other: Self) {
        self.workers.extend(other.workers);
        // Per-check granularity: a project overriding `test` keeps a user-level
        // `lint` rather than replacing the whole map.
        self.checks.extend(other.checks);
        // Roles merge per field, so overriding one model inherits the rest.
        for (name, spec) in other.roles {
            self.roles.entry(name).or_default().merge(spec);
        }
        self.defaults.role = other.defaults.role.or(self.defaults.role.take());
        self.defaults.context = other.defaults.context.or(self.defaults.context.take());
    }
}

fn user_config_path() -> Option<PathBuf> {
    Some(
        crate::local_log::home_dir()?
            .join(".config")
            .join("hex")
            .join("config.yaml"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
