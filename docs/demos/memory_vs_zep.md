# Démo compétitive — Akasha vs Zep

Walkthrough opérateur (~15 min) — Zep cible **mémoire conversationnelle** avec graphe temporel et API cloud ; Akasha combine recall local, bi-temporel léger et observabilité intégrée.

## Prérequis

- Daemon Akasha + accès `memory.db` (via API uniquement en démo publique)
- Variables optionnelles : `AKASHA_MEMORY_RRF=1`, `AKASHA_MEMORY_SCORE_WEIGHTS`

## Narratif produit

| Critère | Zep (typ.) | Akasha |
|---------|------------|--------|
| Modèle | Sessions + summaries + graph cloud | Sessions SQLite + épisodes + faits `valid_from` / `recorded_at` |
| Temporalité | Graph temporel natif | **Partiel** — colonnes bi-temporelles sur faits ; `expires_at` sur entrées |
| Recherche | Hybrid + rerank cloud | RRF local + composite score recency/importance/confidence |
| Observabilité | Dashboard Zep | `GET /api/memory/recall-metrics`, `/api/memory/hygiene-status` |
| Coût | SaaS / self-host enterprise | CPU local, pas de facturation recall |

## Scénario 1 — Session memory + résumés

1. Conversation multi-tours avec `session_id` fixe.

2. `GET /api/memory/short-term?session_id=…` — fenêtre glissante.

3. Forcer compaction (long contexte) → résumé injecté + promotion LT.

**Contraste Zep :** summary automatique côté service ; Akasha compaction **configurable** et plafonnée.

## Scénario 2 — Temporal decay / oubli

1. Créer entrée avec expiration (outil agent ou hygiene).

2. Rechercher avant/après `expires_at` — entrée filtrée hors recall.

3. Montrer maintenance post-recall : boost/decay (`AKASHA_MEMORY_MAINTENANCE_BUDGET`).

4. Janitor périodique : métriques hygiene.

**Point de vente :** oubli **contrôlé local** sans pipeline Zep Cloud ; limite = graphe temporel complet non implémenté (D5 partiel).

## Scénario 3 — Faits bi-temporels (approche Zep graph)

1. Insérer fait avec contexte temporel (conversation mentionnant « avant 2024 j’utilisais X »).

2. Expliquer `insert_fact_with_temporal` / colonnes `valid_from`, `recorded_at`.

3. Honnêteté : politique retrieval « fait obsolète » = P2 — pas encore rerank Zep-like sur timeline.

## Scénario 4 — Métriques & qualité recall

1. Plusieurs requêtes recall via chat.

2. `GET /api/memory/recall-metrics` — compteurs fusion, maintenance, latence.

3. Comparer au dashboard Zep : Akasha orienté **SLO opérateur** intégré au daemon doctor.

## Scénario 5 — Export portable (vs lock-in Zep)

1. `GET /api/memory/export` — bundle schema v1.

2. Montrer champs : entrées LT, faits, métadonnées session.

3. Pas de dépendance project_id Zep ; data_dir = source de vérité.

## Clôture démo

- **Zep gagne** sur graphe temporel riche et scale cloud multi-tenant.
- **Akasha gagne** sur local-first, RRF + graph expand intégré, gouvernance permissions/report, coût zero marginal recall.
- Roadmap : [memory_encryption_rfc.md](../../spec/dev/roadmap/memory_encryption_rfc.md) (A4), bi-temporel D5.

## Références

- [spec/46_memory_facts_knowledge_graph.md](../../spec/46_memory_facts_knowledge_graph.md)
- [memory_landscape_roadmap_matrix.md](../../spec/dev/roadmap/memory_landscape_roadmap_matrix.md) (D5, F1–F2, E5)
