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
- Ajout Gemma / Qwen3-1.7B au manifeste wizard (après preuve FR + A1)

---

## Run cloud 2026-09-28 — mock (sans GPU)

Commande :

```bash
python3 spec/dev/quality/bench_ar_protocol.py --mock --json
```

| Métrique | Résultat |
|----------|----------|
| Mode | `mock` |
| Lignes | **50** / 50 ok |
| Candidats | `qwen2.5-1.5b-instruct-q4`, `qwen3.5-0.8b-q4`, `qwen3-0.6b-q4`, `gemma3-1b-qat-q4`, `qwen3-1.7b-q4` × 5 prompts × ngl 0/99 |
| tok/s / TTFT | **synthétiques** (`note: synthetic — no inference`) — **ne pas** utiliser pour décision produit |

Artefact brut : coller la sortie `--json` sous `bench_embedded_results_run.json` uniquement pour runs **live** GPU ; le mock ne remplace pas les mesures NVIDIA.

## Décision A1 — défaut CUDA (v0.11)

**Décision documentée** : **garder Qwen2.5-1.5B Q4_K_M** comme défaut CUDA manifeste.

**Motif** : benches AR tier 1–2 **live GPU non exécutés** sur ce cycle (pas de runner `self-hosted,gpu,nvidia` ; cloud agent sans NVIDIA). Les tok/s mock ne sont pas des preuves. Les baselines XPS 2026-06-22 (voir [bench_embedded_results.md](bench_embedded_results.md)) montraient Qwen3.5-0.8B prometteur (~14,9 tok/s) mais avec risque `n_batch` hybrid — non revalidé pour le tag.

**Statut** : décision A1 = **statu quo explicite** ; **swap différé** faute de benches GPU v0.11. Réouvrir après collage opérateur.

## Bloqué opérateur — checklist machine de référence

Voir aussi [GPU_SELF_HOSTED_CI.md](GPU_SELF_HOSTED_CI.md) et section dédiée dans [bench_embedded_results.md](bench_embedded_results.md).

1. Provisionner runner `self-hosted,gpu,nvidia` (ou poste Insider / XPS 4 Go+)
2. Build CUDA / artefact `akasha-windows-x86_64-cuda`
3. `akasha config models embedded-download` (candidats tier 1–2)
4. `python3 spec/dev/quality/bench_ar_protocol.py --live` (ou `bench_embedded.ps1` / `.sh`)
5. Matrice `ngl=0` vs `ngl=99` sur GPU 4 Go
6. Valider `n_batch` / arch `qwen35` (pas de crash hybrid)
7. Coller résultats dans `bench_embedded_results.md` + JSON
8. Trancher A1 (garder 1.5B **ou** promouvoir candidat &lt; ~1,5 Go)

