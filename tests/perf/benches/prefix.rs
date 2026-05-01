//! Prefix-index lookup latency.
//!
//! Exercises [`cgn_core::prefix::PrefixIndex`] under realistic load:
//! 10k known prefixes, lookups against an unrelated 32-chunk request.

use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_prefix_overlap(c: &mut Criterion) {
    let index = cgn_core::prefix::PrefixIndex::new(Duration::from_secs(60));
    // Seed: 10k known prefixes spread across 8 nodes.
    for i in 0u32..10_000 {
        let chunks = cgn_perf::hash_prompt("llama3-8b", &(i..i + 16).collect::<Vec<_>>());
        let node = format!("node-{}", i % 8);
        for c in &chunks {
            index.insert(*c, &node);
        }
    }

    c.bench_function("prefix_overlap_32", |b| {
        let chunks = cgn_perf::hash_prompt("llama3-8b", &(50_000..50_032).collect::<Vec<_>>());
        b.iter(|| black_box(index.overlap(&chunks)))
    });
}

criterion_group!(benches, bench_prefix_overlap);
criterion_main!(benches);
