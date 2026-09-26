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
| Doc embarquée | Guide multi-pages + API index | Implemented | OK | OK | `docs/user/`, `user_docs.rs`, `51_doc_vs_code_audit.md`, `scripts/build-user-docs.py` |
| Runtime | Sessions terminal / PTY et backends | Implemented | OK | OK | `docs/user/commandes.md` (§7bis), `43_session_terminal.md`, `dev/runtime/terminal-backends-roadmap.md` |
| Integrations | MCP runtime / OAuth / MVP | In progress | N/A | OK | `dev/integrations/mcp-runtime.md`, `dev/integrations/mcp-oauth.md`, `dev/integrations/mcp-mvp.md` |
| Plugins | Architecture plugins + reputation + reseau host | Implemented | OK | OK | `docs/user/extensions.md`, `16_plugin_architecture.md`, `27_plugin_reputation_system.md`, `dev/plugins/plugin-host-network.md`, `dev/plugins/plugin-map-view-schema.md` |
| Memory | Memoire LT + compaction + knowledge graph + user RAG async | Implemented | Partially documented | OK | `06_memory_model.md`, `47_memory_4_layers.md`, `dev/roadmap/kinbot_inspired_roadmap.md` |
| Graph | Workspace project graph | Implemented | OK | OK | `docs/user/donnees.md`, `54_workspace_project_knowledge_graph.md` |
| Security | Menaces, permissions, policy engine | Implemented | OK | OK | `docs/user/configuration.md` (§8 tools_policy), `12_threat_model.md`, `07_security_model.md`, `49_policy_engine.md` |
| Observability | Metrics, diagnostics, runbooks | Implemented | Partially documented | OK | `observability.md`, `runbooks/01_diagnostic.md`, `runbooks/02_daemon_restart.md` |
| Channels | Telegram / Slack / Discord / WhatsApp | Implemented (partiel selon canal) | OK | OK | `docs/user/extensions.md`, `22_multichannel_architecture.md`, `44_whatsapp.md` |
| Mission autonome | Heartbeats, roles, events API | Implemented | OK | OK | `docs/user/mission.md`, `dev/runtime/autonomous-mission.md`, `agent_contracts/` |
| Cockpit UI | Active work drawer (chat ↔ tasks) | Implemented | OK | OK | `roadmap_v0.10.0.md` P6 A1, `ActiveWorkDrawer.tsx` |
| Cockpit UI | Composer modes architect/code/ask | Implemented | OK | OK | `roadmap_v0.10.0.md` P6 A2, `agent_profiles.rs` |
| Observability | Usage dashboard 7/30j (tokens/coût) | Implemented | OK | OK | `roadmap_v0.10.0.md` P6 A3, `UsageDashboardPanel.tsx` |
| Interfaces | Session pin / rename / fork / dual-pane | Implemented | OK | OK | `roadmap_v0.10.0.md` P6 A4 |
| Life layer | Overnight skill pack (schedule template) | Implemented | OK | OK | `roadmap_v0.10.0.md` P7 L1, `LifeLayerPanel.tsx` |
| Life layer | Morning brief → Telegram notify | Implemented | OK | OK | `roadmap_v0.10.0.md` P7 L2, `/api/channels/notify` |
| Life layer | OAuth CalDAV dans Connectors | Implemented | OK | OK | `roadmap_v0.10.0.md` P7 L3, `ConnectorsPanel` |
| Life layer | Scheduling langage naturel (Hermes) | Implemented | OK | OK | `roadmap_v0.10.0.md` P7 L4, `/api/schedules/from-nl` |
| Interfaces | Companion ESP32 (Freenove FNK0104) | Planned | N/A | Planned | Repo `Akasha_companion` — voix-first + avatar ; `docs/COMPANION_SPEC.md`, `/api/voice/*` |

## Liste prioritaire "À documenter"

Items P7 must v0.11 traités dans la sync doc (réseau host / map view, PTY, tools_policy avancé, mission autonome dev) — restants :

1. Mémoire LT / compaction / knowledge graph — synthèse user plus détaillée (encore Partially documented).
2. Observability (metrics / runbooks) — synthèse user si besoin hors onglet Routeur / doctor.
3. Sync site **Akasha_app** (Whatʼs new) — stretch.

## Process de mise à jour

1. Toute nouvelle feature ou option modifiée met à jour ce fichier.
2. La PR référence les sections user (`docs/`) et dev (`spec/`) impactées.
3. Un statut `Partially documented` ne peut rester au-delà d'une release mineure sans ticket associé.
