# Baselines bench embarqué v0.10.0

Résultats de `bench_embedded.ps1` / `bench_embedded.sh`. Coller les mesures post-tag release.

| Date | Machine | Backend | Modèle | TTFT (s) | tok/s | Load E2E (s) | Notes |
|------|---------|---------|--------|----------|-------|--------------|-------|
| 2026-06-21 | dev (placeholder) | candle | Qwen3 0.6B | — | — | — | Exécuter localement avant tag |
| 2026-06-21 | dev (placeholder) | llama_cpp CUDA | Qwen2.5-1.5B Q4 | — | — | — | Gate >20 tok/s GTX 3080+ |

## Commandes

```powershell
# CUDA (daemon + GGUF)
.\spec\dev\quality\bench_embedded.ps1 -Backend llama_cpp -Json

# CPU Candle
.\spec\dev\quality\bench_embedded.ps1 -Backend candle -Json

# Gate release GPU
.\spec\dev\quality\bench_embedded.ps1 -Backend llama_cpp -Strict
```

```bash
BACKEND=candle ./spec/dev/quality/bench_embedded.sh
```

## Seuils wizard UX

- CPU Candle premier message : avertissement **1–3 min** (load + inférence)
- GPU llama-cpp après GGUF : **<30 s** typique pour premier message
