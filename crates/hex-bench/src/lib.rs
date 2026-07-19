//! `hex-bench` — cross-crate benchmarks for hex.
//!
//! The deterministic kernel (`reduce`/`schedule`) and the runtime drive loop
//! are the hot paths worth measuring, so benchmarks that span [`hex_kernel`]
//! and [`hex_runtime`] live here rather than inside a single crate. Benchmark
//! harnesses live under `benches/`.
//!
//! Status: scaffold — one placeholder criterion benchmark.
