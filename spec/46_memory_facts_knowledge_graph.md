# Mémoire long terme : facts et knowledge graph (long terme)

**Statut** : Phase 1 (table facts) et Phase 2 (APIs graphe) implémentées. Inspiré du core OpenClaw [openclaw-memory-offline-sqlite](https://github.com/AkashaBot/openclaw-memory-offline-sqlite). Intégration dans le Memory Orchestrator et extraction de faits après promote : voir [spec/47_memory_4_layers.md](47_memory_4_layers.md).

## Objectif

Enrichir la mémoire long terme avec :
- une **table de faits** (sujet, prédicat, objet) indexée et recherchable ;
- un **graphe entités/relations** dérivé des faits et de l’attribution (entity_id, session_id, process_id).

## Éléments prévus

### Phase 1 – Table facts

- Table `facts` : `id`, `subject`, `predicate`, `object`, `source_entry_id` (FK vers memory_entries), `created_at`.
- Index FTS5 sur (subject, predicate, object) pour recherche lexicale.
- APIs store : `insert_fact`, `search_facts(query)`, `get_facts_by_subject(subject)`.
- Extraction de faits : à partir des contenus stockés (ou en post-traitement après promote), extraction simple (patterns ou LLM) pour remplir `facts`.

### Phase 2 – Knowledge graph

- Représentation graphe : nœuds = entités (sujets/objets), arêtes = prédicats.
- APIs : `get_entity_graph(entity_id, depth?)`, `get_related_entities(entity_id)`, `get_graph_stats()`.
- Alimentation : dérivée des `facts` et des champs d’attribution déjà présents dans `memory_entries` (entity_id, process_id, session_id).

## Dépendances

- Mémoire long terme actuelle avec attribution (entity_id, process_id, session_id) et métadonnées (title, tags) — en place.
- Décision sur le moment d’extraction (batch, après chaque promote, ou à la demande).

## Types de relations (`memory_relations.kind`)

La table accepte toute chaîne non vide. **Valeurs recommandées** (pour le graphe et les outils) :

| Origine | Exemples de `kind` |
|--------|---------------------|
| Automatique (similarité d’embedding) | `similar`, `relates_to` (seuils cosinus dans `akasha_store::relation_kind_from_embedding_similarity`) |
| Outil `memory_store` / JSON | `related`, `updates`, `supersedes`, `excludes`, `contradicts`, `supports`, `derived_from`, `same_as`, `spouse`, `child`, `birth_date`, … |
| JSON structuré | Champ optionnel `"memory_edges": [ { "to": "<uuid>", "kind": "excludes" }, ... ]` (voir inférence dans le daemon) |

Heuristiques texte légères (marqueurs « correction », « incompatible », etc.) peuvent forcer `updates` / `excludes` / `contradicts` sur les liens auto-générés par embedding. Variable réservée `AKASHA_MEMORY_EDGE_LLM` : étiquetage LLM futur.

## Référence

Comparaison et pistes détaillées : plan « Comparaison mémoire OpenClaw vs Akasha » (court/moyen/long terme).
