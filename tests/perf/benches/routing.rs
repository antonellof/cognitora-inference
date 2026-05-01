//! Routing-decision latency.
//!
//! SLO: `cgn_router_routing_decision_seconds` p99 < 500 µs (see
//! `docs/operations/slo.md`). This bench drives just the hashing +
//! prefix-index probe path because that's what dominates the budget.

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput, BenchmarkId};

fn bench_hash_token(c: &mut Criterion) {
    c.bench_function("hash_token", |b| {
        b.iter(|| black_box(cgn_perf::hash_token("llama3-8b", black_box(42))))
    });
}

fn bench_hash_prompt(c: &mut Criterion) {
    let mut group = c.benchmark_group("hash_prompt");
    for n_tokens in [128u32, 512, 2048] {
        let tokens: Vec<u32> = (0..n_tokens).collect();
        group.throughput(Throughput::Elements(n_tokens as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n_tokens), &tokens, |b, t| {
            b.iter(|| black_box(cgn_perf::hash_prompt("llama3-8b", t)))
        });
    }
    group.finish();
}

criterion_group!(benches, bench_hash_token, bench_hash_prompt);
criterion_main!(benches);
