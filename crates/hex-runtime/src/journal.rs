//! The append-only JSONL journal: the authoritative history of a run.
//!
//! The runtime is the single writer. Each [`Event`] is one line of JSON,
//! flushed on write. Reads tolerate a torn final line (a crash mid-append),
//! discarding only the trailing partial record — every complete line before it
//! is authoritative. Sequence numbers are monotonic and assigned here.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
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

/// Read every complete event from a journal file, tolerating a torn last line.
///
/// # Errors
/// Fails only if the file cannot be read or a non-final line is malformed.
pub fn read_all(path: &Path) -> Result<Vec<Event>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let lines: Vec<String> = reader.lines().collect::<std::io::Result<_>>()?;
    let mut events = Vec::with_capacity(lines.len());
    let last = lines.len().saturating_sub(1);
    for (i, line) in lines.iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(line) {
            Ok(ev) => events.push(ev),
            Err(e) => {
                // Only the very last line may be torn by a crash mid-write.
                if i == last {
                    break;
                }
                return Err(crate::error::HexError::new(format!(
                    "corrupt journal at line {}: {e}",
                    i + 1
                )));
            }
        }
    }
    Ok(events)
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

    /// Open an existing journal for appending, continuing its sequence.
    ///
    /// # Errors
    /// Fails if the file cannot be read or opened for append.
    pub fn open_append(path: PathBuf) -> Result<Self> {
        let next_seq = read_all(&path)?.last().map_or(0, |e| e.seq + 1);
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
