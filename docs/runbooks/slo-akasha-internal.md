# SLO / SLA internes Akasha (draft opérateur)

## Latence conversation (daemon → premier token stream)

| Indicateur | Cible interne | Source |
|------------|---------------|--------|
| p50 TTFB | &lt; 2 s (hors cold-start embedded) | logs `llm_call`, métriques routeur |
| p95 TTFB | &lt; 8 s | idem |
| Cold-start embedded | &lt; 120 s (1ère charge) | `embedded_loaded`, doc utilisateur |

## Fiabilité provider

| Indicateur | Cible | Action si dépassement |
|------------|-------|------------------------|
| Taux de fallback | &lt; 15 % / heure | Vérifier quotas / clés ; `akasha doctor` + `/api/metrics/summary` |
| Erreurs 429 / 5xx répétées | &lt; 5 % des appels | Activer route secondaire ; réduire concurrence |

## Files d’attente / saturation

| Indicateur | Cible | Source |
|------------|-------|--------|
| Profondeur file orchestrateur | &lt; 50 tâches P0 | `/api/metrics` (queue depth) |
| Tâches bloquées `waiting_human` | alerte si &gt; N | API pending human input |

## Plugins

| Indicateur | Cible | Source |
|------------|-------|--------|
| Erreurs chargement WASM | 0 sur reload | `GET /api/plugins/metrics` (`last_load_errors`) |
| Durée reload | p95 &lt; 2 s | `last_load_ms`, `total_load_ms` / cycles |

## Mémoire

| Indicateur | Cible | Source |
|------------|-------|--------|
| Semantic recall vide | noter tendance | `GET /api/memory/recall-metrics` |

## Runbook incident (15 min)

1. `akasha doctor` (+ `--fix` si config manquante).
2. `curl -s http://127.0.0.1:3876/api/metrics/summary` (si exposé) + `/api/router/metrics`.
3. Vérifier `tools_policy.yaml` / vault (Brave, Cloudflare, GitHub, …).
4. Redémarrer daemon si fuite file ; consulter logs `RUST_LOG=akasha_daemon=debug`.

Ce document est volontairement **interne** ; affinez les seuils avec vos mesures réelles (CI bench E2E, `scripts/bench-e2e.ps1`).
