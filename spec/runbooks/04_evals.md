# Runbook — Exécuter la suite d'évals (Phase 8)

## Objectif

Lancer la suite d'évaluations Akasha (sécurité, runbooks, hallucinations) pour valider le comportement du core et du RAG.

## Prérequis

- Workspace Akasha compilé (`cargo build`)
- Pour les évals runbooks : répertoire `spec/runbooks` présent (inclus dans le dépôt)

## Commandes

Depuis la racine du dépôt Akasha :

```bash
# Avec le répertoire spec par défaut (./spec)
cargo run -p akasha-evals

# Avec un répertoire spec personnalisé
AKASHA_SPEC_DIR=/chemin/vers/spec cargo run -p akasha-evals
```

## Sortie attendue

- Liste des tests avec `PASS` ou `FAIL`
- Code de sortie 0 si tous les tests passent, 1 sinon (utilisable en CI)

## Tests exécutés

- **security_prompt_injection** : rejet des patterns d'injection de prompt
- **security_redaction** : masquage des secrets en sortie
- **runbooks_retrieval** : RAG renvoie des chunks runbook pour une requête diagnostic
- **runbooks_consistency** : format du résumé guardrail
- **hallucination_non_empty** : contrôle basique de réponses non vides

## Intégration CI

Exemple GitHub Actions :

```yaml
- name: Run Akasha evals
  run: cargo run -p akasha-evals
  env:
    AKASHA_SPEC_DIR: ${{ github.workspace }}/spec
```
