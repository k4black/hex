//! The control inbox: how an operator (human *or* agent) reaches a run.
//!
//! One command per file under `.hex/runs/<id>/control/`, written to `tmp/` and
//! then `rename()`d into `inbox/` — the rename is atomic on POSIX, so a reader
//! can never observe a torn write. The driver polls `inbox/` at attempt
//! boundaries and moves each file it consumes into `done/`.
//!
//! Polled files rather than a socket, deliberately: there is no persistent
//! listener to push to (a run may be live, paused, or waiting to be `resume`d
//! hours later), and every daemonless job runner — GitHub Actions, GitLab
//! Runner, Buildkite — converges on exactly this shape for the same reason. The
//! files are a *transport*, never authority: what actually happened is whatever
//! the driver journaled in response.

use std::io::Write;
use std::path::{Path, PathBuf};

use hex_proto::{Actor, Command, Disposition};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::journal::now_ms;

/// One queued command plus its issuer. Authority is scoped per actor, not per
/// surface, so the actor travels with the command and lands in the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Who issued the command.
    pub actor: Actor,
    /// What they asked for.
    pub command: Command,
}

/// A run's control directory (`control/` under the run dir).
pub struct Inbox {
    dir: PathBuf,
}

impl Inbox {
    /// The inbox for a run directory. Creating one touches no filesystem.
    #[must_use]
    pub fn new(run_dir: &Path) -> Self {
        Self {
            dir: run_dir.join("control"),
        }
    }

    fn tmp(&self) -> PathBuf {
        self.dir.join("tmp")
    }
    fn queue(&self) -> PathBuf {
        self.dir.join("inbox")
    }
    fn done(&self) -> PathBuf {
        self.dir.join("done")
    }

    /// Queue `command` for the run's driver.
    ///
    /// Written to `tmp/` and renamed into `inbox/`, so a driver polling `inbox/`
    /// only ever sees complete files — the partially written one is in a
    /// directory it never reads.
    ///
    /// # Errors
    /// Fails if the control directories or the file cannot be written.
    pub fn send(&self, actor: &Actor, command: &Command) -> Result<()> {
        std::fs::create_dir_all(self.tmp())?;
        std::fs::create_dir_all(self.queue())?;
        let queue = self.queue();
        // Millis-first names make a lexicographic sort match arrival order, and
        // the uuid keeps two commands issued in the same millisecond distinct.
        let name = format!("{:013}-{}.json", now_ms(), uuid::Uuid::new_v4().simple());
        let envelope = Envelope {
            actor: actor.clone(),
            command: command.clone(),
        };
        let line = format!("{}\n", serde_json::to_string(&envelope)?);
        // Durable: an acknowledged `hex cancel` must not vanish on a hard reset,
        // which is the opposite of what the at-most-once contract intends (never
        // applied twice, but an ack should mean queued).
        stage_then_rename(
            &self.tmp().join(&name),
            &queue.join(&name),
            line.as_bytes(),
            Durability::Fsync,
        )
    }

    /// Claim the oldest queued command, or `None` when the inbox is empty.
    ///
    /// One command at a time, and moved to `done/` *before* it is handed over —
    /// so a crash mid-processing can lose that one command but can never apply it
    /// twice. That direction is deliberate: re-issuing a `steer` is a keystroke,
    /// whereas a double-applied `cancel` would append a second terminal event and
    /// corrupt the journal's lifecycle.
    ///
    /// Claiming *one* rather than the whole batch is what keeps that cost honest.
    /// Draining the batch up front marked every queued file done, and a caller
    /// that stopped early (the driver returns as soon as a command finishes or
    /// pauses the run) silently discarded the rest — losing a steer in entirely
    /// ordinary operation, not just after a crash.
    ///
    /// # Errors
    /// Fails if the inbox cannot be read or an entry cannot be moved.
    pub fn claim_next(&self) -> Result<Option<Envelope>> {
        let queue = self.queue();
        loop {
            let Some(name) = self.names()?.into_iter().next() else {
                return Ok(None);
            };
            let raw = std::fs::read_to_string(queue.join(&name))?;
            std::fs::create_dir_all(self.done())?;
            std::fs::rename(queue.join(&name), self.done().join(&name))?;
            // Both directories changed, and the claim must outlive a power loss:
            // an un-fsynced `inbox → done` rename can roll back and re-deliver a
            // command whose effect is already in the journal.
            sync_dir(&self.done())?;
            sync_dir(&queue)?;
            // A malformed command is not authority for anything, so it is skipped
            // rather than failing the run; the file stays in `done/` as evidence
            // of what was rejected.
            if let Ok(envelope) = serde_json::from_str::<Envelope>(&raw) {
                return Ok(Some(envelope));
            }
        }
    }

    /// Commands sitting in the inbox that no driver has claimed yet, in arrival
    /// order. **Read-only** — nothing is moved to `done/`, so this cannot consume a
    /// command the way [`claim_next`](Self::claim_next) does.
    ///
    /// This is the difference between "queued" and "pending": a `steer` lives here
    /// until the driver reaches an attempt boundary, and only then becomes a
    /// journaled `Steered` that the projection knows about. Without a read of this
    /// directory, `hex status` could not tell an operator whether the steer they
    /// just sent had been picked up or was simply lost.
    ///
    /// # Errors
    /// Fails if the inbox directory cannot be read.
    pub fn queued(&self) -> Result<Vec<Envelope>> {
        let queue = self.queue();
        Ok(self
            .names()?
            .iter()
            .filter_map(|n| std::fs::read_to_string(queue.join(n)).ok())
            .filter_map(|raw| serde_json::from_str::<Envelope>(&raw).ok())
            .collect())
    }

    /// Queued file names in arrival order (names are millis-first, so lexical
    /// order is arrival order). No inbox yet is the common case: nobody has sent
    /// anything.
    fn names(&self) -> Result<Vec<String>> {
        let entries = match std::fs::read_dir(self.queue()) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut names: Vec<String> = entries
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".json"))
            .collect();
        names.sort_unstable();
        Ok(names)
    }
}

/// Whether a staged publish is merely *visible* or also survives power loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Durability {
    /// `fsync` the file before the rename and the destination directory after.
    Fsync,
    /// Neither — the rename is atomic for readers, and that is all that is
    /// needed when losing the write costs nothing.
    Visible,
}

/// Write `contents` to `staged` and `rename()` it onto `published`, so a reader
/// of `published` only ever sees a complete file (the partial one is at a name it
/// never looks at). The shared half of the control inbox's `send` and the
/// heartbeat's `beat`.
///
/// [`Durability`] is what actually differs between those two, and the difference
/// is deliberate: a queued command must survive a hard reset, whereas a beat
/// carries no run state and is rewritten every few seconds, so two fsyncs per
/// beat would be real IO for a beacon that is explicitly never authority.
fn stage_then_rename(
    staged: &Path,
    published: &Path,
    contents: &[u8],
    durability: Durability,
) -> Result<()> {
    {
        let mut file = std::fs::File::create(staged)?;
        file.write_all(contents)?;
        // Durable before it is visible: the rename must not publish a name whose
        // contents a crash could still lose.
        if durability == Durability::Fsync {
            file.sync_all()?;
        }
    }
    std::fs::rename(staged, published)?;
    // Durable *visibility*: the rename made the file visible, but only the
    // directory fsync makes that visible-ness survive power loss.
    if durability == Durability::Fsync
        && let Some(dir) = published.parent()
    {
        sync_dir(dir)?;
    }
    Ok(())
}

/// `fsync` a directory, so a `rename()` into or out of it is durable rather than
/// merely visible.
///
/// A renamed *file*'s contents are already synced before it is published; what a
/// power loss can still discard is the directory entry that publishes it. An
/// ordinary process crash needs none of this (the OS page cache survives it) —
/// this is only the hardware/power boundary, which is where "acknowledged" and
/// "applied once" would otherwise stop meaning anything.
///
/// A filesystem that refuses to fsync a directory at all can neither be worked
/// around nor safely papered over with a hard error on every control command, so
/// that one case is tolerated; anything else propagates.
fn sync_dir(dir: &Path) -> Result<()> {
    match std::fs::File::open(dir).and_then(|dir| dir.sync_all()) {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::InvalidInput | std::io::ErrorKind::Unsupported
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

/// How often a driving process refreshes its run's heartbeat.
pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;

/// How stale a heartbeat may get before a live-locked run is reported as hung.
/// Six intervals: a loaded machine can miss a tick or two without being wrong.
pub const HEARTBEAT_STALE_MS: u64 = 6 * HEARTBEAT_INTERVAL_MS;

/// A driving process's liveness beacon: `heartbeat` in the run dir, rewritten on
/// a timer for as long as the driver lives.
///
/// It answers the question the advisory lock cannot: a held lock proves a
/// *process* is alive, not that the run is progressing. Lock held + fresh beat =
/// running; lock held + stale beat = probably hung; lock free + no terminal
/// event = crashed. The beacon is never authority — it carries no run state, and
/// deleting it changes nothing but the report.
pub struct Heartbeat {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Heartbeat {
    /// Start beating for `run_dir` until dropped.
    #[must_use]
    pub fn start(run_dir: &Path) -> Self {
        let path = run_dir.join("heartbeat");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&stop);
        // A thread rather than a write per attempt boundary: one attempt can
        // legitimately run for half an hour, and a beacon that only ticks
        // between attempts would report every long agent call as hung.
        let thread = std::thread::spawn(move || {
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = beat(&path);
                // Woken in short slices so a finished run's thread exits
                // promptly instead of holding the process for a full interval.
                for _ in 0..HEARTBEAT_INTERVAL_MS / 100 {
                    if flag.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Write one beat (timestamp + pid), staged and renamed so a reader never sees a
/// half-written beacon — but deliberately *not* fsynced, unlike the control-inbox
/// renames: losing a beat to a power cut costs nothing but a stale liveness
/// report (see [`stage_then_rename`]).
fn beat(path: &Path) -> Result<()> {
    let line = format!("{} {}\n", now_ms(), std::process::id());
    stage_then_rename(
        &path.with_extension("tmp"),
        path,
        line.as_bytes(),
        Durability::Visible,
    )
}

/// The last recorded beat (epoch ms) of a run, if any.
#[must_use]
pub fn last_beat_ms(run_dir: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(run_dir.join("heartbeat")).ok()?;
    raw.split_whitespace().next()?.parse().ok()
}

/// What a reader can conclude about a run right now: the runtime-facing state.
///
/// One enum, deliberately. The kernel's `Status` says where a run is in its
/// *lifecycle* — it is pure, so it can see neither a process nor the filesystem —
/// while this says what an operator or a parent agent can actually conclude, and
/// it is computed from the journal, the run lock and the heartbeat. Two enums
/// answering one question is how the two drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Liveness {
    /// A process holds the run lock. [`RunSummary::hung`](crate::RunSummary) is
    /// the diagnostic that refines this into "and it is still ticking".
    Live,
    /// Unfinished, and nothing holds the lock — a Ctrl-C pause, an operator
    /// `hex pause`, and a crash all collapse here, because the operator action is
    /// the same for all three: `hex resume` continues it.
    Interrupted,
    /// The run reached a terminal disposition.
    Finished(Disposition),
    /// The journal could not be read or replayed. A listing still shows it —
    /// hiding a broken run is how a run gets lost — and `hex status` prints why.
    Error(String),
}

impl Liveness {
    /// The canonical lowercase name, shared by text and `--json` output.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Liveness::Live => "live",
            Liveness::Interrupted => "interrupted",
            Liveness::Finished(_) => "finished",
            Liveness::Error(_) => "error",
        }
    }

    /// The terminal disposition, when the run has one.
    #[must_use]
    pub const fn disposition(&self) -> Option<Disposition> {
        match self {
            Liveness::Finished(d) => Some(*d),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_run_dir(tag: &str) -> PathBuf {
        crate::test_support::temp_dir(&format!("control-{tag}"))
    }

    /// `queued` is what lets `hex status` distinguish "sent, nobody has looked at
    /// it" from "lost". It must be strictly read-only: if it consumed anything, an
    /// operator checking on a steer would delete it.
    #[test]
    fn queued_lists_unclaimed_commands_in_order_without_consuming_them() {
        let dir = temp_run_dir("queued");
        let inbox = Inbox::new(&dir);
        assert!(
            inbox
                .queued()
                .expect("empty inbox is not an error")
                .is_empty()
        );
        for text in ["first", "second"] {
            inbox
                .send(
                    &Actor::human("kc"),
                    &Command::Steer {
                        text: text.to_owned(),
                    },
                )
                .expect("send");
        }
        let names: Vec<String> = inbox
            .queued()
            .expect("queued")
            .into_iter()
            .map(|e| match e.command {
                Command::Steer { text } => text,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(names, ["first", "second"], "arrival order");
        // Twice, to prove the read did not move anything into `done/`.
        assert_eq!(inbox.queued().expect("queued").len(), 2);
        // And the driver can still claim them afterwards.
        assert!(inbox.claim_next().expect("claim").is_some());
        assert_eq!(inbox.queued().expect("queued").len(), 1);
    }

    /// The property that makes an early return safe: claiming takes *one* command
    /// and leaves the rest queued. Draining the batch up front marked every file
    /// done, so a caller that stopped at the first `respond`/`pause`/`cancel`
    /// discarded the ones behind it — losing a steer in ordinary operation, with
    /// no crash involved.
    #[test]
    fn claiming_takes_one_command_and_leaves_the_rest_queued() {
        let dir = temp_run_dir("claim-one");
        let inbox = Inbox::new(&dir);
        inbox
            .send(
                &Actor::human("kc"),
                &Command::Respond {
                    text: "yes".to_owned(),
                },
            )
            .expect("send respond");
        inbox
            .send(
                &Actor::human("kc"),
                &Command::Steer {
                    text: "USE-THE-V2-API".to_owned(),
                },
            )
            .expect("send steer");

        let first = inbox.claim_next().expect("claim").expect("a command");
        assert_eq!(first.actor, Actor::human("kc"));
        assert_eq!(
            first.command,
            Command::Respond {
                text: "yes".to_owned()
            },
            "oldest first"
        );
        assert_eq!(
            std::fs::read_dir(dir.join("control").join("inbox"))
                .expect("queue")
                .count(),
            1,
            "the unclaimed command must stay queued, not be marked done"
        );
        let second = inbox.claim_next().expect("claim").expect("a command");
        assert_eq!(
            second.command,
            Command::Steer {
                text: "USE-THE-V2-API".to_owned()
            }
        );
        // Consumed exactly once, and the files survive in `done/` as evidence.
        assert!(inbox.claim_next().expect("claim").is_none());
        assert_eq!(
            std::fs::read_dir(dir.join("control").join("done"))
                .expect("done dir")
                .count(),
            2
        );
    }

    /// The temp-then-rename discipline is the whole point: a controller caught
    /// mid-write must not be able to hand the driver half a command.
    #[test]
    fn a_half_written_file_in_tmp_is_never_consumed() {
        let dir = temp_run_dir("torn");
        let inbox = Inbox::new(&dir);
        std::fs::create_dir_all(dir.join("control").join("tmp")).expect("mkdir tmp");
        std::fs::write(
            dir.join("control").join("tmp").join("0000000000001-x.json"),
            "{\"actor\":{\"kind\":\"human\",\"id\":\"kc\"},\"comm",
        )
        .expect("partial write");

        assert!(
            inbox.claim_next().expect("claim").is_none(),
            "a staged, incomplete command must be invisible to the driver"
        );
        // And it stays in tmp/ — claiming must not move what it cannot read.
        assert!(
            dir.join("control")
                .join("tmp")
                .join("0000000000001-x.json")
                .exists()
        );
    }

    #[test]
    fn a_malformed_queued_command_is_discarded_not_replayed() {
        let dir = temp_run_dir("malformed");
        let inbox = Inbox::new(&dir);
        let queue = dir.join("control").join("inbox");
        std::fs::create_dir_all(&queue).expect("mkdir");
        std::fs::write(queue.join("0000000000002-y.json"), "not json at all").expect("write");
        assert!(inbox.claim_next().expect("claim").is_none());
        assert!(
            inbox.claim_next().expect("claim again").is_none(),
            "a rejected command must not be retried forever"
        );
    }

    #[test]
    fn claiming_from_an_absent_inbox_is_not_an_error() {
        let dir = temp_run_dir("absent");
        assert!(Inbox::new(&dir).claim_next().expect("claim").is_none());
    }

    #[test]
    fn heartbeat_writes_a_beat_and_stops_on_drop() {
        let dir = temp_run_dir("beat");
        {
            let _hb = Heartbeat::start(&dir);
            // The first beat is written before the thread sleeps, but give the
            // scheduler room rather than assuming an instant hand-off.
            for _ in 0..50 {
                if last_beat_ms(&dir).is_some() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        let beat = last_beat_ms(&dir).expect("a beat was written");
        assert!(beat > 0);
        assert!(
            now_ms().saturating_sub(beat) < HEARTBEAT_STALE_MS,
            "a fresh beat is not stale"
        );
    }
}
