//! Writing to stdout without dying when the reader goes away.
//!
//! `println!` panics on `EPIPE`, so `hex logs <run> | head` ended in a Rust
//! panic and a backtrace hint instead of simply stopping — and piping into
//! `head`, `less` or `grep -q` is the whole point of half these verbs. A closed
//! pipe is a normal end of output, not a failure, so it exits 0 quietly, the way
//! every unix filter does.
//!
//! The usual fix is to restore `SIGPIPE` to its default at startup, which needs
//! `unsafe` — forbidden workspace-wide. Routing output through one place costs
//! about the same and has the side benefit that there is now exactly one
//! function that writes to stdout.

use std::io::Write;

/// Write `args` followed by a newline, or exit if stdout has gone away.
pub fn line(args: std::fmt::Arguments<'_>) {
    finish(writeln!(std::io::stdout(), "{args}"));
}

/// Write `args` with no trailing newline.
pub fn raw(args: std::fmt::Arguments<'_>) {
    finish(write!(std::io::stdout(), "{args}"));
}

/// A broken pipe means the reader stopped caring — that is success. Any other
/// write failure is real (a full disk, a closed fd) and must not be swallowed
/// into a zero exit code, or a script would read "no output" as "nothing to do".
fn finish(result: std::io::Result<()>) {
    match result {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => std::process::exit(0),
        Err(e) => {
            eprintln!("hex: cannot write to stdout: {e}");
            std::process::exit(1);
        }
    }
}

/// `println!` that survives a closed pipe.
#[macro_export]
macro_rules! outln {
    () => { $crate::out::line(format_args!("")) };
    ($($arg:tt)*) => { $crate::out::line(format_args!($($arg)*)) };
}

/// `print!` that survives a closed pipe.
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => { $crate::out::raw(format_args!($($arg)*)) };
}
