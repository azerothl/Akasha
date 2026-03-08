# Index des spécifications Akasha

Ce dossier contient la documentation technique et fonctionnelle du projet. Les documents sont regroupés par thème pour faciliter la navigation.

---

## Sommaire

1. [Vision et périmètre](#1-vision-et-périmètre)
2. [Exigences et personas](#2-exigences-et-personas)
3. [Architecture](#3-architecture)
4. [Guide utilisateur et onboarding](#4-guide-utilisateur-et-onboarding)
5. [Runbooks](#5-runbooks)
6. [Plans et feuilles de route](#6-plans-et-feuilles-de-route)
7. [Documents obsolètes ou doublons](#7-documents-obsolètes-ou-doublons)

---

## 1. Vision et périmètre

| Document | Description |
|----------|-------------|
| [00_vision.md](00_vision.md) | Vision du projet, principes directeurs, extension stratégique (standalone, cluster, fallback) |
| [01_scope.md](01_scope.md) | Périmètre v1 : inclus / exclu, hypothèses, contraintes |
| [01_functional_requirements.md](01_functional_requirements.md) | Exigences fonctionnelles (FR) |
| [02_non_functional_requirements.md](02_non_functional_requirements.md) | Exigences non fonctionnelles |
| [03_fonctional_requirements.md](03_fonctional_requirements.md) | Exigences fonctionnelles (détail) |
| [04_non_fonctional_requirements.md](04_non_fonctional_requirements.md) | Exigences non fonctionnelles (détail) |
| [02_personas.md](02_personas.md) | Personas utilisateurs (Alex, Sarah, Marc, Julie) |

---

## 2. Exigences et personas

Voir aussi les documents listés en [§1](#1-vision-et-périmètre) (scope, FR, NFR, personas).

---

## 3. Architecture

### Général

| Document | Description |
|----------|-------------|
| [05_agent_architecture.md](05_agent_architecture.md) | Architecture agentique : Main Agent, Orchestrator, agents spécialisés |
| [29_stack_technique.md](29_stack_technique.md) | Stack technique : langages, bus d’événements, BDD, crypto, sandboxing, cluster |
| [18_deployment_architecture.md](18_deployment_architecture.md) | Architecture de déploiement |

### Sécurité et mémoire

| Document | Description |
|----------|-------------|
| [07_security_model.md](07_security_model.md) | Modèle de sécurité : secrets, isolation, prompt injection, logs |
| [12_threat_model.md](12_threat_model.md) | Modèle de menaces |
| [06_memory_model.md](06_memory_model.md) | Modèle mémoire : court terme, long terme, politique de stockage |

### Runtime et résilience

| Document | Description |
|----------|-------------|
| [19_runtime_resilience.md](19_runtime_resilience.md) | Supervision, redémarrage auto, auto-fix, persistance de session |
| [26_immutable_log.md](26_immutable_log.md) | Journal immuable (append-only, hash chain) |
| [25_degraded_mode.md](25_degraded_mode.md) | Mode dégradé (providers locaux uniquement) |

### LLM et fallback

| Document | Description |
|----------|-------------|
| [32_llm_router_architecture.md](32_llm_router_architecture.md) | Architecture du LLM Router : classifier, providers, fallback, métriques |
| [24_model_fallback.md](24_model_fallback.md) | Stratégie de fallback et types de tâches |
| [28_internal_model.md](28_internal_model.md) | Akasha Core Model (modèle interne natif) |
| [30_architecture_d_entrainement.md](30_architecture_d_entrainement.md) | Architecture d’entraînement du Core Model (RAG, fine-tune, evals) |
| [34_embedded_small_model.md](34_embedded_small_model.md) | **Modèle petit et intégré** : onboarding, diagnostics, validation, réponses simples (sans LLM externe) — objectif et options techniques |

### Canaux et cluster

| Document | Description |
|----------|-------------|
| [22_multichannel_architecture.md](22_multichannel_architecture.md) | Architecture multi-canal (Slack, Discord, Telegram, etc.) |
| [20_cluster_architecture.md](20_cluster_architecture.md) | Mode cluster (NATS, élection de leader, tolérance de panne) |

### Plugins

| Document | Description |
|----------|-------------|
| [16_plugin_architecture.md](16_plugin_architecture.md) | Architecture des plugins (types, règles) |
| [27_plugin_reputation_system.md](27_plugin_reputation_system.md) | Système de réputation des plugins |

### Agents, outils, skills, orchestrateur

| Document | Description |
|----------|-------------|
| [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md) | **Feuille de route** : outils machine, politique, conteneur, skills chargeables, orchestrateur seul point d’entrée, sous-agents, onglets UI Agents/Actions. État d’implémentation. |

### UI et Scheduler

| Document | Description |
|----------|-------------|
| [36_ui_architecture.md](36_ui_architecture.md) | Architecture UI : onglets Chat, Tâches (Task Center), Calendrier ; transport temps réel (WS/SSE, NATS). |
| [37_scheduler_design.md](37_scheduler_design.md) | Scheduler : standalone (tick, persistance, dedup), cluster (leader, workers, event log). |

---

## 4. Guide utilisateur et onboarding

| Document | Description |
|----------|-------------|
| [user_guide.md](user_guide.md) | **Guide utilisateur complet** : commandes CLI, config, interfaces (TUI, Web), commandes slash, canaux, build. Servi par le daemon via `GET /api/docs` et affiché dans l’onglet Doc des interfaces. |
| [onboarding.md](onboarding.md) | Premier lancement pas à pas : build, init, vault, Ollama, daemon, doctor, canaux, evals. |
| [distribution.md](distribution.md) | **Distribution** : binaires + docs/user_guide.md dans le zip, créer une release, option Tauri desktop. |
| [../docs/user_guide_final.md](../docs/user_guide_final.md) | **Guide utilisateur final** (binaires uniquement, sans référence au code). Livré dans le zip sous docs/user_guide.md. |

**Référence complète** (formats, types de données, cas d’usage) : [35_configuration_reference.md](35_configuration_reference.md).

Exemples de configuration :

- [llm_router.example.yaml](llm_router.example.yaml) — Routeur LLM (global, providers, task_types, system)
- [tools_policy.example.yaml](tools_policy.example.yaml) — Politique des outils machine (chemins, commandes, timeout)
- [cluster.example.yaml](cluster.example.yaml) — Configuration cluster (NATS, mTLS) si présent

---

## 5. Runbooks

| Document | Description |
|----------|-------------|
| [runbooks/01_diagnostic.md](runbooks/01_diagnostic.md) | Runbook : diagnostic système (`akasha doctor`) |
| [runbooks/02_daemon_restart.md](runbooks/02_daemon_restart.md) | Runbook : redémarrage du daemon |

---

## 6. Plans et feuilles de route

| Document | Description |
|----------|-------------|
| [Blueprint_implémentation.md](Blueprint_implémentation.md) | Blueprint d’implémentation par phases (0 à 8) avec durées indicatives |
| [40_plan_rattrapage_et_polish.md](40_plan_rattrapage_et_polish.md) | Plan de rattrapage (phases 4, 6, 7, 8) + Phase 9 Polish UI |
| [CHANGELOG_CORRECTIONS.md](CHANGELOG_CORRECTIONS.md) | Changelog des corrections de documentation (référence) |
| [51_doc_vs_code_audit.md](51_doc_vs_code_audit.md) | Audit documentation vs code : crates, CLI, API, config, corrections effectuées |

---

## 7. Documents obsolètes ou doublons

Ces fichiers sont conservés pour référence mais pointent vers la version à jour :

- **17_internal_model.md** → voir [28_internal_model.md](28_internal_model.md)
- **23_runtime_resilience.md** → voir [19_runtime_resilience.md](19_runtime_resilience.md)

---

## Entrées rapides

| Besoin | Document |
|--------|----------|
| Comprendre le projet en une page | [00_vision.md](00_vision.md) |
| Utiliser Akasha (commandes, config, UI) | [user_guide.md](user_guide.md) |
| Formats et types des fichiers de config | [35_configuration_reference.md](35_configuration_reference.md) |
| Premier lancement | [onboarding.md](onboarding.md) |
| Architecture LLM (router, fallback) | [32_llm_router_architecture.md](32_llm_router_architecture.md) |
| Agents, outils, skills, orchestrateur | [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md) |
| Task Center, suivi des tâches en direct | [36_ui_architecture.md](36_ui_architecture.md) |
| Calendrier, récurrences, scheduler | [37_scheduler_design.md](37_scheduler_design.md) |
| Diagnostic / redémarrage | [runbooks/](runbooks/) |
