//! Benchmarks for the kernel hot path (`schedule`) and the runtime YAML loader.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use hex_kernel::graph::Graph;
use hex_kernel::{RunState, Status, schedule};
use hex_proto::Disposition;
use hex_runtime::config::Config;

fn loop_graph() -> Graph {
    Graph::builder("bench", "implement")
        .agent("implement", "codex", "do it", &["ready"])
        .command("test", &["true"])
        .terminal("done", Disposition::Succeeded)
        .edge("implement", "ready", "test")
        .edge("test", "passed", "done")
        .edge("test", "failed", "implement")
        .max_visits("implement", 5)
        .require("test", "passed")
        .build()
}

fn bench_schedule(c: &mut Criterion) {
    let graph = loop_graph();
    let state = RunState {
        status: Status::Running,
        current: Some("implement".to_owned()),
        ..RunState::default()
    };
    c.bench_function("kernel_schedule_agent", |b| {
        b.iter(|| schedule(black_box(&graph), black_box(&state), black_box(0)));
    });
}

fn bench_load(c: &mut Criterion) {
    let source = include_str!("../src/presets/critique-loop.yaml");
    let config = Config::default();
    c.bench_function("runtime_load_critique_loop", |b| {
        b.iter(|| hex_runtime::loader::load(black_box(source), black_box(&config)).expect("loads"));
    });
}

criterion_group!(benches, bench_schedule, bench_load);
criterion_main!(benches);
