# Runbook — Exécuter la suite d'évals (Phase 8 + profil mock CI)

## Objectif

Lancer la suite d'évaluation Akasha pour valider le comportement du core, du RAG et un sous-ensemble **déterministe** de parcours agentiques (sans GPU ni appel LLM réseau).

Les scénarios de **reprise Studio** (REC-01…REC-09, issue #168) ne font **pas** partie de ce runbook : ils restent documentés dans `spec/dev/integrations/studio-durable-recovery.md`.

## Profils

| Profil | Variable | Usage |
|--------|----------|--------|
| **mock CI** (défaut) | `AKASHA_EVALS_PROFILE=mock-ci` | CI GitHub, développement local rapide : 5 evals Phase 8 + 4 scénarios agentiques déterministes |
| **Phase 8 statique** | `AKASHA_EVALS_PROFILE=phase8-static` | Les 5 checks statiques Phase 8, sans appel à un modèle |

## Prérequis

- Workspace Akasha compilé (`cargo build`)
- Pour les évals runbooks : répertoire `spec/runbooks` présent (inclus dans le dépôt)

## Commandes

Depuis la racine du dépôt Akasha :

```bash
# Profil mock CI (défaut) — 9 scénarios déterministes
cargo run -p akasha-evals --release

# Rapport JSON (commit, environnement, latence et probes/actions par scénario)
AKASHA_EVALS_JSON=1 GITHUB_SHA=$(git rev-parse HEAD) cargo run -p akasha-evals --release

# Répertoire spec personnalisé
AKASHA_SPEC_DIR=/chemin/vers/spec cargo run -p akasha-evals --release

# Phase 8 seulement (pas les mocks agentiques, aucun modèle appelé)
AKASHA_EVALS_PROFILE=phase8-static cargo run -p akasha-evals --release
```

Sur les environnements Cloud Agent / Linux où le linker C++ l'exige pour les dépendances transitives :

```bash
CXX=g++ cargo run -p akasha-evals --release
```

## Sortie attendue

- Liste des tests avec `PASS` ou `FAIL`
- Code de sortie 0 si tous les tests passent, 1 sinon (utilisable en CI)
- Avec `AKASHA_EVALS_JSON=1`, le JSON inclut le commit, l'OS, l'architecture, le GPU détecté, le provider/modèle déclarés, la latence par scénario et les probes `actions`/`tools`.
- Le profil `mock-ci` ne mesure aucun modèle réel. Le baseline déterministe versionné est `spec/dev/quality/evals_mock_ci_baseline.json`; les performances et résultats de vrais modèles restent à établir séparément.

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
- **agentic_scheduled_task_lifecycle** : runs planifiés queued/completed/cancelled et déduplication conservés après réouverture du store (`akasha-store`)

## Ajouter un scénario

1. Implémenter une fonction `pub fn eval_<nom>() -> EvalResult` dans `crates/akasha-evals/src/lib.rs` ou `src/agentic_mock.rs` (mocks déterministes, pas d'appel réseau).
2. L'enregistrer dans `run_phase8_evals_timed` ou `run_agentic_mock_evals_timed` selon le périmètre.
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

**État au 20 octobre 2026 :** la PR #165 est fusionnée. Le profil `mock-ci` et le job `evals-mock-ci` passent en CI ; le dernier run complet a réussi. Ce résultat ne valide pas des parcours avec modèle réel.

## Hors périmètre mock CI

- Reprise / annulation / pause Studio (**REC-01…REC-09**, PR #168)
- Politique après interruption de tâche Studio (REC-08)
- Benchmarks modèle : `spec/dev/quality/bench_ar_protocol.py`, `bench_embedded*.ps1`, GPU CI (`gpu-ci.yml`)

## Profil modèle réel agentique

Le profil `real-agentic-v1` exécute les cinq parcours d'acceptation avec un daemon isolé et un modèle Ollama explicitement choisi. Il est séparé de `mock-ci` : un passage mock ne valide aucun comportement modèle réel.

### Prérequis et commande

- Ollama doit répondre à l'URL configurée et le modèle doit être installé localement.
- Construire le CLI et le daemon dans la même révision que le script :

```powershell
cargo build --locked -p akasha-cli -p akasha-daemon
python spec/dev/quality/real_agentic_eval.py --model qwen2.5-coder:14b --device cpu --record-baseline --output spec/dev/quality/reports/qwen25-coder14b-cpu-baseline.json
python spec/dev/quality/real_agentic_eval.py --model qwen2.5-coder:14b --device cpu --output spec/dev/quality/reports/qwen25-coder14b-cpu-comparison.json
```

Pour une mesure NVIDIA, choisir `--device cuda`. Le rapport ne valide ce mode que si Ollama signale une allocation VRAM positive pour le modèle demandé. `--device auto` est réservé au diagnostic et produit un no-go, car le périphérique d'exécution n'est pas vérifiable.

Le script crée un répertoire temporaire pour le home, les données et la base du daemon, retire les clés courantes du processus enfant et supprime ces données à la fin. Il n'utilise donc pas le profil Akasha de l'utilisateur. Les prompts sont limités à des scénarios techniques jetables ; le rapport ne conserve ni réponse brute, ni marqueur de mémoire, ni aperçu d'outil. Le modèle réel peut quand même consommer beaucoup de CPU, de mémoire ou de VRAM ; ne lancer qu'un run à la fois.

### Scénarios et seuils

`spec/dev/quality/evals_agentic_real_baseline.json` contient les cinq scénarios requis, le seuil de latence maximal de 360 secondes par scénario, les cas de no-go et les baselines réelles par modèle/périphérique. `--record-baseline` ne peut ajouter une baseline qu'après un run complet réussi sur un commit propre, et ne remplace jamais une baseline existante. Le run suivant compare les cinq scénarios au même modèle et matériel : chaque parcours doit réussir et rester sous le plafond absolu ou sous 2× sa latence de référence + 5 secondes. Le runner contrôle : choix et réussite de `memory_store`, refus effectif d'une écriture en mode Ask, rappel exact par `memory_search` après redémarrage du daemon, métriques de déclenchement et réussite du fallback, puis création/pause/reprise/exécution exacte/annulation d'une tâche planifiée.

Chaque rapport inclut la révision, l'OS, l'architecture, le matériel GPU détecté, le provider et le modèle observés, les compteurs de tokens, les actions/outils, les latences et les indicateurs de vérification du périphérique. `source_tree_dirty` permet d'écarter un rapport issu d'une copie de travail modifiée. Un rapport ne devient une baseline mesurée d'un modèle qu'après une exécution complète réussie ; le fichier de seuils ne prétend pas être une mesure de qualité.

Le job `evals-mock-ci` vérifie aussi le script et sa politique d'isolation sans appeler de modèle : `python3 spec/dev/quality/test_real_agentic_eval.py`. Exécuter les deux runs réels avant de proposer la fermeture de #143. Archiver leurs JSON et la baseline avec la révision exacte. Les résultats échoués restent des résultats échoués et ne doivent pas être masqués par des mocks ou un fallback implicite.
