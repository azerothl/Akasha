# akasha-embedded-llm

Embedded LLM for onboarding, diagnostics, and offline replies without an external provider.

## Backends

| Backend | Feature | Model | Device |
|---------|---------|-------|--------|
| **llama-cpp-4** (GGUF) | `llama-cpp`, `llama-cpp-cuda` | Qwen2.5-1.5B-Instruct Q4 (default manifest) | CPU / CUDA / Metal |
| **Candle** (Qwen3 0.6B) | `candle` (default) | Hugging Face SafeTensors | CPU (optional Candle CUDA) |
| **Baguettotron** | `baguettotron` | PleIAs/Baguettotron 321M | CPU |

Runtime selection (`AKASHA_EMBEDDED_BACKEND`):

- **`auto`** (default): llama-cpp if GGUF exists → else Baguettotron if `AKASHA_EMBEDDED_MODEL=baguettotron` → else Candle.
- **`llama_cpp`**, **`candle`**, **`baguettotron`**: force a backend.

GGUF path: `AKASHA_EMBEDDED_GGUF_PATH` or `{data_dir}/models/embedded/default.gguf`.

Download default GGUF:

```bash
akasha config models embedded-download
```

## Build (daemon)

CPU llama-cpp:

```bash
cargo build -p akasha-daemon --no-default-features --features embedded,embeddings-tract,embedded-baguettotron,embedded-llama-cpp
```

Windows NVIDIA (release artifact `akasha-windows-x86_64-cuda`):

```powershell
cargo build -p akasha-daemon --no-default-features --features embedded,embeddings-tract,embedded-baguettotron,embedded-llama-cpp,embedded-llama-cpp-cuda
```

Requires **clang**, **cmake**, and (for CUDA) NVIDIA toolkit + drivers.

## Usage

```rust
use akasha_embedded_llm::EmbeddedLlm;

let llm = EmbeddedLlm::new();
let reply = llm.complete("What is 2+2? One sentence.", Some(64), Some(0.3))?;
```

Streaming: `complete_stream(..., |chunk| { ... })` — llama-cpp emits token-by-token.

## Status

- TUI: `/embedded`
- API: `GET /api/router/embedded-status` (backend, device, model_path)
- CLI: `akasha doctor`

See [spec/34_embedded_small_model.md](../../spec/34_embedded_small_model.md) and [embedded-llama-cpp-rfc.md](../../spec/dev/roadmap/embedded-llama-cpp-rfc.md).

## Bench

Manual throughput (v1 target >20 tok/s on GTX 3080+ with loaded Q4 model):

```powershell
./spec/dev/quality/bench_embedded_llama_cpp.ps1
```
