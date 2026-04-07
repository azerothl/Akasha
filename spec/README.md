# Index des spécifications Akasha

Ce dossier contient la documentation technique et fonctionnelle du projet. Les documents sont regroupés par thème pour faciliter la navigation.

**Documentation publique (binaires uniquement)** : le guide utilisateur sans référence au code est [docs/user_guide_final.md](../docs/user_guide_final.md) ; il est livré dans le zip des releases sous `docs/user_guide.md` et affiché dans l’onglet Doc des interfaces. En développement, `GET /api/docs` sert d’abord [spec/user_guide.md](user_guide.md) ; voir [51_doc_vs_code_audit.md](51_doc_vs_code_audit.md) § 9.

**Autres entrées hors `spec/`** : [docs/README.md](../docs/README.md) (bench, tests, intégrations plugin), [README.md](../README.md) (racine du dépôt).

---

## Sommaire

1. [Vision et périmètre](#1-vision-et-périmètre)
2. [Exigences et personas](#2-exigences-et-personas)
3. [Architecture](#3-architecture)
4. [Mémoire avancée, gateway, politique, observabilité](#4-mémoire-avancée-gateway-politique-observabilité)
5. [Automatisation, intégrations, personnalité](#5-automatisation-intégrations-personnalité)
6. [Guide utilisateur et onboarding](#6-guide-utilisateur-et-onboarding)
7. [Runbooks](#7-runbooks)
8. [Plans, audits et feuilles de route](#8-plans-audits-et-feuilles-de-route)
9. [Schémas YAML, exemples et compétences](#9-schémas-yaml-exemples-et-compétences)
10. [Documents obsolètes ou doublons](#10-documents-obsolètes-ou-doublons)
11. [Entrées rapides](#11-entrées-rapides)

---

## 1. Vision et périmètre

| Document | Description |
|----------|-------------|
| [00_vision.md](00_vision.md) | Vision du projet, principes directeurs, extension stratégique (standalone, cluster, fallback) |
| [01_scope.md](01_scope.md) | Périmètre v1 : inclus / exclu, hypothèses, contraintes |
| [01_functional_requirements.md](01_functional_requirements.md) | Exigences fonctionnelles — **résumé exécutif** (liste FR) |
| [02_non_functional_requirements.md](02_non_functional_requirements.md) | Exigences non fonctionnelles — **résumé exécutif** |
| [03_fonctional_requirements.md](03_fonctional_requirements.md) | Exigences fonctionnelles — **spécification détaillée** (critères ; nom de fichier historique « fonctional ») |
| [04_non_fonctional_requirements.md](04_non_fonctional_requirements.md) | Exigences non fonctionnelles — **spécification détaillée** |
| [02_personas.md](02_personas.md) | Personas utilisateurs (Alex, Sarah, Marc, Julie) |

---

## 2. Exigences et personas

Voir [§1](#1-vision-et-périmètre) (scope, FR, NFR, personas). Les paires 01/03 et 02/04 sont documentées comme résumé vs détail dans les en-têtes des fichiers.

---

## 3. Architecture

### Général

| Document | Description |
|----------|-------------|
| [05_agent_architecture.md](05_agent_architecture.md) | Architecture agentique : Main Agent, Orchestrator, agents spécialisés |
| [29_stack_technique.md](29_stack_technique.md) | Stack technique : langages, bus d’événements, BDD, crypto, sandboxing, cluster |
| [18_deployment_architecture.md](18_deployment_architecture.md) | Architecture de déploiement |

### Sécurité

| Document | Description |
|----------|-------------|
| [07_security_model.md](07_security_model.md) | Modèle de sécurité : secrets, isolation, prompt injection, logs |
| [12_threat_model.md](12_threat_model.md) | Modèle de menaces |

### Mémoire (vue d’ensemble)

| Document | Description |
|----------|-------------|
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
| [34_embedded_small_model.md](34_embedded_small_model.md) | Modèle petit et intégré : onboarding, diagnostics, réponses simples |
| [36_bitnet_integration_study.md](36_bitnet_integration_study.md) | Étude d’intégration BitNet (complément [34](34_embedded_small_model.md)) |
| [42_image_generation.md](42_image_generation.md) | Génération d’images : outil generate_image, affichage dans le chat |

### Canaux et cluster

| Document | Description |
|----------|-------------|
| [22_multichannel_architecture.md](22_multichannel_architecture.md) | Architecture multi-canal (Slack, Discord, Telegram, Teams, etc.) |
| [20_cluster_architecture.md](20_cluster_architecture.md) | Mode cluster (NATS, élection de leader, tolérance de panne) |

### Plugins

| Document | Description |
|----------|-------------|
| [16_plugin_architecture.md](16_plugin_architecture.md) | Architecture des plugins (types, règles) |
| [27_plugin_reputation_system.md](27_plugin_reputation_system.md) | Système de réputation des plugins |

### Agents, outils, skills, orchestrateur

| Document | Description |
|----------|-------------|
| [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md) | Feuille de route : outils, politique, skills, orchestrateur, état d’implémentation |
| [53_web_crawl_cloudflare.md](53_web_crawl_cloudflare.md) | Crawl web optionnel (Cloudflare) ; `tools_policy`, outils web_crawl |

### UI et scheduler

| Document | Description |
|----------|-------------|
| [36_ui_architecture.md](36_ui_architecture.md) | Architecture UI : onglets, Task Center, calendrier, transport temps réel |
| [37_scheduler_design.md](37_scheduler_design.md) | Scheduler standalone et cluster |
| [38_interfaces.md](38_interfaces.md) | TUI vs Tauri, lancement, raccourcis, matériel client, accessibilité |

### Contrats d’agents (référence)

| Document | Description |
|----------|-------------|
| [agent_contracts/README.md](agent_contracts/README.md) | Dossier : contrats / conventions pour agents et intégrations |

---

## 4. Mémoire avancée, gateway, politique, observabilité

| Document | Description |
|----------|-------------|
| [47_memory_4_layers.md](47_memory_4_layers.md) | Mémoire à quatre couches (complément au [06_memory_model.md](06_memory_model.md)) |
| [46_memory_facts_knowledge_graph.md](46_memory_facts_knowledge_graph.md) | Faits, graphe de connaissances |
| [54_workspace_project_knowledge_graph.md](54_workspace_project_knowledge_graph.md) | Graphes projet multi-workspaces (indexation, API, UI, agents) |
| [48_gateway_layer.md](48_gateway_layer.md) | Couche gateway (enveloppe messages, cycle de vie tâches) |
| [49_policy_engine.md](49_policy_engine.md) | Moteur de politique (transitions tâches, défauts sûrs) |
| [observability.md](observability.md) | Observabilité : métriques, timeline, diagnostic |

---

## 5. Automatisation, intégrations, personnalité

| Document | Description |
|----------|-------------|
| [39_browser_automation.md](39_browser_automation.md) | Automatisation navigateur (spec produit / intégration) |
| [api_integration.md](api_integration.md) | Intégration HTTP / API pour outils et observateurs |
| [43_session_terminal.md](43_session_terminal.md) | Session et terminal (usage avancé) |
| [44_whatsapp.md](44_whatsapp.md) | Canal WhatsApp (spec / état) |
| [52_personality_architecture.md](52_personality_architecture.md) | Architecture personnalité (modes, mémoire personnalité) |
| [personality_core.yaml](personality_core.yaml) / [personality_modes.yaml](personality_modes.yaml) | Données de personnalité par défaut |

### Projets longue durée

| Document | Description |
|----------|-------------|
| [projects_long_running.md](projects_long_running.md) | Tâches et projets longue durée |

---

## 6. Guide utilisateur et onboarding

| Document | Description |
|----------|-------------|
| [user_guide.md](user_guide.md) | Guide utilisateur **complet** (dépôt : build, specs). Servi par `GET /api/docs` en dev |
| [38_interfaces.md](38_interfaces.md) | Interfaces détaillées (voir aussi [§3](#3-architecture)) |
| [onboarding.md](onboarding.md) | Premier lancement pas à pas |
| [distribution.md](distribution.md) | Distribution binaires, zip, release |
| [../docs/user_guide_final.md](../docs/user_guide_final.md) | Guide **final** pour utilisateurs sans code source |

**Référence config** : [35_configuration_reference.md](35_configuration_reference.md).

---

## 7. Runbooks

| Document | Description |
|----------|-------------|
| [runbooks/01_diagnostic.md](runbooks/01_diagnostic.md) | Diagnostic système (`akasha doctor`) |
| [runbooks/02_daemon_restart.md](runbooks/02_daemon_restart.md) | Redémarrage du daemon |
| [runbooks/03_bitnet_server_setup.md](runbooks/03_bitnet_server_setup.md) | Mise en place serveur BitNet |
| [runbooks/04_evals.md](runbooks/04_evals.md) | Suite d’évals (akasha-evals) |

---

## 8. Plans, audits et feuilles de route

| Document | Description |
|----------|-------------|
| [Blueprint_implémentation.md](Blueprint_implémentation.md) | Blueprint par phases (0–8) |
| [40_plan_rattrapage_et_polish.md](40_plan_rattrapage_et_polish.md) | Plan de rattrapage + polish UI |
| [41_phase9_polish_checklist.md](41_phase9_polish_checklist.md) | Checklist Phase 9 (polish) |
| [plan_avancement.md](plan_avancement.md) | Suivi d’avancement interne |
| [45_task_center_calendar_audit.md](45_task_center_calendar_audit.md) | Audit Task Center / calendrier |
| [CHANGELOG_CORRECTIONS.md](CHANGELOG_CORRECTIONS.md) | Historique des corrections de documentation |
| [51_doc_vs_code_audit.md](51_doc_vs_code_audit.md) | Audit documentation vs code (CLI, API, env) |
| [31_Diagramme_architecture.mmd](31_Diagramme_architecture.mmd) | Diagramme d’architecture (source Mermaid) |

---

## 9. Schémas YAML, exemples et compétences

| Fichier | Description |
|---------|-------------|
| [schemas/execution_plan.schema.json](schemas/execution_plan.schema.json) | Schéma JSON execution plan |
| [09_event_model.yaml](09_event_model.yaml) | Modèle d’événements |
| [10_data_model.yaml](10_data_model.yaml) | Modèle de données |
| [06_memory_model.yaml](06_memory_model.yaml) | Mémoire (YAML) |
| [07_security_model.yaml](07_security_model.yaml) | Sécurité (YAML) |
| [08_permissions_model.yaml](08_permissions_model.yaml) | Permissions |
| [11_state_machine.yaml](11_state_machine.yaml) | Machine d’états |
| [12_cluster_architecture.yaml](12_cluster_architecture.yaml) | Cluster |
| [13_model_strategy.yaml](13_model_strategy.yaml) | Stratégie modèles |
| [14_degraded_mode.yaml](14_degraded_mode.yaml) | Mode dégradé |
| [15_immutable_log.yaml](15_immutable_log.yaml) | Journal immuable |
| [16_plugin_reputation.yaml](16_plugin_reputation.yaml) | Réputation plugins |
| [17_internal_model.yaml](17_internal_model.yaml) | Modèle interne |
| [20_cluster_architecture.yaml](20_cluster_architecture.yaml) | Cluster (variante) |
| [21_health_monitoring.yaml](21_health_monitoring.yaml) | Santé / monitoring |
| [27_plugin_reputation.yaml](27_plugin_reputation.yaml) | Réputation (variante) |
| [initiative_policy.yaml](initiative_policy.yaml) | Politique d’initiative |
| [orchestration_rules.example.yaml](orchestration_rules.example.yaml) | Exemple règles d’orchestration |
| [llm_router.example.yaml](llm_router.example.yaml) | Exemple routeur LLM |
| [tools_policy.example.yaml](tools_policy.example.yaml) | Exemple politique d’outils |
| [voice_router.example.yaml](voice_router.example.yaml) | Exemple voix TTS/STT |
| [cluster.example.yaml](cluster.example.yaml) | Exemple cluster |
| [skills/read_file_skill.yaml](skills/read_file_skill.yaml) | Exemple de skill |

---

## 10. Documents obsolètes ou doublons

Conservés pour référence ; la version à jour est indiquée ci-dessous.

| Ancien fichier | Voir |
|----------------|------|
| [17_internal_model.md](17_internal_model.md) | [28_internal_model.md](28_internal_model.md) |
| [23_runtime_resilience.md](23_runtime_resilience.md) | [19_runtime_resilience.md](19_runtime_resilience.md) |

---

## 11. Entrées rapides

| Besoin | Document |
|--------|----------|
| Comprendre le projet en une page | [00_vision.md](00_vision.md) |
| Utiliser Akasha (commandes, config, UI) | [user_guide.md](user_guide.md) ou [../docs/user_guide_final.md](../docs/user_guide_final.md) |
| Interfaces (TUI, Tauri) et accessibilité | [38_interfaces.md](38_interfaces.md) |
| Formats de configuration | [35_configuration_reference.md](35_configuration_reference.md) |
| Premier lancement | [onboarding.md](onboarding.md) |
| Architecture LLM (router, fallback) | [32_llm_router_architecture.md](32_llm_router_architecture.md) |
| Gateway et politique moteur | [48_gateway_layer.md](48_gateway_layer.md), [49_policy_engine.md](49_policy_engine.md) |
| Mémoire (couches, graphe) | [47_memory_4_layers.md](47_memory_4_layers.md), [46_memory_facts_knowledge_graph.md](46_memory_facts_knowledge_graph.md) |
| Agents, outils, skills | [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md) |
| Task Center, UI | [36_ui_architecture.md](36_ui_architecture.md) |
| Calendrier, scheduler | [37_scheduler_design.md](37_scheduler_design.md) |
| Génération d’images | [42_image_generation.md](42_image_generation.md) |
| Voix (TTS/STT) | [35_configuration_reference.md](35_configuration_reference.md), [akasha-models/README.md](../akasha-models/README.md) |
| Observabilité | [observability.md](observability.md) |
| Personnalité | [52_personality_architecture.md](52_personality_architecture.md) |
| Audit doc vs code | [51_doc_vs_code_audit.md](51_doc_vs_code_audit.md) |
| Diagnostic / redémarrage | [runbooks/](runbooks/) |
