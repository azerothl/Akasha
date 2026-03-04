# Modèle petit et intégré (onboarding, diagnostics, validation)

## Objectif

Intégrer un **petit modèle de langage embarqué** dans l’application Akasha, utilisé pour :

- **Onboarding** : aider l’utilisateur pendant `akasha init` (explications, choix de provider, dépannage).
- **Diagnostics** : fournir des conseils pour `akasha doctor --advice` lorsque **aucun LLM externe** n’est configuré (Ollama/OpenAI indisponible ou non installé).
- **Validation** : compléter ou remplacer les règles actuelles (ex. détection de prompt injection dans `akasha_core::check_prompt_injection`) par un petit classifieur si pertinent.
- **Réponses simples** : remplacer le stub `akasha_core:core` par de vraies réponses courtes quand aucun provider n’est disponible (message d’accueil, aide minimale).

L’idée est de ne **pas dépendre** d’Ollama ou d’un service cloud pour ces cas limités, tout en gardant les providers externes pour la conversation complète.

---

## Cas d’usage détaillés

| Cas | Aujourd’hui | Avec modèle intégré |
|-----|-------------|----------------------|
| **Init / onboarding** | Prompts CLI fixes, pas de LLM. | Optionnel : questions/réponses courtes, explication des options (Ollama vs OpenAI), dépannage “Ollama ne répond pas”. |
| **Doctor --advice** | Utilise le routeur LLM (Ollama, etc.) + RAG. Si pas de LLM → échec ou stub. | Si aucun provider configuré ou disponible : utiliser le modèle intégré + RAG pour produire un conseil court. |
| **Validation (prompt injection)** | Règles heuristiques dans `akasha_core::check_prompt_injection`. | Optionnel : petit classifieur binaire (safe / suspect) en complément des règles. |
| **Réponses sans provider** | `AkashaCoreProvider` renvoie un message fixe “Configure Ollama or cloud providers…”. | Modèle intégré génère une courte réponse (accueil, redirection vers la doc ou l’init). |

---

## Contraintes

- **Portabilité** : même binaire (ou binaire + fichier de modèle optionnel) sur Windows, Linux, macOS, sans dépendance à un runtime Python ou à un service externe.
- **Taille** : modèle petit (idéalement &lt; 500 Mo, voire &lt; 200 Mo en quantifié) pour ne pas alourdir trop l’installation.
- **Perf** : inférence CPU acceptable (quelques secondes pour une réponse courte), pas d’obligation GPU.
- **Maintenabilité** : stack Rust de préférence (crate d’inférence + format de modèle standard).

---

## Options techniques

### 1. Candle (Hugging Face)

- **Principe** : inférence en Rust, modèles au format SafeTensors / PyTorch convertis.
- **Atouts** : écosystème Hugging Face, modèles variés, pas de binaires C++ (pur Rust après résolution des deps).
- **Limites** : sous Windows, `candle` 0.5 (utilisé par `candle_embed`) a des erreurs de build (rand_distr / half::f16). Versions plus récentes à évaluer.
- **Modèle** : petit modèle type Phi-2 small, TinyLlama, ou modèle “instruction-tuned” léger exporté en SafeTensors.

### 2. GGUF + crate `llm` (rustformers)

- **Principe** : chargement de modèles GGUF (compatibles llama.cpp), inférence CPU en Rust.
- **Atouts** : format très répandu, quantifications (Q4_K_M, etc.) pour réduire taille et coût mémoire.
- **Limites** : le projet rustformers/llm est **archivé** (juin 2024), plus de maintenance active. Peut toutefois rester utilisable pour un modèle fixe.
- **Modèle** : TinyLlama 1.1B, Phi-2, ou modèle “instruct” &lt; 2B en Q4.

### 3. GGUF + autres crates (gguf-llms, etc.)

- **gguf-llms** : parsing / chargement GGUF, pas encore une stack d’inférence complète.
- Autres projets émergents à suivre pour inférence GGUF pure Rust.

### 4. ONNX + tract

- **Principe** : modèle exporté en ONNX, inférence via `tract` (pur Rust).
- **Atouts** : pas de runtime ONNX C++ (évite les soucis de liaison sous Windows).
- **Limites** : peu de modèles de langage “génération de texte” prêts à l’emploi en ONNX pour un usage embarqué ; plutôt adapté à des classifieurs ou petits modèles dédiés (ex. détection de prompt injection).

### 5. Hybride règles + modèle léger

- **Validation** : garder les règles actuelles, ajouter optionnellement un petit classifieur (ONNX ou Candle) pour réduire faux positifs/négatifs.
- **Diagnostics / réponses simples** : templates + RAG (comme aujourd’hui) quand un LLM est dispo ; quand aucun LLM n’est dispo, un seul petit modèle intégré pour les réponses courtes et le conseil diagnostic.

---

## Recommandation (ordre de priorité)

1. **Court terme**  
   - Améliorer le **stub** `akasha_core:core` : messages plus utiles, liens vers `akasha init`, `akasha paths`, doc.  
   - Pour **doctor --advice** sans LLM : réponse structurée à partir du RAG seul (résumés de runbooks) ou message explicite “Configurez un LLM (Ollama, OpenAI) pour des conseils personnalisés”.

2. **Moyen terme**  
   - Évaluer **Candle** avec une version récente (build Windows OK) ou **llm** (GGUF) pour embarquer un petit modèle (0.5B–1B, Q4).  
   - Utiliser ce modèle uniquement en **secours** :  
     - conseil diagnostic quand aucun provider n’est configuré ;  
     - réponses très courtes pour “pas de provider” (remplacement du stub).

3. **Optionnel**  
   - **Onboarding** : assistant optionnel pendant `akasha init` (questions/réponses courtes).  
   - **Validation** : classifieur léger (ONNX/Candle) en complément de `check_prompt_injection`.

---

## Points d’intégration dans le code

- **Routeur LLM** (`akasha_llm`) : le provider `akasha_core` pourrait charger un “embedding model” ou un petit générateur interne au lieu de renvoyer un texte fixe ; ou un nouveau provider `akasha_embedded` qui délègue à ce modèle.
- **Daemon** (`api.rs`) : pour `/api/diagnostic/advice`, si `llm_router.complete(...)` échoue ou si seul le provider stub est disponible, appeler un module “embedded advice” qui utilise le petit modèle + RAG.
- **CLI** (`cmd_init`) : option “mode assistant” qui pose des questions et envoie des prompts au modèle intégré (si présent) pour explications et dépannage.
- **akasha-core** : `check_prompt_injection` reste basé sur les règles ; un module optionnel pourrait appeler un classifieur embarqué si on en ajoute un.

---

## Fichiers et références

- Stub actuel : `crates/akasha-llm/src/provider.rs` (`AkashaCoreProvider`).
- Diagnostic advice : `crates/akasha-daemon/src/api.rs` (POST `/api/diagnostic/advice`).
- Init : `crates/akasha-cli/src/main.rs` (`cmd_init`).
- Validation : `crates/akasha-core/src/security.rs` (`check_prompt_injection`).

---

## POC Candle (en cours)

- **Crate** : `crates/akasha-embedded-llm`.
- **Stack** : `candle-pipelines` 0.0.7 (Qwen3 0.6B) ou **Baguettotron 321M** (feature `baguettotron`, [PleIAs/Baguettotron](https://huggingface.co/PleIAs/Baguettotron)) pour configs à faible ressource ou conversation.
- **Choix du modèle** : `AKASHA_EMBEDDED_MODEL` = `qwen3_0_6b` (défaut) ou `baguettotron`. Pour Baguettotron : compiler le daemon avec `cargo build -p akasha-daemon --features embedded-baguettotron`, puis lancer avec `AKASHA_EMBEDDED_MODEL=baguettotron`.
- **API** : `EmbeddedLlm::new()`, `complete(prompt, max_tokens, temperature)` (bloquant), `is_available()`.
- **Plateforme** : **recommandation WSL2 pour Windows** tant qu’une solution native Windows n’est pas validée ; build et run sous Linux/WSL2. Le build peut passer sous Windows (Candle 0.9.2) selon l’environnement.
- **Suite** : brancher le crate en fallback dans le daemon (diagnostic advice, réponses simples) et documenter l’usage dans le guide.

---

*Document de spécification — à mettre à jour au fur et à mesure des POC (Candle, llm/GGUF) et des décisions de bundle (taille de modèle, optionnel ou inclus par défaut).*
