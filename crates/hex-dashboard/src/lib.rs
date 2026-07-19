//! `hex-dashboard` — optional TUI/web viewer (later / after-demand).
//!
//! A pure *consumer* of the projection and event stream. It renders what the
//! journal already contains and must never own state or drive execution — the
//! same event stream that powers `hex watch` and `--json` output feeds it.
//!
//! Depends only on [`hex_core`] projections (and, transitively, `hex-proto`).
//!
//! Status: stub.

use hex_core::protocol_version;

/// Placeholder banner proving the projection dependency compiles.
#[must_use]
pub fn about() -> String {
    format!(
        "hex-dashboard — projection viewer (protocol v{})",
        protocol_version()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_mentions_protocol() {
        assert!(about().contains("protocol v"));
    }
}
