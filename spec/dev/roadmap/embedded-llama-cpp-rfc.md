# RFC — Backend embarqué llama-cpp-4 (Akasha)

Status: **Implemented (v1)**  
Owner: Akasha core team  
Scope: `akasha-embedded-llm`, `akasha-daemon`, `akasha-cli`, releases Windows CUDA

## Décision

- **Crate** : [`llama-cpp-4`](https://github.com/eugenehp/llama-cpp-rs) (sys: `llama-cpp-sys-4`).
- **Coexistence** : Candle (Qwen3 / Baguettotron) reste le fallback CPU ; llama-cpp-4 = chemin perf GGUF + GPU.
- **Rbitnet** : voie long terme pure Rust — pas de duplication dans ce RFC.

## Backends runtime

| ID | Feature Cargo | Format | Usage |
|----|---------------|--------|-------|
| `llama_cpp` | `llama-cpp`, `llama-cpp-cuda` | GGUF | Défaut `auto` si GGUF présent |
| `candle` | `candle` (default) | SafeTensors HF | Fallback |
| `baguettotron` | `baguettotron` | HF | Faible ressource |

## Variables d'environnement

| Variable | Description |
|----------|-------------|
| `AKASHA_EMBEDDED_BACKEND` | `auto` \| `llama_cpp` \| `candle` \| `baguettotron` |
| `AKASHA_EMBEDDED_GGUF_PATH` | Chemin `.gguf` (sinon `{data_dir}/models/embedded/default.gguf`) |
| `AKASHA_EMBEDDED_N_GPU_LAYERS` | Couches GPU llama.cpp (défaut 99 si CUDA compilé) |
| `AKASHA_EMBEDDED_MODEL` | Inchangé pour Candle (`qwen3_0_6b`, `baguettotron`) |
| `AKASHA_DATA_DIR` | Répertoire données (défaut `~/akasha`) |

## Build

```bash
# CPU llama.cpp
cargo build -p akasha-daemon --features embedded-llama-cpp

# CUDA (Windows/Linux NVIDIA)
cargo build -p akasha-daemon --no-default-features \
  --features embedded,embeddings-tract,embedded-baguettotron,embedded-llama-cpp,embedded-llama-cpp-cuda
```

Prérequis : `clang`, `cmake`, C++17 ; CUDA 11.8+ pour feature CUDA.

## Modèle GGUF v1

- **Défaut** : Qwen2.5-1.5B-Instruct Q4_K_M (~1 Go)
- Manifeste : [`spec/embedded_models.json`](../embedded_models.json)
- CLI : `akasha config models embedded-download`

## Non-objectifs v1

- Remplacer Rbitnet / mistral.rs
- Barre de progression UI (backlog v1.1)
- Migration pure Rust complète de llama.cpp

## Spike build (machine cible)

Valider localement sur une machine NVIDIA (ex. GTX 3080) :

```powershell
cargo build -p akasha-daemon --no-default-features --features embedded,embeddings-tract,embedded-baguettotron,embedded-llama-cpp,embedded-llama-cpp-cuda
akasha config models embedded-download
$env:AKASHA_EMBEDDED_BACKEND = "auto"
./target/release/akasha-daemon.exe
./spec/dev/quality/bench_embedded_llama_cpp.ps1
```

Critères v1 : premier token <2 s (modèle chargé), >20 tok/s génération Q4, streaming visible dans TUI/UI.

## Releases

- `akasha-windows-x86_64` : Candle CPU (inchangé)
- `akasha-windows-x86_64-cuda` : llama-cpp-4 + CUDA
