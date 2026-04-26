# Compaction hiérarchique mémoire (spec)

## Objectif

Sessions très longues : éviter une seule couche de résumé qui dilue tout. Alignement **Hermes / produits long-context** : résumés **à plusieurs niveaux** + **checkpoints** exploitables par l’orchestrateur.

## Niveaux proposés

1. **Court terme (session)** — inchangé : tours récents + compaction LLM existante (`ShortTermStore`, plafond `MAX_COMPACTIONS_PER_SESSION`).
2. **Segment L1** — lorsque la fenêtre courte dépasse le budget : résumer un **bloc de N tours** en un paragraphe structuré (faits, décisions, outils utilisés, fichiers touchés).
3. **Segment L2** — agréger plusieurs L1 en **méta-résumé** (objectif utilisateur, état du livrable, risques), stocké en mémoire long terme avec `source: session_checkpoint` et lien `relates_to` vers les L1 / épisodes concernés.
4. **Checkpoint explicite** — événement épisodique `session_checkpoint` avec JSON `{ level, summary_ref, token_estimate, at_turn }` pour reprise UI et audit.

## Déclencheurs

- Ratio tokens estimés / `AKASHA_MAX_CONTEXT_TOKENS` (déjà utilisé pour compaction) **et** nombre de tours depuis dernier L2 > seuil configurable (`AKASHA_MEMORY_L2_EVERY_TURNS`, futur).
- Token estimate : utiliser **`ShortTermStore::estimate_tokens_calibrated`** (provider/modèle) pour cohérence avec le routeur.

## Non-objectifs (v1 spec)

- Pas de réécriture automatique des embeddings existants ; nouveaux segments seulement.
- Pas de merge cross-session sauf politique mémoire existante (`memory_store` / filtres session).

## Implémentation

À brancher dans `run_message_via_llm` / orchestrateur après compaction courte : file d’attente « segments » persistée (fichier ou table SQLite via `akasha-store` selon choix produit).
