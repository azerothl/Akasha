# Démo compétitive — Akasha vs Mem0

Walkthrough opérateur (~15 min) pour montrer pourquoi Akasha convient en **local-first** sans compte SaaS Mem0.

## Prérequis

- Daemon : `./target/debug/akasha start --foreground` (ou binaire installé)
- Data dir : `akasha paths` → ex. `~/akasha`
- Optionnel : compte Mem0 cloud pour contraste (non requis pour la démo Akasha)

## Narratif produit

| Critère | Mem0 (typ.) | Akasha |
|---------|-------------|--------|
| Hébergement | Cloud API + SDK ; self-host possible mais stack séparée | Binaire + SQLite `memory.db` dans **votre** data_dir |
| Modèle mental | `user_id` + memories CRUD via API | 4 couches (court/long, faits KG, user RAG) + outils agent |
| Retrieval | Vector + filtres metadata | RRF keyword + embedding, recency/importance, graph expand |
| Fact extraction | Pipeline Mem0 auto | `AKASHA_MEMORY_FACT_LLM=1` + heuristique `extract_facts_simple` |
| Export / portabilité | API export vendor | `GET /api/memory/export`, `POST /api/memory/import` |

## Scénario 1 — « Retiens ma préférence »

1. Envoyer via TUI ou `POST /api/message` :
   > Je préfère les réponses courtes et sans emoji. Retiens-le.

2. Vérifier promotion LT :
   ```bash
   akasha memory search "réponses courtes" --limit 5
   ```
   ou `GET /api/memory/second-brain/overview`

3. **Nouvelle session** (`session_id` différent) — même préférence injectée en préfixe recall.

**Point de vente :** pas de clé API Mem0 ; embeddings ONNX locaux (`embedding_model/`).

## Scénario 2 — CRUD explicite (parité Mem0 store/update/delete)

1. Stocker un fait :
   ```bash
   curl -s http://127.0.0.1:3876/api/message -H 'Content-Type: application/json' \
     -d '{"message":"Utilise memory_store pour enregistrer: mon IDE principal est Cursor."}'
   ```

2. Mettre à jour / oublier via outils `memory_update`, `memory_delete` (ou Second Brain UI).

3. Montrer hygiène : `GET /api/memory/hygiene-status`, janitor `AKASHA_MEMORY_HYGIENE_INTERVAL_SECS`.

## Scénario 3 — Retrieval hybride (différenciation)

1. Indexer une entrée avec terme rare exact + synonyme sémantique.

2. Requête **mot-clé exact** → branche FTS5 / keywords.

3. Requête **paraphrase** → branche embedding ; montrer fusion RRF :
   ```bash
   # défaut AKASHA_MEMORY_RRF=1
   curl -s 'http://127.0.0.1:3876/api/memory/recall-metrics'
   ```

4. Optionnel : activer multi-query (`AKASHA_MEMORY_MULTI_QUERY=1`) pour contraster avec Mem0 seul vectoriel.

## Scénario 4 — User RAG (documents)

1. `POST /api/user-rag/documents` (fichier PDF/MD).

2. `GET /api/user-rag/documents/:id/status` — index async.

3. Question agent avec citation chunks — comparer à Mem0 « project memory » cloud.

## Clôture démo

- **Souveraineté :** `memory.db` + vault sur disque ; voir gap chiffrement [memory_encryption_rfc.md](../../spec/dev/roadmap/memory_encryption_rfc.md).
- **Intégration externe :** [memory_api_external.md](../../spec/dev/integrations/memory_api_external.md) — subset stable pour LangGraph sans SDK Mem0.

## Références

- [spec/47_memory_4_layers.md](../../spec/47_memory_4_layers.md)
- [memory_landscape_roadmap_matrix.md](../../spec/dev/roadmap/memory_landscape_roadmap_matrix.md) (C1–C5, E5)
