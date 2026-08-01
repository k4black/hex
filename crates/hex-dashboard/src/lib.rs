//! `hex-dashboard` — TUI/web viewer (later).
//!
//! Another thin [`hex_runtime::Runtime`] client: a projection consumer
//! that renders what the journal already contains, fed by the same event
//! stream that powers `hex watch` and `--json`. A transport/projection,
//! **never** orchestration — it owns no state and duplicates no kernel logic,
//! though as a full client it may also start and control runs.
//!
//! Status: stub.

use hex_runtime::PROTOCOL_VERSION;

/// Placeholder banner proving the runtime dependency compiles.
#[must_use]
pub fn about() -> String {
    format!("hex-dashboard — projection viewer (protocol v{PROTOCOL_VERSION})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_mentions_protocol() {
        assert!(about().contains("protocol v"));
    }
}
