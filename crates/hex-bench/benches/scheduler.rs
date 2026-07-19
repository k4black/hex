//! Placeholder benchmark for the scheduler's readiness computation.

use criterion::{Criterion, criterion_group, criterion_main};
use hex_core::graph::Graph;
use hex_engine::Scheduler;
use std::hint::black_box;

fn bench_ready(c: &mut Criterion) {
    let graph = Graph::default();
    let scheduler = Scheduler::new();
    c.bench_function("scheduler_ready_empty", |b| {
        b.iter(|| scheduler.ready(black_box(&graph)));
    });
}

criterion_group!(benches, bench_ready);
criterion_main!(benches);
