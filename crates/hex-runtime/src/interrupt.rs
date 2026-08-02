//! Turn Ctrl-C into a recorded, agent-killing stop.
//!
//! Without this, Ctrl-C killed `hex` and nothing else. Every attempt is spawned
//! into its own process group (so a deadline can take down the whole tree it
//! spawned), which also removes it from the terminal's foreground group — so the
//! tty's SIGINT reaches only `hex`, and the agent kept running: spending tokens
//! and writing the workspace with no journal, no logs and no notice. Worse, the
//! run was left with an open attempt and no terminal, so `hex resume` would
//! start a *second* agent alongside the live orphan.
//!
//! Policy lives here rather than in the CLI because it is run supervision, not
//! rendering; the CLI only chooses to install it for a foreground run. The
//! mechanism it drives ([`hex_worker::interrupt`]) sits one crate further in,
//! where the polling wait can act on it.
//!
//! **First** SIGINT/SIGTERM asks the in-flight attempt to stop: the polling wait
//! kills its process group (`SIGTERM`, 2s, `SIGKILL`), the driver journals what
//! the attempt spent and produced, closes it with `AttemptInterrupted`, and
//! pauses the run — so `hex resume` continues it. **Second** signal exits
//! immediately with 130, on the assumption that something is wedged and the
//! operator wants out now.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether a stop has been requested. Re-exported so a client (which depends on
/// the runtime, never on `hex-worker`) can tell an interrupted pause from an
/// operator `pause` when it renders the outcome.
pub use hex_worker::interrupt::requested;

/// Exit status for "killed by SIGINT", by the usual `128 + signal` convention.
/// Only used for the impatient second press — an orderly stop exits 6 (paused).
const EXIT_SIGINT: i32 = 130;

/// Install the handler for the lifetime of the process.
///
/// Idempotent: later calls are no-ops, so a host that drives several runs does
/// not stack handlers. Unix-only; elsewhere this does nothing and Ctrl-C keeps
/// its default behaviour.
///
/// `on_first` runs on the first signal, before the driver notices — the CLI uses
/// it to tell the operator that a kill is in progress, since tearing down an
/// agent's process group takes up to the `SIGTERM` grace period and a silent
/// pause reads as a hang.
pub fn install(on_first: impl Fn() + Send + 'static) {
    static INSTALLED: AtomicBool = AtomicBool::new(false);
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;

        // `Signals` delivers on a normal thread (via a self-pipe), not in the
        // handler itself, so this body is ordinary code — no async-signal-safety
        // constraints, and no `unsafe`, which the workspace forbids.
        let Ok(mut signals) = Signals::new([SIGINT, SIGTERM]) else {
            return; // no handler is better than refusing to run
        };
        std::thread::Builder::new()
            .name("hex-interrupt".to_owned())
            .spawn(move || {
                for _ in &mut signals {
                    if hex_worker::interrupt::requested() {
                        // Already stopping and the operator pressed again: the
                        // orderly path is evidently not working, so leave now.
                        std::process::exit(EXIT_SIGINT);
                    }
                    hex_worker::interrupt::request();
                    on_first();
                }
            })
            .ok();
    }
    #[cfg(not(unix))]
    let _ = on_first;
}
