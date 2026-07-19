//! Generic agent adapter: drive one external coding-agent CLI headless.
//!
//! One `AgentWorker` type covers claude, codex, and any other CLI — it is
//! parametrized by an argv template from the worker registry. The agent talks
//! back over the injected `hex emit` file channel (transport 1): the runtime
//! sets `HEX_EMIT_FILE`, the agent runs `hex emit <event>`, and after the
//! process exits this adapter reads the emitted signal.

use std::fs::{self, File};
use std::process::{Command as ProcCommand, Stdio};

use hex_proto::Capability;

use crate::{CapabilityManifest, WorkOutcome, WorkRequest, Worker};

/// The env var naming the file an agent appends its routing signal to.
pub const EMIT_FILE_ENV: &str = "HEX_EMIT_FILE";

/// Drives one external agent process from an argv template. Any `{prompt}`
/// token in the argv is replaced with the request's prompt; if none is present
/// the prompt is piped to the child's stdin.
#[derive(Debug, Clone)]
pub struct AgentWorker {
    /// Worker registry name (e.g. `codex`, `claude`).
    pub name: String,
    /// Argv template, executed directly — never a shell string.
    pub command: Vec<String>,
    /// Advertised capabilities.
    pub capabilities: CapabilityManifest,
}

impl AgentWorker {
    /// Build an agent worker with the default headless capabilities.
    #[must_use]
    pub fn new(name: impl Into<String>, command: Vec<String>) -> Self {
        Self {
            name: name.into(),
            command,
            capabilities: CapabilityManifest::from(&[
                Capability::FreshSessions,
                Capability::StreamingOutput,
                Capability::GracefulCancel,
            ]),
        }
    }
}

impl Worker for AgentWorker {
    fn capabilities(&self) -> CapabilityManifest {
        self.capabilities.clone()
    }

    fn run(&self, request: &WorkRequest) -> WorkOutcome {
        let Some((program, args)) = self.command.split_first() else {
            return WorkOutcome::error(format!("worker `{}` has an empty command", self.name));
        };

        let emit_file = request.attempt_dir.join("emitted");
        // Start clean so a resumed attempt never reads a stale signal.
        let _ = fs::remove_file(&emit_file);

        let uses_placeholder = self.command.iter().any(|a| a.contains("{prompt}"));
        let rendered: Vec<String> = args
            .iter()
            .map(|a| a.replace("{prompt}", &request.prompt))
            .collect();

        let (stdout, stderr) = match (
            File::create(request.attempt_dir.join("stdout.log")),
            File::create(request.attempt_dir.join("stderr.log")),
        ) {
            (Ok(o), Ok(e)) => (o, e),
            _ => return WorkOutcome::error("could not open attempt log files"),
        };

        let mut cmd = ProcCommand::new(program);
        cmd.args(&rendered)
            .current_dir(&request.workdir)
            .env("HEX_RUN_ID", &request.run_id)
            .env("HEX_NODE_ID", &request.node_id)
            .env("HEX_ATTEMPT_ID", &request.attempt_id)
            .env(EMIT_FILE_ENV, &emit_file)
            .env("HEX_MAY_PROPOSE", request.may_propose.join(","))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));

        if uses_placeholder {
            cmd.stdin(Stdio::null());
        } else {
            cmd.stdin(Stdio::piped());
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return WorkOutcome::error(format!("spawn `{program}` failed: {e}")),
        };

        if !uses_placeholder
            && let Some(mut stdin) = child.stdin.take()
        {
            use std::io::Write;
            let _ = stdin.write_all(request.prompt.as_bytes());
            // drop closes stdin
        }

        let status = match child.wait() {
            Ok(s) => s,
            Err(e) => return WorkOutcome::error(format!("wait failed: {e}")),
        };

        match read_signal(&emit_file, &request.may_propose) {
            Ok(signal) => WorkOutcome::signal(signal),
            Err(reason) => {
                let code = status.code().map_or_else(|| "signal".to_owned(), |c| c.to_string());
                WorkOutcome::error(format!("{reason} (exit {code})"))
            }
        }
    }
}

/// Read the last emitted signal and confirm it is allowed for this node.
fn read_signal(emit_file: &std::path::Path, may_propose: &[String]) -> Result<String, String> {
    let contents = fs::read_to_string(emit_file)
        .map_err(|_| "agent emitted no signal".to_owned())?;
    let last = contents
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .ok_or_else(|| "agent emitted no signal".to_owned())?;
    if may_propose.iter().any(|allowed| allowed == last) {
        Ok(last.to_owned())
    } else {
        Err(format!("agent emitted `{last}` which is not in may_propose"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hex-agent-test-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    fn request(dir: &std::path::Path, may: &[&str]) -> WorkRequest {
        WorkRequest {
            run_id: "run_0".to_owned(),
            node_id: "implement".to_owned(),
            attempt_id: "att_1".to_owned(),
            prompt: "hello".to_owned(),
            may_propose: may.iter().map(|s| (*s).to_owned()).collect(),
            workdir: dir.to_path_buf(),
            attempt_dir: dir.to_path_buf(),
        }
    }

    #[test]
    fn reads_emitted_signal_from_the_child() {
        let dir = temp_dir("emit");
        // Simulate an agent that writes its signal to $HEX_EMIT_FILE.
        let worker = AgentWorker::new(
            "fake",
            vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "printf ready > \"$HEX_EMIT_FILE\"".to_owned(),
            ],
        );
        let outcome = worker.run(&request(&dir, &["ready"]));
        assert_eq!(outcome, WorkOutcome::signal("ready"));
    }

    #[test]
    fn disallowed_signal_is_an_error() {
        let dir = temp_dir("disallowed");
        let worker = AgentWorker::new(
            "fake",
            vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "printf sneaky > \"$HEX_EMIT_FILE\"".to_owned(),
            ],
        );
        let outcome = worker.run(&request(&dir, &["ready"]));
        assert!(outcome.error.is_some());
    }

    #[test]
    fn no_emit_is_an_error() {
        let dir = temp_dir("silent");
        let worker = AgentWorker::new("fake", vec!["true".to_owned()]);
        let outcome = worker.run(&request(&dir, &["ready"]));
        assert!(outcome.error.is_some());
    }
}
