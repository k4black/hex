//! `hex-dashboard` — TUI/web viewer (later).
//!
//! Another thin [`hex_runtime::Runtime`] client: a projection consumer
//! that renders what the journal already contains, fed by the same event
//! stream that powers `hex watch` and `--json`. A transport/projection,
//! **never** orchestration — it owns no state and duplicates no kernel logic,
//! though as a full client it may also start and control runs.
//!
//! Status: stub. Its first line of real code arrives when it is built.
