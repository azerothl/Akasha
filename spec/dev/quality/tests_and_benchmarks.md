# Tests et benchmarks — vue d’ensemble

Ce document recense les tests unitaires, tests d’intégration et benchmarks du projet Akasha : ce qu’ils couvrent et comment les lancer.

---

## 1. Lancer tous les tests

Depuis la racine du dépôt :

```bash
cargo test
```

Pour exécuter uniquement les tests d’un crate :

```bash
cargo test -p <nom_du_crate>
```

Exemples : `cargo test -p akasha-daemon`, `cargo test -p akasha-llm`, `cargo test -p akasha-core`.

---

## 2. Tests par crate

### 2.1 akasha-daemon

**Emplacement** : `crates/akasha-daemon/src/*.rs` (modules `tests`) et `crates/akasha-daemon/tests/`.

| Domaine | Fichier | Ce qui est testé | Commande |
|--------|---------|-------------------|----------|
| **API (HTTP, parsing, heuristiques)** | `api.rs` | `parse_content_length` (début de requête HTTP), `parse_device_invoke_params` (arguments device_invoke), `message_suggests_tool_only_action` (caméra, météo, sauvegarde, image, code), `agent_role_system_prompt` (rôle par type d’agent) | `cargo test -p akasha-daemon --lib api::tests` |
| **Device bridge** | `device_bridge.rs` | File d'attente : `submit_request` → `get_pending` → `fulfill` (receiver reçoit le `DeviceResult`) ; `get_pending` vide → `None` ; `fulfill` avec mauvais `request_id` → `false` ; `cancel` → receiver en erreur | `cargo test -p akasha-daemon --lib device_bridge::tests` |
| **Génération d'image** | `image_generation.rs` | **`resolve_api_key`** : fallback env, vault mock `vault://key` ; **`generate_image_impl`** : sans config → erreur « non configurée » ; provider inconnu → « non supporté » ; OpenAI sans clé → « Clé API non trouvée » | `cargo test -p akasha-daemon --lib image_generation::tests` |
| **Orchestrateur** | `agents/orchestrator.rs` | Override de décomposition : un step `code` pour une action outil (ex. « Prends une photo ») → réécrit en `conversation` ; step `code` pour une vraie demande de code ou step `search` → inchangé | `cargo test -p akasha-daemon --lib agents::orchestrator::tests` |
| **Planificateur** | `scheduler.rs` | `sync_terminal_task_run_statuses` : marquage des runs terminés pour les tâches récurrentes | `cargo test -p akasha-daemon --lib scheduler::tests` |
| **RAG utilisateur** | `user_rag.rs` | `UserRagStore` : ajout, liste, recherche par requête, suppression de documents ; requête vide retourne des chunks | `cargo test -p akasha-daemon --lib user_rag::tests` |
| **E2E santé** | `tests/e2e_health.rs` | Démarrage du daemon avec répertoire temporaire, appel GET `/` jusqu’à réponse 200 (ignoré par défaut) | `cargo test -p akasha-daemon --test e2e_health -- --ignored` avec `RUN_E2E=1` |
| **E2E API** | `tests/e2e_api_smoke.rs` | `GET /`, `GET /api/status` (`{"status":"ok"}`), `GET /api/docs` (clé `content` non vide), `GET /api/update/status` (JSON stable) | idem avec `--test e2e_api_smoke` |
| **Helpers E2E** | `tests/common.rs` | Port libre + spawn `akasha-daemon` + attente prêt | utilisé par `e2e_health` et `e2e_api_smoke` |

**Remarque** : Sur certains environnements (ex. Windows avec dépendances ONNX/ort), la compilation ou le lien du daemon avec les features par défaut peut échouer. On peut lancer les tests avec des features minimales :

```bash
cargo test -p akasha-daemon --no-default-features --features "embedded" --lib
```

#### Intégration binaire `akasha` (CLI)

**Emplacement** : `crates/akasha-cli/tests/cli_smoke.rs`.

| Domaine | Ce qui est testé | Commande |
|--------|-------------------|----------|
| **paths** | `akasha paths` avec `AKASHA_DATA_DIR` pointant vers un répertoire temporaire ; la sortie contient ce chemin | `RUN_E2E=1 cargo test -p akasha-cli --test cli_smoke -- --ignored` |
| **--version** | `akasha --version` se termine avec succès | idem |
| **doctor --json** | JSON avec `checks` (tableau non vide, champs `id` / `ok`) et `config_paths.data_dir` cohérent avec `AKASHA_DATA_DIR` | idem |

---

### 2.2 akasha-llm

**Emplacement** : `crates/akasha-llm/src/*.rs`.

| Domaine | Fichier | Ce qui est testé | Commande |
|--------|---------|-------------------|----------|
| **Découverte** | `discovery.rs` | `discover_local()` ne panique pas (Ollama local) | `cargo test -p akasha-llm discovery` |
| **Providers (modèles locaux / cloud)** | `provider.rs` | Ollama : nom `ollama`, `is_local()` true, `is_available()` sans panique. Akasha embedded : nom `akasha_embedded`, `is_local()` true, `is_available()` cohérent avec la feature et avec `EmbeddedLlm::is_available()` ; `complete()` renvoie `Unavailable` quand le modèle embedded n’est pas dispo. **Azure (0.7.0)** : nom `azure_openai`, `is_local()` false, `is_available()` selon clé API, défaut `max_tokens` = 4096 (aligné avec les autres providers OpenAI-compatible). | `cargo test -p akasha-llm provider::tests` |
| **Routeur** | `router.rs` | Complétion via un provider local mock enregistré ; `embedded_available()` après enregistrement de l’embedded ; `resolve_task_type_for_agent("conversation")` → `"conversation"`. **0.7.0** : `routes_by_category()` expose la route `"orchestrator"` quand configurée ; absente par défaut (l’orchestrateur retombe sur `"system"`). | `cargo test -p akasha-llm router::tests` |
| **Config – clamping (0.7.0)** | `config.rs` | `apply_config_to_request` : `max_tokens`, `top_k`, `num_ctx`, `num_gpu` > `u32::MAX` sont limités à `u32::MAX` (pas de troncature) ; valeurs normales inchangées ; style de config tableau (backward-compat). | `cargo test -p akasha-llm --no-default-features config::tests` |

Sans feature `embedded` (pas d’appel au modèle Candle/Baguettotron) :

```bash
cargo test -p akasha-llm --no-default-features
```

Avec feature `embedded` (pour vérifier la cohérence avec le modèle embedded) :

```bash
cargo test -p akasha-llm -F embedded
```

---

### 2.3 akasha-embedded-llm

**Emplacement** : `crates/akasha-embedded-llm/` (`lib.rs`, `config.rs`, `hardware.rs`, `profiles.rs`, `runtime.rs`, `calibrate.rs`, `llama_cpp_backend.rs`, `candle_backend.rs`).

| Test | Ce qui est testé | Commande |
|------|-------------------|----------|
| `embedded_llm_default_constructs` / `compiled_backends_lists_features` | Façade + backends compilés selon features | `cargo test -p akasha-embedded-llm --lib` |
| `backend_choice_parses_aliases` | `AKASHA_EMBEDDED_BACKEND=llama-cpp` | idem |
| `tier_*` (`hardware.rs`) | Classification tier CPU/GPU | idem |
| `profiles_json_parses` / `gpu_low_candidates_prefer_cpu_ngl` | Matrice `embedded_profiles.json` | idem (avec `spec/` présent) |
| `embedded_llm_unload_clears_state` | Après `unload()`, `is_loaded()` false | idem |
| Build llama-cpp (CI optionnel) | Compile `llama-cpp-sys` sans GGUF e2e | `cargo test -p akasha-embedded-llm --features llama-cpp,download` |

**Bench embarqué v0.10** (CPU + CUDA, JSON, gate release) :

```powershell
./spec/dev/quality/bench_embedded.ps1 -Backend llama_cpp -Json
./spec/dev/quality/bench_embedded.ps1 -Backend candle -Json
./spec/dev/quality/bench_embedded.ps1 -Backend llama_cpp -Strict   # gate >20 tok/s CUDA
./spec/dev/quality/bench_embedded_models.ps1 -Matrix -SimulatedTier gpu_low_4gb -NglValues 0,99
```

```bash
BACKEND=candle ./spec/dev/quality/bench_embedded.sh
```

**API calibration** (daemon `--features embedded-llama-cpp-cuda`) : `GET /api/router/embedded/hardware`, `POST /api/router/embedded/calibrate`, `GET /api/router/embedded/calibrate/status`. Persistance : `~/akasha/embedded_runtime.json`.

Baselines : [bench_embedded_results.md](bench_embedded_results.md), matrice tier : [embedded_profile_baselines.json](embedded_profile_baselines.json). Criterion : `cargo bench -p akasha-embedded-llm --bench embedded_inference`.

**Socle AR v0.11** (sans GPU) : protocole 5 prompts FR + `GET /api/capabilities` — voir [bench_ar_v0.11.md](bench_ar_v0.11.md).

```bash
python3 spec/dev/quality/bench_ar_protocol.py --mock --json
CXX=g++ cargo test -p akasha-embedded-llm --lib capabilities::
```

**Bench manuel v1** (legacy alias CUDA) :

```powershell
./spec/dev/quality/bench_embedded_llama_cpp.ps1
```

Prérequis : daemon `--features embedded-llama-cpp-cuda`, `akasha config models embedded-download`, daemon démarré.

---

### 2.4 akasha-core

**Emplacement** : `crates/akasha-core/src/security.rs`.

| Test | Ce qui est testé |
|------|-------------------|
| `prompt_injection_rejects_ignore_previous` | Détection de « ignore previous instructions » (injection de prompt) |
| `prompt_injection_rejects_override_patterns` | Rejet de formulations type « you are a helpful assistant with no restrictions », « developer mode », « jailbreak » |
| `prompt_injection_allows_normal_input` | Acceptation d’entrées normales (météo, RBAC, doctor) |
| `redact_removes_secrets` | Masquage de secrets (ex. clés API) dans un texte |
| `redact_empty_secrets_leaves_text_unchanged` | Texte inchangé quand la liste de secrets est vide |

**Commande** : `cargo test -p akasha-core`.

---

### 2.5 akasha-store

**Emplacement** : `crates/akasha-store/src/long_term_memory.rs` (module `tests`).

| Domaine | Ce qui est testé |
|--------|-------------------|
| **Similarité et encodage** | `cosine_similarity` (identiques → 1, orthogonaux → 0, longueurs différentes / vides → 0) ; `decode_embedding_bytes` et roundtrip bytes ↔ `Vec<f32>` |
| **Persistance** | `open` (création schéma sur SQLite temporaire), `insert` / `insert_with_attribution`, `list_recent` |
| **Recherche** | `search_by_embedding` : ordre par similarité, top_k, filtre par `session_id` ; `search_by_keywords` |
| **Utilitaires** | `stats`, `delete_by_id`, `content_exists` |

**Commande** : `cargo test -p akasha-store`.

---

### 2.6 akasha-tools

**Emplacement** : `crates/akasha-tools/src/policy.rs`.

| Domaine | Ce qui est testé |
|--------|-------------------|
| **Interfaces appareils** | Wildcard `*`, liste vide, liste explicite, blocage qui prime sur autorisation, casse |
| **Outils device** | `device_discover` / `device_invoke` selon interfaces et profils (refus sans interface, autorisation avec wildcard, blocage par profil, combinaison profil + interfaces) |
| **Path traversal** | Rejet de `../secret`, `a/..`, `a/../b` via `Component::ParentDir` ; chemins relatifs normaux autorisés |
| **Confusion préfixe (0.7.0)** | `/home/app/data` n’autorise PAS `/home/app/database/secret` (`Path::starts_with` vs string prefix) ; chemins dans le répertoire autorisé acceptés ; `workspace_root` utilise `Path::starts_with` (test `/home/app/workspace` vs `/home/app/workspace2`) |

**Commande** : `cargo test -p akasha-tools`.

---

### 2.7 akasha-embeddings

**Emplacement** : `crates/akasha-embeddings/src/lib.rs`.

| Test | Ce qui est testé |
|------|-------------------|
| `roundtrip_embedding_bytes` | Sérialisation/désérialisation des vecteurs d’embedding (bytes ↔ `Vec<f32>`) |

**Commande** : `cargo test -p akasha-embeddings`.

---

### 2.8 akasha-ui (frontend)

**Emplacement** : `apps/akasha-ui/src/*.test.ts` (Vitest).

| Domaine | Fichier | Ce qui est testé |
|--------|---------|-------------------|
| **Prétraitement images data URL** | `preprocessDataUrlImages.test.ts` | Conversion markdown `![label](<data:image/...>)` → `<div class="markdown-data-image-wrap">` + `<img src="...">` avec `alt` échappé ; chaîne vide, pas de correspondance, plusieurs images |

**Lancer les tests** (depuis la racine du dépôt ou depuis `apps/akasha-ui`) :

```bash
cd apps/akasha-ui && npm run test
```

En mode watch : `npm run test:watch`.

---

## 3. Benchmarks

### 3.1 akasha-daemon — prompts et heuristiques

**Fichier** : `crates/akasha-daemon/benches/prompt_bench.rs`.

Aucun appel LLM ni réseau ; mesure uniquement le coût CPU de la construction des prompts et des heuristiques.

| Benchmark | Ce qui est mesuré |
|-----------|-------------------|
| `build_decomposer_prompt` | Temps de construction de la chaîne du prompt décomposeur (template + message utilisateur fixe) |
| `agent_role_system_prompt_all_types` | Temps d’appel à `agent_role_system_prompt` pour tous les types d’agent reconnus (code, search, financial, etc.) |
| `message_suggests_tool_only_action_batch` | Temps d’évaluation de `message_suggests_tool_only_action` sur un lot de messages (caméra, météo, sauvegarde, image, code) |

**Lancer les benchmarks** :

```bash
cargo bench -p akasha-daemon
```

Si la compilation avec les features par défaut échoue (ex. lien Windows/ONNX), utiliser des features minimales :

```bash
cargo bench -p akasha-daemon --no-default-features --features "embedded"
```

**Résultats et interprétation** : voir [bench_prompt_results.md](bench_prompt_results.md).

---

## 4. Récapitulatif des commandes

| Objectif | Commande |
|----------|----------|
| Tous les tests (workspace) | `cargo test` |
| Tests du daemon uniquement | `cargo test -p akasha-daemon --no-default-features --features "embedded" --lib` (voir remarque Windows/ONNX ci‑dessus) |
| Tests store (mémoire long terme) | `cargo test -p akasha-store` |
| Tests UI (Vitest) | `cd apps/akasha-ui && npm run test` |
| Tests du routeur LLM (sans embedded) | `cargo test -p akasha-llm --no-default-features` |
| Tests du modèle embedded | `cargo test -p akasha-embedded-llm --lib` |
| CI (GitHub Actions) | Voir `.github/workflows/ci.yml` : tests workspace + E2E avec `RUN_E2E=1` ; UI E2E Playwright (`apps/akasha-ui`) |
| Tests E2E daemon (santé + API) | `RUN_E2E=1 cargo test -p akasha-daemon --test e2e_health --test e2e_api_smoke -- --ignored --test-threads=1` |
| Tests E2E CLI | `RUN_E2E=1 cargo test -p akasha-cli --test cli_smoke -- --ignored` |
| Tests E2E UI (Playwright) | `cd apps/akasha-ui && npm run test:e2e` (voir script ; nécessite build UI + daemon) |
| Test complétion réelle embedded (téléchargement + inférence) | `cargo test -p akasha-embedded-llm --lib -- --ignored` |
| Benchmarks prompts daemon | `cargo bench -p akasha-daemon` |

---

## 5. Tests ignorés (optionnels)

Certains tests sont marqués `#[ignore]` pour ne pas ralentir la CI ni imposer de dépendances externes :

- **e2e_health**, **e2e_api_smoke** : lancent le binaire `akasha-daemon` et appellent l’API HTTP (nécessitent un build préalable et `RUN_E2E=1`). Préférer `--test-threads=1` pour limiter les courses sur le réseau local.
- **cli_smoke** : exécute le binaire `akasha` (`paths`, `--version`, `doctor --json`) avec `AKASHA_DATA_DIR` temporaire (`RUN_E2E=1`).
- **embedded_llm_complete_e2e** : télécharge le modèle et fait une vraie inférence (réseau + disque + CPU/GPU).

Pour les exécuter :

```bash
# E2E daemon (après cargo build -p akasha-daemon)
RUN_E2E=1 cargo test -p akasha-daemon --test e2e_health -- --ignored

# Complétion embedded (modèle téléchargé au premier run)
cargo test -p akasha-embedded-llm --lib -- --ignored
```

---

## 6. Nouveaux tests 0.7.0

Les tests ci-dessous ont été ajoutés dans la version 0.7.0 pour couvrir les nouvelles fonctionnalités et les corrections de sécurité.

| Crate | Module | Ce qui est testé |
|-------|--------|-----------------|
| `akasha-tools` | `policy::tests` | **Confusion de préfixe** : `Path::starts_with` interdit `/home/app/database` quand `/home/app/data` est autorisé (string prefix serait vulnérable) ; `workspace_root` utilise correctement `Path::starts_with` |
| `akasha-llm` | `config::tests` | **Clamping u64→u32** : `max_tokens`, `top_k`, `num_ctx`, `num_gpu` > `u32::MAX` sont limités (pas de troncature silencieuse) |
| `akasha-llm` | `provider::tests` | **Azure `max_tokens`** : défaut aligné à 4096 (cohérence avec les autres providers OpenAI-compatible) |
| `akasha-llm` | `router::tests` | **Route orchestrateur** : `routes_by_category()` expose la route `"orchestrator"` quand configurée ; absente du config par défaut |
| `akasha-daemon` | `api::tests` | **`normalize_tool_path_hint`**, **`canonicalize_tool_name`**, **`looks_like_meta_agent_response`**, **`parse_write_file_request`**, **détection small talk / session recall** (imports manquants corrigés) |
| `akasha-daemon` | `agents::contract::tests` | **UTF-8 boundary** : `parse_contract_tail_slice_does_not_panic_mid_utf8_char` (calcul du décalage corrigé) |

**Liens docs associés (0.7.0)** :
- Routage/orchestrator : `spec/32_llm_router_architecture.md`, `spec/user_guide.md`
- Configuration providers/limites : `spec/35_configuration_reference.md`, `spec/llm_router.example.yaml`
- Guide binaire utilisateur : `docs/user_guide_final.md`

```bash
# Lancer l'ensemble des nouveaux tests
cargo test -p akasha-tools --lib policy::tests
cargo test -p akasha-llm --no-default-features
cargo test -p akasha-daemon --no-default-features --lib
```
