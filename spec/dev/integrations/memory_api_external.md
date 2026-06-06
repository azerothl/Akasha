# API mémoire externe (subset stable)

Contrat HTTP pour intégrer LangGraph, scripts ou agents externes au hub mémoire Akasha.

## Endpoints

| Méthode | Chemin | Description |
|---------|--------|-------------|
| GET | `/api/memory/export` | Export JSON (`MemoryExportBundle`, schema v1) |
| POST | `/api/memory/import` | Import JSON (body = bundle) |
| GET | `/api/memory/recall-metrics` | Compteurs recall + maintenance |
| GET | `/api/memory/hygiene-status` | Janitor / purge metrics |
| GET | `/api/memory/short-term?session_id=` | Tours court terme |
| GET/POST | `/api/memory/second-brain/*` | UI second brain |

## Outils agent (daemon)

- `memory_search`, `memory_store`, `memory_update`, `memory_delete`, `memory_forget`

## Variables d'environnement

| Variable | Défaut | Effet |
|----------|--------|-------|
| `AKASHA_MEMORY_RRF` | `1` | Fusion RRF keyword + embedding |
| `AKASHA_MEMORY_SCORE_WEIGHTS` | `0.55,0.2,0.15,0.1` | sim, recency, importance, confidence |
| `AKASHA_MEMORY_MAINTENANCE_BUDGET` | `3` | Entrées boost/decay post-recall |
| `AKASHA_MEMORY_FACT_LLM` | off | Extraction faits LLM post-promote |
| `AKASHA_MEMORY_HYGIENE_INTERVAL_SECS` | `3600` | Janitor (0 = off) |

## Identité agent

Fichier `data_dir/agent_identity.yaml` — promote auto au démarrage (`scope=agent`).
