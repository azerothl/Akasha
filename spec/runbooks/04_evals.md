# Runbook — Exécuter la suite d'évals (Phase 8 + profil mock CI)

## Objectif

Lancer la suite d'évaluation Akasha pour valider le comportement du core, du RAG et un sous-ensemble **déterministe** de parcours agentiques (sans GPU ni appel LLM réseau).

Les scénarios de **reprise Studio** (REC-01…REC-09, issue #168) ne font **pas** partie de ce runbook : ils restent documentés dans `spec/dev/integrations/studio-durable-recovery.md`.

## Profils

| Profil | Variable | Usage |
|--------|----------|--------|
| **mock CI** (défaut) | `AKASHA_EVALS_PROFILE=mock-ci` | CI GitHub, développement local rapide : 5 evals Phase 8 + 3 scénarios mock agentiques |
| **modèle** | `AKASHA_EVALS_PROFILE=model` | Phase 8 uniquement ; les runs avec vrai modèle (CPU embarqué, CUDA, Ollama, cloud) se font via les runbooks qualité (`bench_ar_protocol.py --live`, benches embarqués, recettes Studio) |

## Prérequis

- Workspace Akasha compilé (`cargo build`)
- Pour les évals runbooks : répertoire `spec/runbooks` présent (inclus dans le dépôt)

## Commandes

Depuis la racine du dépôt Akasha :

```bash
# Profil mock CI (défaut) — 8 scénarios déterministes
cargo run -p akasha-evals --release

# Rapport JSON (commit CI, latence, liste des scénarios)
AKASHA_EVALS_JSON=1 GITHUB_SHA=$(git rev-parse HEAD) cargo run -p akasha-evals --release

# Répertoire spec personnalisé
AKASHA_SPEC_DIR=/chemin/vers/spec cargo run -p akasha-evals --release

# Phase 8 seulement (pas les mocks agentiques)
AKASHA_EVALS_PROFILE=model cargo run -p akasha-evals --release
```

Sur les environnements Cloud Agent / Linux où le linker C++ l'exige pour les dépendances transitives :

```bash
CXX=g++ cargo run -p akasha-evals --release
```

## Sortie attendue

- Liste des tests avec `PASS` ou `FAIL`
- Code de sortie 0 si tous les tests passent, 1 sinon (utilisable en CI)
- Avec `AKASHA_EVALS_JSON=1`, un objet JSON après le résumé texte

## Scénarios (profil mock-ci)

### Phase 8 (existant)

- **security_prompt_injection** : rejet des patterns d'injection de prompt
- **security_redaction** : masquage des secrets en sortie
- **runbooks_retrieval** : RAG renvoie des chunks runbook pour une requête diagnostic
- **runbooks_consistency** : format du résumé guardrail
- **hallucination_non_empty** : contrôle basique de réponses non vides

### Agentique mock (CI, sans réseau LLM)

- **agentic_tools_policy_refusal** : refus d'outil hors profil, écriture hors chemins autorisés, commande bloquée (`akasha-tools`)
- **agentic_llm_provider_fallback** : chaîne primary indisponible → fallback mock réussi (`akasha-llm`)
- **agentic_memory_persisted_restart** : entrée mémoire long terme lisible après fermeture/réouverture SQLite (`akasha-store`)

## Ajouter un scénario

1. Implémenter une fonction `pub fn eval_<nom>() -> EvalResult` dans `crates/akasha-evals/src/lib.rs` ou `src/agentic_mock.rs` (mocks déterministes, pas d'appel réseau).
2. L'enregistrer dans `run_phase8_evals` ou `run_agentic_mock_evals` selon le périmètre.
3. Documenter le nom et la règle de réussite dans ce runbook.
4. Vérifier localement : `cargo run -p akasha-evals --release`.
5. Le job CI `evals-mock-ci` exécute automatiquement le profil `mock-ci`.

## Intégration CI

Le workflow `.github/workflows/ci.yml` définit le job **`evals-mock-ci`** (Ubuntu, sans GPU) :

```yaml
- name: Run Akasha evals (mock CI profile)
  env:
    AKASHA_SPEC_DIR: ${{ github.workspace }}/spec
    AKASHA_EVALS_PROFILE: mock-ci
    AKASHA_EVALS_JSON: "1"
  run: cargo run -p akasha-evals --release
```

**Note :** une refonte du workflow CI est en cours sur la PR codex #165 ; un rebase de la branche evals sera nécessaire après merge de #165 pour résoudre d'éventuels conflits sur `ci.yml`.

## Hors périmètre mock CI

- Reprise / annulation / pause Studio (**REC-01…REC-09**, PR #168)
- Politique après interruption de tâche Studio (REC-08)
- Benchmarks modèle : `spec/dev/quality/bench_ar_protocol.py`, `bench_embedded*.ps1`, GPU CI (`gpu-ci.yml`)
