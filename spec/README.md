# Index des specifications Akasha (developpeurs)

Ce dossier contient uniquement la documentation technique:
- architecture et design
- implementation et exploitation
- suivi d'evolution des fonctionnalites

La documentation utilisateur finale est maintenue dans `docs/`.

## Suivi d'evolution des fonctionnalites

- Registre central: [feature_evolution_tracking.md](feature_evolution_tracking.md)
- Resume d'avancement historique: [plan_avancement.md](plan_avancement.md)
- Corrections doc/code: [51_doc_vs_code_audit.md](51_doc_vs_code_audit.md)
- Refactor code (suivi): [dev/quality/REFACTOR_MONOREPO_TRACKING.md](dev/quality/REFACTOR_MONOREPO_TRACKING.md)
- Frontières crates: [dev/core/CRATE_BOUNDARIES.md](dev/core/CRATE_BOUNDARIES.md)

## Blocs fonctionnels dev

### 1) Produit, scope, exigences

- [00_vision.md](00_vision.md)
- [01_scope.md](01_scope.md)
- [01_functional_requirements.md](01_functional_requirements.md)
- [02_non_functional_requirements.md](02_non_functional_requirements.md)
- [03_fonctional_requirements.md](03_fonctional_requirements.md)
- [04_non_fonctional_requirements.md](04_non_fonctional_requirements.md)
- [02_personas.md](02_personas.md)

### 2) Architecture coeur

- [05_agent_architecture.md](05_agent_architecture.md)
- [18_deployment_architecture.md](18_deployment_architecture.md)
- [29_stack_technique.md](29_stack_technique.md)
- [32_llm_router_architecture.md](32_llm_router_architecture.md)
- [24_model_fallback.md](24_model_fallback.md)
- [28_internal_model.md](28_internal_model.md)
- [30_architecture_d_entrainement.md](30_architecture_d_entrainement.md)
- [34_embedded_small_model.md](34_embedded_small_model.md)
- [36_bitnet_integration_study.md](36_bitnet_integration_study.md)

### 3) Securite, fiabilite, observabilite

- [07_security_model.md](07_security_model.md)
- [12_threat_model.md](12_threat_model.md)
- [19_runtime_resilience.md](19_runtime_resilience.md)
- [25_degraded_mode.md](25_degraded_mode.md)
- [26_immutable_log.md](26_immutable_log.md)
- [49_policy_engine.md](49_policy_engine.md)
- [observability.md](observability.md)
- [runbooks/01_diagnostic.md](runbooks/01_diagnostic.md)
- [runbooks/02_daemon_restart.md](runbooks/02_daemon_restart.md)

### 4) Donnees, memoire, graphes

- [06_memory_model.md](06_memory_model.md)
- [47_memory_4_layers.md](47_memory_4_layers.md)
- [46_memory_facts_knowledge_graph.md](46_memory_facts_knowledge_graph.md)
- [54_workspace_project_knowledge_graph.md](54_workspace_project_knowledge_graph.md)
- [10_data_model.yaml](10_data_model.yaml)
- [09_event_model.yaml](09_event_model.yaml)

### 5) Interfaces et experience

- [36_ui_architecture.md](36_ui_architecture.md)
- [38_interfaces.md](38_interfaces.md)
- [37_scheduler_design.md](37_scheduler_design.md)
- [43_session_terminal.md](43_session_terminal.md)
- [42_image_generation.md](42_image_generation.md)

### 6) Integrations, plugins, canaux

- [16_plugin_architecture.md](16_plugin_architecture.md)
- [27_plugin_reputation_system.md](27_plugin_reputation_system.md)
- [39_browser_automation.md](39_browser_automation.md)
- [api_integration.md](api_integration.md)
- [22_multichannel_architecture.md](22_multichannel_architecture.md)
- [44_whatsapp.md](44_whatsapp.md)
- [52_personality_architecture.md](52_personality_architecture.md)

### 7) Operations, runbooks, qualite

- [runbooks/03_bitnet_server_setup.md](runbooks/03_bitnet_server_setup.md)
- [runbooks/04_evals.md](runbooks/04_evals.md)
- [35_configuration_reference.md](35_configuration_reference.md)
- [distribution.md](distribution.md)
- [onboarding.md](onboarding.md)
- [projects_long_running.md](projects_long_running.md)

### 8) Roadmap, audits, plans

- [Blueprint_implémentation.md](Blueprint_implémentation.md)
- [40_plan_rattrapage_et_polish.md](40_plan_rattrapage_et_polish.md)
- [41_phase9_polish_checklist.md](41_phase9_polish_checklist.md)
- [45_task_center_calendar_audit.md](45_task_center_calendar_audit.md)
- [CHANGELOG_CORRECTIONS.md](CHANGELOG_CORRECTIONS.md)
- [31_Diagramme_architecture.mmd](31_Diagramme_architecture.mmd)

### 9) Documentation dev migree depuis `docs/`

#### Integrations

- [dev/integrations/automation-webhooks.md](dev/integrations/automation-webhooks.md)
- [dev/integrations/mcp-mvp.md](dev/integrations/mcp-mvp.md)
- [dev/integrations/mcp-oauth.md](dev/integrations/mcp-oauth.md)
- [dev/integrations/mcp-runtime.md](dev/integrations/mcp-runtime.md)
- [dev/integrations/claude/claude-src-akasha.md](dev/integrations/claude/claude-src-akasha.md)

#### Runtime

- [dev/runtime/cache-strategy.md](dev/runtime/cache-strategy.md)
- [dev/runtime/gateway-shell-hooks.md](dev/runtime/gateway-shell-hooks.md)
- [dev/runtime/terminal-backends-roadmap.md](dev/runtime/terminal-backends-roadmap.md)

#### Plugins

- [dev/plugins/plugin-host-network.md](dev/plugins/plugin-host-network.md)
- [dev/plugins/plugin-map-view-schema.md](dev/plugins/plugin-map-view-schema.md)

#### Qualite

- [dev/quality/bench_prompt_results.md](dev/quality/bench_prompt_results.md)
- [dev/quality/tests_and_benchmarks.md](dev/quality/tests_and_benchmarks.md)
- [dev/quality/documentation_coverage_audit.md](dev/quality/documentation_coverage_audit.md)
- [dev/quality/REFACTOR_MONOREPO_TRACKING.md](dev/quality/REFACTOR_MONOREPO_TRACKING.md)

#### Ops

- [dev/ops/slo-akasha-internal.md](dev/ops/slo-akasha-internal.md)

#### Releases et roadmap

- [dev/releases/internal_release_0.8.md](dev/releases/internal_release_0.8.md)
- [dev/roadmap/hermes-akasha-parity-matrix.md](dev/roadmap/hermes-akasha-parity-matrix.md)
- [dev/roadmap/hermes-integration-remainder.md](dev/roadmap/hermes-integration-remainder.md)
- [dev/roadmap/hermes-partial-domains-roadmap.md](dev/roadmap/hermes-partial-domains-roadmap.md)
- [dev/roadmap/docs_spec_migration_map.md](dev/roadmap/docs_spec_migration_map.md)

## Conventions de rangement

- `spec/dev/integrations`: API, MCP, bridges externes
- `spec/dev/runtime`: execution, sessions, cache, backend terminal
- `spec/dev/plugins`: host plugin et schemas lies
- `spec/dev/quality`: tests, benchs, validations
- `spec/dev/ops`: runbooks internes d'exploitation
- `spec/dev/releases`: notes techniques de version
- `spec/dev/roadmap`: plans et matrices de convergence
