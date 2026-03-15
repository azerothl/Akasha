# Mémoire 4 couches (architecture et implémentation)

**Statut** : Implémenté (Phases 1–6).

## Vue d’ensemble

La mémoire des agents Akasha est organisée en 4 couches, avec ingestion multi-représentation et retrieval composite piloté par un Memory Orchestrator.

## Les 4 couches

1. **Mémoire court terme** — Contexte actif de session : conversation en cours, tâche active, sous-agents. Implémentée par `ShortTermStore` (turns, compaction, persistance JSON). Injectée en `[Contexte récent (cette session)]`.

2. **Mémoire vectorielle** — Contenus sémantiquement proches : conversations résumées, documents, décisions, extraits de tâches. Table `memory_entries` (content, embedding, entity_id, process_id, session_id, title, tags, **importance**, **scope**, **expires_at**). Recherche hybride keyword + embedding. Usage : rappel contextuel large.

3. **Mémoire graphe** — Relations structurées (sujet – prédicat – objet). Table `facts` avec `source_entry_id` vers `memory_entries`. APIs : `get_facts_by_entity`, `get_entity_graph`, `get_related_entities`. Usage : raisonnement relationnel, contexte métier.

4. **Mémoire épisodique** — Journal d’événements significatifs. Table `episodic_events` (event_type, payload, entity_id, process_id, session_id, task_id, importance, scope, tags). Usage : historique narratif, apprentissage des habitudes, audit.

## Métadonnées de contexte (Phase 1)

- **Importance** : banal (0), utile (1), important (2), critique (3), permanent (4). Stockée sur `memory_entries` et `episodic_events`.
- **Scope** : global_user, project, task, channel, plugin, session, agent. Filtre au retrieval.
- **Validity / freshness** : `expires_at` sur `memory_entries` ; par défaut les entrées expirées sont exclues du retrieval.
- **Recency / importance scores** : `recency_score(created_at)`, `importance_score(importance)` exposés dans `akasha-store` pour la fusion (Phase 5).

## Pipeline d’ingestion (Phases 2–4)

À chaque **promote** réussi, l’acteur mémoire :

1. **Document** : insert dans `memory_entries` (avec importance, scope, expires_at selon la source).
2. **Faits** : extraction par `extract_facts_simple(content)` → `insert_fact` pour chaque triplet (lien `source_entry_id`).
3. **Épisode** : émission côté daemon après promote (ex. `memory_promoted`, `user_preference`) vers `episodic_events`.

Les appels `promote` dans le daemon passent désormais entity_id / process_id / session_id et des valeurs par défaut d’importance et scope selon la source (compaction → session/utile, user_fact → global_user/important, etc.).

## Memory Orchestrator (Phase 5)

Module `memory_orchestrator` : fonction `recall_context(client, RecallParams)` qui :

- **Semantic retriever** : `client.search(message, top_k, filter)` avec filtre session optionnel.
- **Project block** : si `suggest_project`, recherche par requête projet.
- **Graph retriever** : `client.get_facts_by_entity(entity_id|process_id, limit)` → section « Relations et faits ».
- **Episodic retriever** : `client.search_episodic(filter session, limit)` → section « Événements récents ».
- **User identity** : si `is_first_message`, recherche « nom prénom utilisateur » pour salutation.

Le contexte fusionné est assemblé dans `FusedMemoryContext` et renvoyé via `to_context_string()`. L’API remplace les anciens appels multiples à `long_term_client.search()` par un seul appel à `recall_context`.

## Policy Retriever (Phase 6)

- **Règles explicites** : `RecallParams.policy_summary` (optionnel), injecté en `[Règles et préférences]`.
- **Préférences apprises** : recherche épisodique avec `event_type = "user_preference"` ; ajoutée au bloc policy.

## Fichiers principaux

| Composant        | Fichiers |
|------------------|----------|
| Store            | `akasha-store/src/long_term_memory.rs`, `episodic_memory.rs`, `facts.rs` |
| Memory actor     | `akasha-daemon/src/memory_actor.rs` (Promote, EmitEvent, SearchEpisodic, GetFactsByEntity) |
| Orchestrator     | `akasha-daemon/src/memory_orchestrator.rs` |
| Intégration prompt | `akasha-daemon/src/api.rs` (recall_context, construction du contexte) |

## Référence

- [spec/46_memory_facts_knowledge_graph.md](46_memory_facts_knowledge_graph.md) — facts et graphe (aligné et étendu par la Phase 3).
- [spec/05_agent_architecture.md](05_agent_architecture.md) — Main Agent, Orchestrator, accès mémoire.
