# Matrice roadmap — rapport « mémoire agents 2026 » vs Akasha

**Source rapport** : export Deep Research (juin 2026), ~2 500 mots, 3 sources (arXiv CMA, arXiv Graph Agent Memory, Le Fil IA hybride).  
**Objectif** : base de priorisation produit/tech — statut **Fait** / **Partiel** / **Gap** / **Spec seule**, avec références code/spec.

**Légende priorité roadmap**

| Priorité | Signification |
|----------|----------------|
| **P0** | Impact utilisateur fort, faible risque ou débloqueur |
| **P1** | Différenciation ou qualité mémoire notable |
| **P2** | Amélioration incrémentale / R&D |
| **P3** | Vision long terme (coût/complexité élevés) |

---

## Synthèse exécutive

| Zone | Fait | Partiel | Gap | Spec seule |
|------|------|---------|-----|------------|
| Fondations (court/long terme, local) | 4 | 2 | 1 | 1 |
| Retrieval hybride | 0 | 2 | 1 | 0 |
| Mémoire structurée / CRUD | 2 | 3 | 1 | 0 |
| Graph RAG | 1 | 4 | 3 | 0 |
| Identité / CMA / Second Brain | 0 | 2 | 4 | 0 |
| Exploitation / decay / janitor | 0 | 3 | 2 | 2 |
| Écosystème (multi-agent, plugins) | 1 | 2 | 2 | 0 |
| **Hors rapport** (atouts Akasha) | 3 | 2 | 0 | 1 |

**Message roadmap** : Akasha a déjà l’ossature du rapport (4 couches, hybride keyword→embedding, graphe, épisodique, orchestrateur). Les gains rapides sont **RRF + fusion recency/importance**, **extraction de faits LLM**, **maintenance boost/decay complète**, **graph expand par défaut mesuré**. La vision CMA/héritage est un **P2–P3** différenciant, pas un prérequis.

---

## Matrice détaillée

### A — Fondations mémoire

| ID | Thème (rapport) | Recommandation rapport | Statut Akasha | Preuves | Priorité | Prochaine action |
|----|-----------------|------------------------|---------------|---------|----------|------------------|
| A1 | Mémoire court terme | Fenêtre session + compaction | **Fait** | `ShortTermStore`, compaction LLM 75 % contexte, `MAX_COMPACTIONS_PER_SESSION` — `spec/06_memory_model.md` | — | — |
| A2 | Mémoire long terme vectorielle | SQLite + embeddings persistants | **Fait** | `memory.db`, `LongTermStore`, `akasha-embeddings` local — `spec/06`, `crates/akasha-store` | — | — |
| A3 | Compaction hiérarchique L1/L2 | Résumés multi-niveaux + checkpoints | **Fait** | `memory_hierarchical.rs` + `session_checkpoint` + liens L2→L1 | — | — |
| A4 | Chiffrement mémoire AES-256 | Stockage long terme chiffré | **Gap** | Cible `spec/06_memory_model.md` ; `memory.db` en clair sur disque ; vault chiffré pour secrets seulement | **P2** | RFC : chiffrement au repos `memory.db` vs dossier data_dir (SQLCipher / fichier vault) |
| A5 | Souveraineté / local | Déploiement local, pas de cloud obligatoire | **Fait** | Embeddings ONNX, SQLite, data_dir `~/akasha` — `AGENTS.md` | — | Documenter dans guide utilisateur (diff vs Mem0 cloud) |

### B — Retrieval hybride (RRF, BM25, sémantique)

| ID | Thème (rapport) | Recommandation rapport | Statut Akasha | Preuves | Priorité | Prochaine action |
|----|-----------------|------------------------|---------------|---------|----------|------------------|
| B1 | Fusion hybride vectoriel + lexical | RRF entre BM25 et embeddings | **Fait** | `memory_fusion.rs`, `AKASHA_MEMORY_RRF` — `memory_actor` Search | — | — |
| B2 | BM25 | Moteur lexical BM25 | **Partiel** | `search_by_keywords` + FTS5 (`long_term_memory.rs`) — pas BM25 classique | **P2** | Évaluer si FTS5 suffit avant d’ajouter BM25 ; sinon crate ou index dédié |
| B3 | Fallback embedding seul | Si lexical vide, vectoriel | **Fait** | Branche `keyword_candidates.is_empty()` → `search_by_embedding` | — | — |
| B4 | Fusion recency / importance | Score composite au retrieval | **Fait** | Composite dans `memory_fusion::composite_score` + `AKASHA_MEMORY_SCORE_WEIGHTS` | — | — |
| B5 | User RAG documents | (hors rapport explicite) | **Partiel** | `retrieve_hybrid` + sidecar `.chunks.json` | **P1** | Indexation async à l'upload |

### C — Mémoire structurée (type Mem0)

| ID | Thème (rapport) | Recommandation rapport | Statut Akasha | Preuves | Priorité | Prochaine action |
|----|-----------------|------------------------|---------------|---------|----------|------------------|
| C1 | Extraction auto faits / préférences | LLM → faits durables CRUD | **Fait** | `memory_fact_extract.rs`, `AKASHA_MEMORY_FACT_LLM` + heuristique `extract_facts_simple` | — | — |
| C2 | API CRUD mémoire | store / search / update / delete | **Fait** | `memory_store`, `memory_search`, `memory_update`, `memory_delete` | — | — |
| C3 | Préférences utilisateur | Profil dynamique inter-sessions | **Fait** | Épisodes `user_preference`, `personality_memory`, `user_profile.json`, policy retriever — `spec/47`, `memory_orchestrator.rs` | — | — |
| C4 | Identité agent (vs user_id seul) | Instance persistante « qui est Akasha » | **Partiel** | `agent_identity.yaml` + bootstrap promote | **P2** | UI édition identité |
| C5 | Oubli sélectif | Decay + suppression ciblée | **Fait** | purge expired + low confidence via hygiene | — | — |

### D — Graph RAG

| ID | Thème (rapport) | Recommandation rapport | Statut Akasha | Preuves | Priorité | Prochaine action |
|----|-----------------|------------------------|---------------|---------|----------|------------------|
| D1 | Table faits SPO | KG sujet–prédicat–objet | **Fait** | `facts`, FTS5, `insert_fact`, orchestrateur — `spec/46`, `spec/47` | — | — |
| D2 | Arêtes entre entrées mémoire | Relations `memory_relations` | **Fait** | Auto similarité embedding + liens explicites `memory_store` / JSON edges — `spec/46` | — | — |
| D3 | Expansion graphe au retrieval | Traversée multi-hop | **Fait** | `graph_expand_hops` profil enriched, 2-hop borné, `[lié:id]` — `memory_orchestrator.rs` | — | — |
| D4 | Graph RAG « full index » | Tout indexer en graphe | **Gap** | Indexation graphe ciblée (promote + edges), pas pipeline GraphRAG communautés | **P2** | Pilote : projets `source project:*` → sous-graphe dédié |
| D5 | Bi-temporel | Temps événement vs temps ingestion | **Partiel** | Colonnes `valid_from`, `recorded_at` sur `facts` + `insert_fact_with_temporal` | **P2** | Politique retrieval fait obsolète |
| D6 | Hypergraphes / RL sur graphe | Recherche état de l’art 2026 | **Gap** | Non prévu | **P3** | Veille seulement sauf cas d’usage produit |
| D7 | Workspace graph | (hors rapport) | **Partiel** | `akasha-workspace-graph`, injection `workspace_graph_top_k` — `api_workspace_graph` | **P1** | Aligner retrieval workspace + mémoire long terme (même score fusion) |

### E — CMA / Second Brain / identité

| ID | Thème (rapport) | Recommandation rapport | Statut Akasha | Preuves | Priorité | Prochaine action |
|----|-----------------|------------------------|---------------|---------|----------|------------------|
| E1 | Gouvernance 4 niveaux (CMA) | Hiérarchie constitutionnelle | **Gap** | Aucune couche gouvernance mémoire distincte | **P3** | Réduire à 2 niveaux produit : « constitution » (fichier) + « opérationnel » (DB) |
| E2 | Cycle de vie (Naissance → Départ) | Héritage entre instances | **Partiel** | `GET/POST /api/memory/export|import`, fork session, `agent_identity.yaml` | **P2** | Hook upgrade daemon |
| E3 | Forking identité | Bifurcation agent | **Partiel** | Fork session/tâche ; filtre `process_id` au recall | **P2** | Clone mémoire par branche |
| E4 | Second Brain central | Hub mémoire multi-agents | **Partiel** | `/api/memory/*`, [memory_api_external.md](../integrations/memory_api_external.md) | — | — |
| E5 | Vs Mem0 / Letta / Zep | Différenciation concurrentielle | **Partiel** | Local + graphe 4 couches ; démos [memory_vs_mem0.md](../../../docs/demos/memory_vs_mem0.md), [memory_vs_letta.md](../../../docs/demos/memory_vs_letta.md), [memory_vs_zep.md](../../../docs/demos/memory_vs_zep.md) | **P1** | — |

### F — Maintenance, decay, coût

| ID | Thème (rapport) | Recommandation rapport | Statut Akasha | Preuves | Priorité | Prochaine action |
|----|-----------------|------------------------|---------------|---------|----------|------------------|
| F1 | Janitor agent | Maintenance fond de tâche | **Fait** | boost/decay/gap + hygiene — `memory_maintenance.rs`, `memory_hygiene.rs` | — | — |
| F2 | Memory decay | Oubli contrôlé | **Fait** | `expires_at`, confidence decay, purge hygiene | — | — |
| F3 | Compression historique | Résumer anciens journaux | **Partiel** | L1/L2 hierarchical ; rollup 90j backlog | **P1** | Job rollup anciennes entrées |
| F4 | Valeur informationnelle | Coût LLM vs importance | **Partiel** | Gate `source_eligible` / importance pour fact LLM | **P2** | Étendre à edge LLM |
| F5 | Métriques mémoire | Observabilité SLO | **Fait** | `/api/memory/recall-metrics`, hygiene-status | — | — |

### G — Écosystème et intégrations

| ID | Thème (rapport) | Recommandation rapport | Statut Akasha | Preuves | Priorité | Prochaine action |
|----|-----------------|------------------------|---------------|---------|----------|------------------|
| G1 | Interop LangGraph / AutoGen | Mémoire non silo | **Partiel** | [memory_api_external.md](../integrations/memory_api_external.md) | **P2** | Exemple Python LangGraph |
| G2 | Plugin Memory | Extension plugins | **Gap** | Trait `MemoryPlugin` stub — `spec/06` | **P3** | Brancher trait sur `memory_actor` ou déléguer à HTTP interne |
| G3 | Multi-agents coordonnés | Second Brain pour sous-agents | **Partiel** | Orchestrateur, session state JSON, agents spécialisés (research, documentalist) | **P1** | Mémoire par `task_id` / `process_id` dans recall systématique |
| G4 | Code RAG | (hors rapport) | **Fait** | `code_rag.rs` hybride symboles + sémantique Code Studio | — | Réutiliser patterns fusion pour B1 |

---

## Atouts Akasha non couverts par le rapport

| ID | Capacité | Statut | Action roadmap |
|----|----------|--------|----------------|
| H1 | Mémoire 4 couches unifiée | **Fait** | Mettre à jour doc marketing / deep research prompts pour citer `spec/47` |
| H2 | Code Studio mémoire isolée | **Fait** | `include_preference_and_personality_episodic: false` — documenter profils mémoire |
| H3 | Deep Research (rapport lui-même) | **Partiel** | Améliorer pipeline : >3 sources, requêtes loguées, matrice auto-générée |
| H4 | RAG spec/runbooks | **Fait** | — |
| H5 | Profils mémoire (`memory_profile`) | **Fait** | UI : exposer réglages semantic_top_k, graph_expand, user_rag |

---

## Phases roadmap suggérées

### Phase 1 — Qualité retrieval (4–6 sem.) — P0

- B1 RRF ou fusion dual-list
- B4 recency + importance dans scoring
- F1 maintenance post-retrieval complète
- C1 extraction faits LLM (optionnelle, budgetée)

**Critère d’acceptation** : bench interne +10 % rappel sur requêtes lexicales exactes ; métriques `memory_*` dans status.

### Phase 2 — Graphe et structure (6–10 sem.) — P1

- D3 graph expand profilé + citations
- C2 `memory_update` + hygiène doublons
- A3 + F3 compaction hiérarchique (spec 54)
- B5 user_rag embeddings
- E5 narrative concurrentielle documentée

### Phase 3 — Identité et pérennité (10–16 sem.) — P2

- E2 protocole héritage mémoire au upgrade
- C4 manifeste identité agent
- D5 bi-temporel léger sur faits
- G1 API mémoire externe
- A4 chiffrement au repos (si exigence FR-005)

### Phase 4 — Vision CMA (backlog) — P3

- E1 gouvernance simplifiée
- D4/D6 GraphRAG large / hypergraphes
- G2 plugins mémoire

---

## Mapping rapport → IDs matrice

| Section rapport | IDs |
|-----------------|-----|
| Mémoire hybride RRF | B1–B4 |
| Gestion structurée Mem0 | C1–C5 |
| Graph RAG | D1–D7 |
| CMA / Second Brain | E1–E5 |
| Vs Mem0/Letta/Zep | E5, C4 |
| Memory decay / janitor | F1–F3 |
| Multi-agents / interop | G1–G3 |
| PostgreSQL / Neo4j | **Écart volontaire** — rester SQLite ; réévaluer seulement si >1M entrées ou requêtes graphe lourdes |

---

## Références

- [spec/06_memory_model.md](../../06_memory_model.md)
- [spec/47_memory_4_layers.md](../../47_memory_4_layers.md)
- [spec/46_memory_facts_knowledge_graph.md](../../46_memory_facts_knowledge_graph.md)
- [spec/54_memory_hierarchical_compaction.md](../../54_memory_hierarchical_compaction.md)
- [spec/dev/roadmap/jcode_inspired_integration_rfc.md](jcode_inspired_integration_rfc.md)
- Code : `crates/akasha-daemon/src/memory_orchestrator.rs`, `memory_actor.rs`, `memory_maintenance.rs`, `crates/akasha-store/src/long_term_memory.rs`, `facts.rs`

**Dernière mise à jour** : 2026-06-04 (matrice initiale post-analyse rapport Deep Research).
