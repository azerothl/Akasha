//! Criterion benches for embedded inference (requires local model / GGUF).
//! Run: cargo bench -p akasha-embedded-llm --bench embedded_inference --features candle

use criterion::{criterion_group, criterion_main, Criterion};

fn bench_status_snapshot(c: &mut Criterion) {
    c.bench_function("status_snapshot", |b| {
        b.iter(|| akasha_embedded_llm::EmbeddedLlm::status_snapshot())
    });
}

criterion_group!(benches, bench_status_snapshot);
criterion_main!(benches);
