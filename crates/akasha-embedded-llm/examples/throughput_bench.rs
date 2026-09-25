//! Direct embedded throughput bench (bypasses llm_router / agent).
//! Usage:
//!   set AKASHA_EMBEDDED_BACKEND=llama_cpp
//!   set AKASHA_EMBEDDED_GGUF_PATH=C:\path\model.gguf
//!   cargo run -p akasha-embedded-llm --features llama-cpp-cuda --example throughput_bench

use std::time::Instant;

fn main() {
    let prompt = std::env::var("BENCH_PROMPT").unwrap_or_else(|_| {
        "Write exactly 50 short numbered lines, one per line, starting at 1.".into()
    });
    let max_tokens: usize = std::env::var("BENCH_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(128);

    let snap = akasha_embedded_llm::EmbeddedLlm::status_snapshot();
    eprintln!(
        "backend={:?} device={:?} gguf={:?}",
        snap.backend, snap.device, snap.model_path
    );

    let llm = akasha_embedded_llm::EmbeddedLlm::new();

    let load_start = Instant::now();
    // preload is an associated function (no &self), same as calibrate.rs.
    if let Err(e) = akasha_embedded_llm::EmbeddedLlm::preload() {
        eprintln!("preload failed: {e}");
        std::process::exit(1);
    }
    let load_s = load_start.elapsed().as_secs_f64();
    eprintln!("load_s={load_s:.2}");

    let mut first_token: Option<f64> = None;
    let gen_start = Instant::now();
    let text = match llm.complete_stream(
        &prompt,
        Some(max_tokens),
        Some(0.3),
        |chunk| {
            if !chunk.is_empty() && first_token.is_none() {
                first_token = Some(gen_start.elapsed().as_secs_f64());
            }
        },
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("inference failed: {e}");
            std::process::exit(1);
        }
    };
    let gen_s = gen_start.elapsed().as_secs_f64();
    let approx_tokens = text.split_whitespace().count().max(1);
    let tok_per_s = approx_tokens as f64 / gen_s.max(0.001);
    let ttft = first_token.unwrap_or(load_s);

    println!(
        "{{\"ok\":true,\"load_s\":{:.2},\"ttft_s\":{:.2},\"gen_s\":{:.2},\"approx_tokens\":{},\"tok_per_s\":{:.1},\"backend\":\"{}\",\"device\":\"{}\"}}",
        load_s,
        ttft,
        gen_s,
        approx_tokens,
        tok_per_s,
        snap.backend.unwrap_or_else(|| "?".into()),
        snap.device.unwrap_or_else(|| "?".into()),
    );
}
