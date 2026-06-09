# Suivi d'évolution des fonctionnalités

Ce registre est la source de vérité sur l'état de couverture des fonctionnalités et options Akasha.

## Légende des statuts

- `Implemented`: implémenté et documenté côté dev + user
- `Partially documented`: implémenté mais documentation incomplète
- `Planned`: prévu, non implémenté
- `In progress`: implémentation en cours

## Registre (vue fonctionnelle)

| Bloc | Fonctionnalite / option | Statut implementation | Statut doc user (`docs/`) | Statut doc dev (`spec/`) | References |
|---|---|---|---|---|---|
| Core | Commandes CLI (`start/stop/init/doctor/config/vault`) | Implemented | OK | OK | `docs/user/commandes.md`, `35_configuration_reference.md` |
| Core | Routeur LLM + fallback | Implemented | OK | OK | `docs/user/configuration.md`, `32_llm_router_architecture.md`, `24_model_fallback.md` |
| Interfaces | TUI + UI desktop/web + onglets | Implemented | OK | OK | `docs/user/interfaces.md`, `38_interfaces.md`, `36_ui_architecture.md` |
| Workspace UI | Cookbook (modèles + recettes), Comparer, Recherche, Notes | Implemented | OK | OK | `docs/user/workspace.md`, `38_interfaces.md`, `spec/cookbook/` |
| Doc embarquée | Guide multi-pages + API index | Implemented | OK | Partially documented | `docs/user/`, `user_docs.rs`, `51_doc_vs_code_audit.md` |
| Runtime | Sessions terminal / PTY et backends | Implemented | Partially documented | OK | `43_session_terminal.md`, `dev/runtime/terminal-backends-roadmap.md` |
| Integrations | MCP runtime / OAuth / MVP | In progress | N/A | OK | `dev/integrations/mcp-runtime.md`, `dev/integrations/mcp-oauth.md`, `dev/integrations/mcp-mvp.md` |
| Plugins | Architecture plugins + reputation + reseau host | Implemented | Partially documented | OK | `16_plugin_architecture.md`, `27_plugin_reputation_system.md`, `dev/plugins/plugin-host-network.md` |
| Memory | Memoire LT + compaction + knowledge graph + user RAG async | Implemented | Partially documented | OK | `06_memory_model.md`, `47_memory_4_layers.md`, `dev/roadmap/kinbot_inspired_roadmap.md` |
| Graph | Workspace project graph | Implemented | OK | OK | `docs/user/donnees.md`, `54_workspace_project_knowledge_graph.md` |
| Security | Menaces, permissions, policy engine | Implemented | Partially documented | OK | `12_threat_model.md`, `07_security_model.md`, `49_policy_engine.md` |
| Observability | Metrics, diagnostics, runbooks | Implemented | Partially documented | OK | `observability.md`, `runbooks/01_diagnostic.md`, `runbooks/02_daemon_restart.md` |
| Channels | Telegram / Slack / Discord / WhatsApp | Implemented (partiel selon canal) | OK | OK | `docs/user/extensions.md`, `22_multichannel_architecture.md`, `44_whatsapp.md` |
| Mission autonome | Heartbeats, roles, events API | Implemented | OK | Partially documented | `docs/user/mission.md`, `plan_avancement.md` |

## Liste prioritaire "À documenter"

1. Couverture utilisateur des détails plugin avancé (réseau host, schémas map view).
2. Clarification user des limites runtime terminal/PTY et prérequis.
3. Synthèse utilisateur des options sécurité/politique d'outils avancées.
4. Compléter la section dev de la mission autonome (cycle, contracts, observabilité).

## Process de mise à jour

1. Toute nouvelle feature ou option modifiée met à jour ce fichier.
2. La PR référence les sections user (`docs/`) et dev (`spec/`) impactées.
3. Un statut `Partially documented` ne peut rester au-delà d'une release mineure sans ticket associé.
