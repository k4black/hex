//! The append-only JSONL journal: the authoritative history of a run.
//!
//! The runtime is the single writer. Each [`Event`] is one line of JSON,
//! flushed on write. Reads tolerate a torn final line (a crash mid-append),
//! discarding only the trailing partial record — every complete line before it
//! is authoritative. Sequence numbers are monotonic and assigned here.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hex_proto::{Actor, Event, EventBody, PROTOCOL_VERSION};

use crate::error::Result;

/// Current wall-clock time as Unix epoch milliseconds.
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// The result of scanning a journal file: the valid events and the byte offset
/// just past the last complete, newline-terminated, valid record. Trailing
/// bytes with no newline are a torn tail (a crash mid-append) and are excluded.
struct Scan {
    events: Vec<Event>,
    valid_len: u64,
}

/// Scan a journal at the byte/newline level. A record is authoritative only if
/// it is newline-terminated *and* parses; sequences must be contiguous from 0
/// and the run id consistent. A malformed but newline-terminated line is real
/// corruption (not a torn tail) and is an error; only a trailing record with no
/// newline is tolerated.
fn scan(path: &Path) -> Result<Scan> {
    let bytes = std::fs::read(path)?;
    let mut events = Vec::new();
    let mut valid_len: u64 = 0;
    let mut cursor = 0usize;
    let mut expected_seq = 0u64;
    let mut run_id: Option<String> = None;

    while let Some(rel) = bytes[cursor..].iter().position(|&b| b == b'\n') {
        let line_end = cursor + rel + 1; // include the newline
        let raw = &bytes[cursor..cursor + rel];
        cursor = line_end;
        let text = std::str::from_utf8(raw).map_err(|_| corrupt(events.len(), "invalid utf-8"))?;
        if text.trim().is_empty() {
            valid_len = line_end as u64;
            continue;
        }
        let ev: Event = serde_json::from_str(text)
            .map_err(|e| corrupt(events.len(), &format!("malformed record: {e}")))?;
        if ev.schema_version != PROTOCOL_VERSION {
            return Err(corrupt(
                events.len(),
                &format!("unsupported schema_version {}", ev.schema_version),
            ));
        }
        if ev.seq != expected_seq {
            return Err(corrupt(
                events.len(),
                &format!("non-contiguous seq {} (expected {expected_seq})", ev.seq),
            ));
        }
        match &run_id {
            Some(id) if id != &ev.run_id => {
                return Err(corrupt(events.len(), "run id changes mid-journal"));
            }
            None => run_id = Some(ev.run_id.clone()),
            _ => {}
        }
        expected_seq += 1;
        valid_len = line_end as u64;
        events.push(ev);
    }
    // Anything after `cursor` has no terminating newline: a torn tail.
    Ok(Scan { events, valid_len })
}

fn corrupt(index: usize, why: &str) -> crate::error::HexError {
    crate::error::HexError::new(format!("corrupt journal at record {index}: {why}"))
}

/// Read every complete, contiguous event from a journal file, tolerating a torn
/// last line.
///
/// # Errors
/// Fails if the file cannot be read or a non-final record is malformed,
/// out-of-sequence, or from a different run.
pub fn read_all(path: &Path) -> Result<Vec<Event>> {
    Ok(scan(path)?.events)
}

/// The single writer for one run's journal.
#[derive(Debug)]
pub struct Journal {
    file: File,
    path: PathBuf,
    next_seq: u64,
}

impl Journal {
    /// Create a fresh journal (truncating any existing file).
    ///
    /// # Errors
    /// Fails if the file cannot be created.
    pub fn create(path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        Ok(Self {
            file,
            path,
            next_seq: 0,
        })
    }

    /// Open an existing journal for appending, continuing its sequence. If the
    /// file carries a torn tail (a crash mid-append), it is truncated to the
    /// last clean record first so the next append cannot concatenate onto a
    /// partial line.
    ///
    /// # Errors
    /// Fails if the file cannot be read/repaired or opened for append.
    pub fn open_append(path: PathBuf) -> Result<Self> {
        let scanned = scan(&path)?;
        let file_len = std::fs::metadata(&path)?.len();
        if scanned.valid_len < file_len {
            let f = OpenOptions::new().write(true).open(&path)?;
            f.set_len(scanned.valid_len)?;
            f.sync_all()?;
        }
        let next_seq = scanned.events.last().map_or(0, |e| e.seq + 1);
        let file = OpenOptions::new().append(true).open(&path)?;
        Ok(Self {
            file,
            path,
            next_seq,
        })
    }

    /// Path of the underlying file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event, assigning its sequence number and timestamp. Returns
    /// the written event so the caller can fold it through `reduce`.
    ///
    /// # Errors
    /// Fails if the line cannot be written or flushed.
    pub fn append(
        &mut self,
        run_id: &str,
        node_id: Option<&str>,
        attempt_id: Option<&str>,
        actor: Actor,
        body: EventBody,
    ) -> Result<Event> {
        let event = Event {
            schema_version: PROTOCOL_VERSION,
            seq: self.next_seq,
            at_ms: now_ms(),
            run_id: run_id.to_owned(),
            node_id: node_id.map(ToOwned::to_owned),
            attempt_id: attempt_id.map(ToOwned::to_owned),
            actor,
            body,
        };
        self.next_seq += 1;
        let line = serde_json::to_string(&event)?;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hex_proto::Disposition;

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hex-journal-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir.join("events.jsonl")
    }

    #[test]
    fn append_then_read_roundtrips_with_monotonic_seq() {
        let path = temp_path("roundtrip");
        let mut j = Journal::create(path.clone()).expect("create");
        j.append("run_0", None, None, Actor::runtime(), EventBody::RunStarted)
            .expect("append");
        j.append(
            "run_0",
            None,
            None,
            Actor::runtime(),
            EventBody::RunFinished {
                disposition: Disposition::Succeeded,
            },
        )
        .expect("append");
        let events = read_all(&path).expect("read");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 0);
        assert_eq!(events[1].seq, 1);
    }

    #[test]
    fn torn_final_line_is_tolerated() {
        let path = temp_path("torn");
        let mut j = Journal::create(path.clone()).expect("create");
        j.append("run_0", None, None, Actor::runtime(), EventBody::RunStarted)
            .expect("append");
        // Simulate a crash mid-write: append a partial JSON line.
        {
            let mut f = OpenOptions::new().append(true).open(&path).expect("open");
            f.write_all(b"{\"schema_version\":1,\"seq\":1,\"partial")
                .expect("write");
        }
        let events = read_all(&path).expect("read tolerates torn tail");
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn open_append_repairs_a_torn_tail_before_writing() {
        let path = temp_path("repair");
        {
            let mut j = Journal::create(path.clone()).expect("create");
            j.append("run_0", None, None, Actor::runtime(), EventBody::RunStarted)
                .expect("append");
        }
        // Crash mid-write: a valid record with no terminating newline.
        {
            let mut f = OpenOptions::new().append(true).open(&path).expect("open");
            f.write_all(b"{\"schema_version\":1,\"seq\":1,\"partial")
                .expect("write");
        }
        // Reopening must truncate the torn tail, so the next append lands clean.
        let mut j = Journal::open_append(path.clone()).expect("reopen repairs");
        let ev = j
            .append(
                "run_0",
                None,
                None,
                Actor::runtime(),
                EventBody::RunFinished {
                    disposition: Disposition::Succeeded,
                },
            )
            .expect("append after repair");
        assert_eq!(ev.seq, 1, "seq continues cleanly");
        // And the journal reads back as two contiguous records, not corruption.
        let events = read_all(&path).expect("clean read");
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn malformed_terminated_line_is_corruption() {
        let path = temp_path("corrupt");
        {
            let mut j = Journal::create(path.clone()).expect("create");
            j.append("run_0", None, None, Actor::runtime(), EventBody::RunStarted)
                .expect("append");
        }
        // A *newline-terminated* bad line is real corruption, not a torn tail.
        {
            let mut f = OpenOptions::new().append(true).open(&path).expect("open");
            f.write_all(b"{not json}\n").expect("write");
        }
        assert!(read_all(&path).is_err());
    }

    #[test]
    fn open_append_continues_sequence() {
        let path = temp_path("continue");
        {
            let mut j = Journal::create(path.clone()).expect("create");
            j.append("run_0", None, None, Actor::runtime(), EventBody::RunStarted)
                .expect("append");
        }
        let mut j = Journal::open_append(path.clone()).expect("reopen");
        let ev = j
            .append("run_0", None, None, Actor::runtime(), EventBody::AttemptInterrupted)
            .expect("append");
        assert_eq!(ev.seq, 1);
    }
}
