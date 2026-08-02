//! Process-wide "the operator asked us to stop" flag.
//!
//! Ctrl-C reaches `hex` and nothing else: [`logged_command`] puts every attempt
//! in its own process group (so a deadline can kill the whole tree it spawned),
//! which also takes it *out* of the terminal's foreground group, so the tty's
//! SIGINT never reaches the agent. Without this flag, killing `hex` left the
//! agent running — spending tokens and writing the workspace with no journal,
//! no logs and no notice.
//!
//! The flag lives here, in the lowest crate that has to read it, because the
//! polling wait ([`crate::wait_bounded`]) is what turns a request into a dead
//! process group. The runtime *sets* it from its signal handler; the worker only
//! ever observes it. That keeps the dependency direction intact and keeps this
//! module free of any policy about what an interrupted run should become.
//!
//! [`logged_command`]: crate::agent

use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Ask every in-flight attempt to stop. Idempotent, and safe to call from a
/// signal-handling thread.
pub fn request() {
    REQUESTED.store(true, Ordering::SeqCst);
}

/// Whether a stop has been requested.
#[must_use]
pub fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}

/// Clear the flag.
///
/// Only for tests and for a host that drives several runs in one process: a
/// stale flag would kill the next attempt the instant it started.
pub fn reset() {
    REQUESTED.store(false, Ordering::SeqCst);
}
