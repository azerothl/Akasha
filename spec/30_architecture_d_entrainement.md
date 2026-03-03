# Architecture d’entraînement du “Akasha Core Model”

But : un modèle local qui comprend Akasha et sert à :

onboarding (installation/config)

validation structurelle (health/consistency)

support “ops” (diagnostic)

gouvernance plugins (conformité)

aide aux updates (migration, compat)

## A. Ce que le modèle doit savoir (corpus)

1) Knowledge pack “Spec & API”

Les fichiers /spec (vision, requirements, YAML)

API internes : event model, permissions model, plugin interfaces, state machine

Documentation CLI / UI

“Runbooks” (réparer, redémarrer, dégrader, cluster join)

2) Knowledge pack “Ops & Incident”

Incidents types : crash loop, plugin faulty, API down, corruption DB, split brain cluster

Playbooks : triage -> actions -> validation -> rollback

3) Knowledge pack “Security & Threat model”

Prompt injection patterns

Secret exfiltration patterns

Politiques de redaction logs

RBAC/ABAC rules

## B. Stratégie d’entraînement réaliste

Je recommande une approche RAG-first + fine-tune léger (plutôt que gros fine-tune coûteux).

### Étape 1 — RAG obligatoire

Indexation des specs + runbooks + schémas YAML

Retrieval structuré (par type: “API”, “security”, “ops”, “plugin interface”)

Réponses guidées par “policy prompts” + citations internes (IDs de docs)

### Étape 2 — Fine-tune léger (optionnel mais très utile)
Objectif : apprendre le style “support Akasha” et les patterns de diagnostic.

Dataset :

Paires (Symptômes -> Diagnostic -> Actions sûres)

Q/R onboarding (installation, config)

“Validation checklist” (structure ok ?)

“Migration assistant” (vX -> vY)

“Policy compliance” (ne jamais exposer secrets, proposer alternatives)

Format d’exemples (très efficace) :

tool-usage traces (le modèle apprend à appeler des “diagnostic tools”)

structured outputs (JSON/YAML) pour actions planifiées

### Étape 3 — Eval & Guardrails

Tests d’exfiltration secrets (must fail)

Tests prompt injection (must refuse / sanitize)

Tests de cohérence (ne pas inventer config)

Safety: actions “destructives” interdites sans validation explicite user

## C. Runtime du modèle interne

Toujours dispo (offline)

Accès lecture-only aux specs + logs (redacted)

Peut proposer des plans et patches mais exécution soumise à policy engine

Sorties structurées : diagnosis, confidence, recommended_actions[], risk_level