# Benchmarks prompts et agents — résultats et propositions

Ce document décrit les benchmarks ajoutés pour la construction des prompts et les heuristiques de routage, les optimisations appliquées et les propositions d’amélioration.

## Exécution des benchmarks

Les benchmarks ne font aucun appel LLM ni réseau. Pour compiler et lancer uniquement les benches du daemon (sans dépendances lourdes type ONNX/embeddings si besoin) :

```bash
cargo bench -p akasha-daemon
```

Sur des environnements où le lien avec les dépendances par défaut échoue (ex. Windows + ort_sys), utiliser des features minimales :

```bash
cargo bench -p akasha-daemon --no-default-features --features "embedded"
```

### Benchmarks disponibles

| Nom | Description |
|-----|-------------|
| `build_decomposer_prompt` | Construction de la chaîne du prompt décomposeur (template + message utilisateur). |
| `agent_role_system_prompt_all_types` | Appel de `agent_role_system_prompt` pour chaque type d’agent reconnu (code, search, financial, etc.). |
| `message_suggests_tool_only_action_batch` | Évaluation de `message_suggests_tool_only_action` sur un lot de messages (tool-only, code, mixte). |

Exemple de sortie typique (valeurs indicatives) :

```
build_decomposer_prompt            time:   [1.234 µs 1.245 µs 1.257 µs]
agent_role_system_prompt_all_types time:   [125 ns 128 ns 132 ns]
message_suggests_tool_only_action  time:   [2.45 µs 2.50 µs 2.56 µs]
```

La taille typique du prompt décomposeur (template seul) est d’environ 1 400 caractères ; avec un message utilisateur court, le total reste sous 2 000 caractères.

---

## Optimisations appliquées

### 1. Heuristiques en une seule passe

**Problème** : `message_suggests_tool_only_action` et les quatre rappels (write, web_search, device_camera, image_generation) dans `run_message_via_llm` appelaient chacun une fonction faisant un `to_lowercase()` sur le même message → jusqu’à 5 allocations par requête pour les heuristiques, plus 4 pour les rappels.

**Changement** : Introduction de `MessageIntentFlags` et de `compute_message_intent_flags(message)` qui effectue un seul `to_lowercase()` et remplit tous les flags (save_file, external_info, camera_or_mic, image_generation, code_generation).  
- `message_suggests_tool_only_action` et les helpers publics (ex. `message_suggests_save_file`) s’appuient sur cette fonction.  
- Dans `run_message_via_llm`, on appelle une fois `compute_message_intent_flags(&message)` et on réutilise les flags pour les quatre rappels.

**Effet** : Une seule allocation `to_lowercase` par message pour l’ensemble des heuristiques et des rappels.

### 2. Pré-allocation du préfixe de contexte

**Problème** : `context_prefix` était une `String::new()` puis de nombreux `push_str` → reallocations successives.

**Changement** : `String::with_capacity(8192)` avant d’ajouter APP_CONTEXT, rôle, mémoire, documents, etc.

**Effet** : Moins de reallocations lors de la construction du bloc de contexte (surtout utile quand mémoire long terme + RAG + profil sont présents).

### 3. Template décomposeur en constante

**Problème** : Le prompt décomposeur était construit avec un gros `format!(r#"… {}"#, message)` à chaque appel.

**Changement** : Le texte fixe est dans `DECOMPOSER_PROMPT_TEMPLATE` (`const &str`) ; `build_decomposer_prompt(message)` fait `format!("{}{}", DECOMPOSER_PROMPT_TEMPLATE, message)`.

**Effet** : Lisibilité, et possibilité pour le compilateur de mieux optimiser (chaîne statique). La longueur du template reste inchangée.

---

## Tests ajoutés

### api.rs

- **message_suggests_tool_only_action** : true pour caméra, météo, sauvegarde fichier, génération d’image ; false pour demande explicite de code et pour « écris un script qui prend une photo » (intention code dominante).
- **agent_role_system_prompt** : pour chaque type avec rôle (code, search, financial, documentalist, project_manager, technical_writer, research, security_audit, creative), retour de `Some(s)` non vide ; pour `conversation`, `""` et `schedule`, retour de `None`.

### orchestrator.rs

- **apply_decomposition_override** : extraction en fonction pure `apply_decomposition_override(message, steps)`.
- Tests : un step `code` avec message « Prends une photo » → converti en `conversation` ; step `code` avec « Écris un script Python » → inchangé ; plusieurs steps ou step `search` → pas de modification.

---

## Propositions d’amélioration (suite possible)

1. **Résultats chiffrés** : Exécuter `cargo bench -p akasha-daemon` sur une machine de référence (Linux/Windows) et noter ici les temps moyens et percentiles pour chaque bench, afin de détecter des régressions.
2. **Routage** : Ajouter des cas limites (ex. « écris un script qui fait une recherche web ») en tests ; affiner le prompt du décomposeur ou les heuristiques si besoin.
3. **Taille du prompt décomposeur** : Si le coût tokens côté LLM devient important, envisager une version raccourcie du template (ex. moins d’exemples) ou une estimation en tokens (tiktoken ou approximation ~4 caractères/token) pour surveillance.
4. **Cache rôle par agent** : Pour un même type d’agent, le rôle ne change pas ; un cache `agent_type -> &str` (ou lazy static) pourrait éviter des branches répétées si on appelle souvent `agent_role_system_prompt` pour les mêmes types.
