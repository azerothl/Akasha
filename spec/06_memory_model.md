# Memory Model

Document décrit le **modèle cible**. **Court terme** (session, compaction), **long terme** (SQLite, embeddings portés par l’application) et **récupération par similarité** sont implémentés ; la politique de sélection fine reste à préciser (voir « État d’implémentation »).

---

## Court Terme (cible)

Type: Volatile  
Durée: Session  
Contenu:
- Conversation active
- Task status
- Context immédiat

**Contrainte** : le court terme injecté dans le contexte LLM ne doit pas dépasser la taille de fenêtre du modèle (nombre de tokens max). Si la mémoire court terme dépasse cette limite, elle doit être **compactée** avant envoi au LLM.

## Compaction du court terme (cible)

Lorsque la mémoire court terme (ex. fenêtre des N derniers échanges) devient trop volumineuse pour tenir dans le contexte :

1. **Déclencheur** : estimation en tokens (ou caractères) du contenu court terme à injecter ; si > seuil (ex. 70–80 % de la fenêtre contexte du modèle), déclencher une compaction.
2. **Stratégies possibles** (à préciser en implémentation) :
   - **Résumé** : faire résumer par le LLM les échanges les plus anciens de la fenêtre ; remplacer ces échanges par un bloc « Résumé de la conversation précédente : … » et garder les K derniers échanges en clair.
   - **Déplacement vers long terme** : extraire faits / préférences / décisions des échanges anciens (via politique de sélection), les écrire en long terme, puis remplacer dans le court terme par un court résumé ou une référence (« Contexte antérieur : thème X, décision Y »).
   - **Troncature douce** : garder les M derniers échanges intacts et ajouter en tête un résumé fixe (N tokens max) des échanges encore plus anciens.
3. **Objectif** : le contexte effectivement envoyé au LLM reste sous la limite du modèle tout en conservant l’essentiel (résumé + récents) pour la cohérence de la conversation.

## Long Terme (cible)

Type: Persistant chiffré  
Stockage: Base locale sécurisée  
Indexation:
- Embeddings
- Tags
- Entités

Chiffrement:
- AES-256 minimum

## Politique de Stockage (cible)

**À conserver en long terme :**
- Informations personnelles importantes
- Préférences utilisateur
- Historique de décisions
- Contexte récurrent

**Jamais stocké :**
- Secrets en clair
- Tokens API non chiffrés

## Sélection court terme / long terme (cible)

Pour distinguer ce qui est conservé en long terme de ce qui reste du « bruit » :

- **Critères possibles** (à préciser en implémentation) : importance explicite (ex. « retiens que … »), récurrence (mentionné plusieurs fois), type d’entité (préférence, décision, fait sur l’utilisateur), score d’importance dérivé du flux (LLM ou heuristiques).
- **Court terme** : fenêtre glissante de la conversation courante (N derniers échanges) et statut des tâches ; non persistée entre sessions ou réinitialisable.
- **Long terme** : uniquement les éléments qui passent le filtre de la politique (ci‑dessus) et les critères de sélection ; stockage persistant, indexé (embeddings/tags/entités) et chiffré.

---

## État d’implémentation

| Composant | Statut | Détail |
|-----------|--------|--------|
| Court terme structuré | **Oui** | `ShortTermStore` (daemon) : par `session_id`, fenêtre des N derniers tours (user/assistant/system) ; injectée dans le prompt LLM. `session_id` fourni ou généré dans `POST /api/message`, renvoyé dans la réponse ; TUI et Web UI le conservent et le renvoient pour enchaîner la conversation. |
| Compaction court terme | **Oui** | Si tokens estimés (histoire + message) > 75 % de `AKASHA_MAX_CONTEXT_TOKENS` (défaut 8192), les 50 % plus anciens des tours sont résumés par le LLM et remplacés par un tour système « Résumé de la conversation précédente : … ». **Coût** : un appel LLM (résumé) par compaction. **Plafond** : `MAX_COMPACTIONS_PER_SESSION` (défaut 5) par session ; au-delà, la compaction est refusée (éviter les boucles coûteuses). Si le contexte reste trop long, l’utilisateur peut démarrer une nouvelle session. |
| Long terme | **Oui** | `LongTermStore` (akasha-store) : SQLite `memory.db` dans le data_dir ; entrées avec `content` + `embedding` (blob f32). Recherche par similarité cosinus. **Embeddings** : modèle porté par l’application (crate `akasha-embeddings`, fastembed/ONNX en processus, pas d’application tierce) ; cache du modèle sous `data_dir/embedding_model`. Acteur dédié (thread) pour éviter de passer SQLite entre threads. Promotion automatique des résumés de compaction vers le long terme. Récupération des 5 mémoires les plus pertinentes injectée en préfixe du prompt (« [Mémoire à long terme] »). |
| Politique / sélection | Partiel | Promotion automatique des résumés de compaction ; pas encore de critères explicites (importance, récurrence, type d’entité). |
| Plugin Memory | Stub | Trait `MemoryPlugin` (store/retrieve par clé) dans l’API plugin ; pas d’implémentation ni de branchement dans le flux. |
| RAG | Oui (spec/runbooks) | RAG pack pour la spec et les runbooks (recherche par mots‑clés), pas pour la mémoire utilisateur. |

En résumé : **court terme** et **long terme** sont implémentés. Le modèle d’embeddings est **porté par l’application** (fastembed, inférence locale). Retrieval hybride **RRF + scores recency/importance/confidence** via `akasha-store::memory_fusion` (voir variables ci-dessous).

### Retrieval hybride et flags (2026-06)

| Variable | Défaut | Rôle |
|----------|--------|------|
| `AKASHA_MEMORY_RRF` | `1` | Fusion RRF listes keyword + embedding |
| `AKASHA_MEMORY_SCORE_WEIGHTS` | `0.55,0.2,0.15,0.1` | Poids sim, recency, importance, confidence |
| `AKASHA_MEMORY_MAINTENANCE_BUDGET` | `3` | Boost/decay post-recall (0 = off) |
| `AKASHA_MEMORY_FACT_LLM` | off | Extraction faits LLM après promote |
| `AKASHA_MEMORY_HYGIENE_INTERVAL_SECS` | `3600` | Janitor purge (0 = off) |
| `AKASHA_MEMORY_DECAY_RECALL_THRESHOLD` | `5` | Seuil recall sans useful → decay |

Colonnes maintenance sur `memory_entries` : `confidence`, `last_recalled_at`, `recall_count`, `useful_count`.

---

## Maintenance opportuniste post-retrieval

**Statut : implémenté** (`memory_maintenance.rs`, métriques sur `/api/memory/recall-metrics`).

Objectif: améliorer la qualité mémoire sans ajouter de latence visible côté réponse utilisateur.

Principe:

1. Le pipeline principal récupère et injecte les mémoires pertinentes.
2. Une fois la réponse envoyée, un worker asynchrone exécute une maintenance bornée.
3. Les résultats de maintenance sont persistés avec budget strict (temps et volume).

Tâches de maintenance prévues:

- **Confidence boost**: renforcer les entrées effectivement utiles (retrouvées puis utilisées).
- **Confidence decay**: diminuer légèrement les entrées retrouvées mais répétitivement non utilisées.
- **Renforcement de liens**: créer/renforcer des relations entre mémoires co-utilisées.
- **Gap markers**: enregistrer les contextes avec faible rappel utile pour alimenter les extractions futures.

Contraintes:

- Exécution non bloquante (aucune dépendance dans le chemin critique de réponse).
- Budget par tour (nombre max d’entrées traitées, temps max).
- Backoff automatique en charge élevée du daemon.

Métriques associées (observabilité):

- `memory_retrieval_candidates_total`
- `memory_retrieval_used_total`
- `memory_confidence_boost_total`
- `memory_confidence_decay_total`
- `memory_gap_markers_total`
- `memory_retrieval_usefulness_ratio` (dérivée)

Référence d’architecture: `spec/dev/roadmap/jcode_inspired_integration_rfc.md`.

---

## Mémoire long terme sur Windows

Le backend d’embeddings utilise **fastembed** (ONNX Runtime). Sous Windows, les binaires précompilés d’ONNX peuvent provoquer des **erreurs de liaison** (symboles `__std_*` non résolus) à cause d’un décalage d’ABI entre la toolchain MSVC de Rust et celle avec laquelle ONNX a été compilé.

### Options pour faire tourner la mémoire long terme sur Windows

1. **WSL2 (recommandé)**  
   Compiler et lancer le daemon sous WSL2 (Linux). Même code, même modèle, pas de recompilation d’ONNX.  
   ```bash
   # Dans WSL
   cargo build
   ./target/debug/akasha start --foreground
   ```

2. **Daemon sans mémoire long terme**  
   Le daemon peut être compilé sans les embeddings (pas d’ONNX) :  
   ```bash
   cargo build -p akasha-daemon --no-default-features
   ```  
   Le reste (CLI, TUI) se compile normalement. La mémoire court terme et le RAG restent actifs ; seules la recherche par similarité et la promotion vers le long terme sont désactivées.

3. **Recompiler ONNX Runtime (avancé)**  
   Compiler ONNX Runtime depuis les sources avec la **même version de Visual Studio** que celle utilisée par `rustc` (MSVC), puis faire pointer le crate `ort` vers ce build. Documenté sur [onnxruntime](https://onnxruntime.ai/docs/build/inferencing.html) ; réservé aux utilisateurs à l’aise avec CMake et la toolchain C++ Windows.

4. **Backend tract (pur Rust, Windows)**  
   Le crate `akasha-embeddings` propose un second backend via la feature **`tract`** : inférence ONNX en pur Rust avec `tract-onnx`, sans binaires C++. Compatible Windows. Le daemon peut être compilé avec ce backend à la place de fastembed :  
   ```bash
   cargo build -p akasha-daemon --no-default-features --features embedded,embeddings-tract
   ```  
   Le modèle (all-MiniLM-L6-v2) et le tokenizer sont téléchargés automatiquement au premier usage dans le cache (data_dir/embedding_model). Les embeddings restent en 384 dimensions, compatibles avec la base mémoire existante.

5. **Daemon sans mémoire long terme**  
   Si aucune des options ci-dessus n’est possible :  
   ```bash
   cargo build -p akasha-daemon --no-default-features --features embedded
   ```  
   Mémoire court terme et RAG actifs ; recherche par similarité et promotion long terme désactivées.

---

## Modèle système Akasha (route « system »)

Pour éviter que l’**extraction de faits** (et la décomposition de tâches) dépende du modèle choisi par le routeur pour la conversation (souvent un modèle « chat » peu adapté au format structuré `FACT:`), une **catégorie dédiée** est utilisée : **`system`**.

### Comportement

- **Extraction mémoire** : l’appel LLM qui extrait les faits personnels (nom, préférences, etc.) utilise **toujours** la route `system` (via `CompletionRequest.preferred_task_type = "system"`), et non la classification du prompt.
- **Décomposition** : l’orchestrateur utilise aussi la route `system` pour décomposer la requête utilisateur en sous-tâches (conversation / code / search).
- **Configuration par défaut** : la route `system` pointe vers **akasha_embedded** (modèle intégré type Baguettotron), avec repli sur **akasha_core** si besoin. Ainsi, extraction et décomposition fonctionnent de manière prévisible, y compris sans Ollama ni provider externe.
- **Personnalisation** : l’utilisateur peut définir une autre route pour `system` (Ollama, OpenAI, etc.) via la config du routeur ou la TUI (catégorie « system »), par exemple pour utiliser un petit modèle local plus performant pour les tâches structurées.

### Intérêt

- **Routage fiable** : les tâches « internes » (extraction, décomposition) ne sont plus envoyées au modèle de conversation par défaut, qui peut mal respecter le format `FACT:`.
- **Mise et récupération en mémoire** : l’extraction des faits est déléguée à un modèle dédié (local, Ollama ou provider externe selon la config), ce qui améliore le remplissage de la mémoire long terme.
- **Cohérence** : un seul type de tâche « system » pour tout ce qui relève du fonctionnement interne d’Akasha (router, ajout/extraction mémoire), configurable de façon centralisée.