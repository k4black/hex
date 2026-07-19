//! `hex-bench` — cross-crate benchmarks for hex.
//!
//! The deterministic reducer and scheduler are the hot paths worth measuring,
//! so benchmarks that span [`hex_core`] and [`hex_engine`] live here rather than
//! inside a single crate. Benchmark harnesses live under `benches/`.
//!
//! Status: scaffold — one placeholder criterion benchmark.
