//! Placeholder benchmarks for the kernel scheduler and the runtime drive loop.

use criterion::{Criterion, criterion_group, criterion_main};
use hex_kernel::graph::Graph;
use hex_kernel::{RunState, schedule};
use hex_runtime::Runtime;
use std::hint::black_box;

fn bench_schedule(c: &mut Criterion) {
    let graph = Graph::default();
    let state = RunState::default();
    c.bench_function("kernel_schedule_empty", |b| {
        b.iter(|| schedule(black_box(&graph), black_box(&state)));
    });
}

fn bench_tick(c: &mut Criterion) {
    let mut runtime = Runtime::new(Graph::default());
    c.bench_function("runtime_tick_empty", |b| {
        b.iter(|| black_box(runtime.tick()));
    });
}

criterion_group!(benches, bench_schedule, bench_tick);
criterion_main!(benches);
