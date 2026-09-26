# Bench AR v0.11 — protocole & socle (sans GPU self-hosted)

Complète [embedded_models_research_v0.10.md](../roadmap/embedded_models_research_v0.10.md) et [bench_embedded_results.md](bench_embedded_results.md).  
Les mesures NVIDIA réelles restent un **gate opérateur** (P0 / runner `self-hosted,gpu,nvidia`) — ce document décrit le **socle automatisable**.

## Livrables socle

| Artefact | Rôle |
|----------|------|
| [`bench_ar_protocol.json`](bench_ar_protocol.json) | 5 prompts FR + candidats tier 1–2 + gate A1 + veille tier 3 |
| [`bench_ar_protocol.py`](bench_ar_protocol.py) | Runner `--mock` (CI / sans GPU) et `--live` (daemon) |
| `GET /api/capabilities` | Flags GGUF / vision / mmproj / evidence (taxonomie Camelid) |
| `AKASHA_EMBEDDED_N_BATCH` | Override `n_batch` llama.cpp (défaut 2048 ; boost arch `qwen35`) |

## Taxonomie capabilities (Camelid)

| Classe | Signification |
|--------|----------------|
| `supported` | Prouvé sur backends stock de ce build ; safe « prêt » si evidence runtime OK |
| `evidence_only` | Preuve bench / communauté ; pas défaut produit avant protocole AR + décision A1 |
| `groundwork_only` | Docs / watch (ex. Phi-4-mini hors onboarding, Qwen3.6 ROCmFP4) |

Endpoint : `GET /api/capabilities` (schéma `schema_version: 1`).  
Complète `GET /api/router/embedded-status` (runtime) sans le remplacer.

## Protocole (5 prompts FR)

Voir `prompts[]` dans le JSON. Métriques : **TTFT**, **tok/s**, **load** (live) ; mock émet des valeurs synthétiques pour valider le wiring.

## Commandes

```bash
# Validation CI / cloud agent (pas de GPU)
python3 spec/dev/quality/bench_ar_protocol.py --mock --json

# Live (daemon sur :3876, GGUF présent)
python3 spec/dev/quality/bench_ar_protocol.py --live --candidate qwen2.5-1.5b-instruct-q4

# Matrice ngl 0 vs 99 — PowerShell existant (GPU)
# .\spec\dev\quality\bench_embedded_models.ps1 -Matrix
```

## n_batch (Qwen3.5 hybrid)

Crash historique : `GGML_ASSERT(n_tokens_all <= cparams.n_batch)` sur arch hybrid.  
Mitigation code : `n_batch` effectif ≥ 2048 (env `AKASHA_EMBEDDED_N_BATCH`, hint arch via `AKASHA_EMBEDDED_ARCH=qwen35`).  
Validation produit GPU : coller résultats dans [bench_embedded_results.md](bench_embedded_results.md) avant swap défaut A1.

## Hors scope de ce socle

- Exécution benches NVIDIA sur runner self-hosted
- Décision A1 swap défaut CUDA
- Ajout Gemma / Qwen3-1.7B au manifeste wizard (après preuve FR)
