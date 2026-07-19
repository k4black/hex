//! The worker registry the drive loop executes agent attempts through.

use std::collections::BTreeMap;

use hex_worker::{AgentWorker, Worker};

use crate::config::Config;

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

    /// Registered worker names (sorted).
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.map.keys().map(String::as_str).collect()
    }

    /// Build a registry of [`AgentWorker`]s from the config's worker registry.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let mut workers = Self::new();
        for (name, spec) in &config.workers {
            workers.insert(
                name.clone(),
                Box::new(AgentWorker::new(name.clone(), spec.command.clone())),
            );
        }
        workers
    }
}
