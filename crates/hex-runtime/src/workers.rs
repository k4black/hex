//! The worker registry the drive loop executes agent attempts through.

use std::collections::BTreeMap;

use hex_worker::{ClaudeWorker, CodexWorker, CommandWorker, OpencodeWorker, PiWorker, Worker};

use crate::config::{Config, WorkerKind, WorkerSpec};

/// A name → worker-adapter registry. Built from config for real runs, or
/// populated with mocks in tests.
#[derive(Default)]
pub struct Workers {
    map: BTreeMap<String, Box<dyn Worker>>,
}

impl Workers {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a worker under `name`.
    pub fn insert(&mut self, name: impl Into<String>, worker: Box<dyn Worker>) {
        self.map.insert(name.into(), worker);
    }

    /// Look up a worker adapter by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&dyn Worker> {
        self.map.get(name).map(AsRef::as_ref)
    }

    /// Registered workers in name order, for preflight reporting.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &dyn Worker)> {
        self.map.iter().map(|(k, v)| (k.as_str(), v.as_ref()))
    }

    /// Build the registry a graph resolves names against.
    ///
    /// Two kinds of entry land here, and a graph cannot tell them apart — it just
    /// names one:
    ///
    /// - every **worker** under its own name, so a bespoke `kind: command` entry
    ///   can be used directly;
    /// - every **role** under the role's name, built from the worker it binds plus
    ///   that role's `model`/`effort`. This is what a graph normally names
    ///   (`worker: reviewer`), and it is why two roles can share one CLI while
    ///   differing in model, effort and prompt.
    ///
    /// Roles are registered last, so a role wins a name clash with a worker — the
    /// role is the user-facing concept.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let mut workers = Self::new();
        for (name, spec) in &config.workers {
            workers.insert(name.clone(), build(name, spec, spec.model.clone(), None));
        }
        for (role_name, role) in &config.roles {
            // A role naming no worker, or an unknown one, is left unregistered:
            // preflight reports it as a missing binding rather than this layer
            // silently substituting a default agent.
            let Some(spec) = role.worker.as_deref().and_then(|w| config.workers.get(w)) else {
                continue;
            };
            let model = role.model.clone().or_else(|| spec.model.clone());
            workers.insert(
                role_name.clone(),
                build(role_name, spec, model, role.effort.clone()),
            );
        }
        workers
    }
}

/// Build one adapter for a worker spec, with an effective model and effort.
fn build(
    name: &str,
    spec: &WorkerSpec,
    model: Option<String>,
    effort: Option<String>,
) -> Box<dyn Worker> {
    match spec.kind {
        WorkerKind::Codex => Box::new(CodexWorker::new(model).with_effort(effort)),
        WorkerKind::Claude => Box::new(ClaudeWorker::new(model).with_effort(effort)),
        WorkerKind::Opencode => Box::new(OpencodeWorker::new(model)),
        WorkerKind::Pi => Box::new(PiWorker::new(model).with_effort(effort)),
        WorkerKind::Command => Box::new(
            CommandWorker::new(name.to_owned(), spec.command.clone())
                .with_result_capture(spec.result),
        ),
    }
}
