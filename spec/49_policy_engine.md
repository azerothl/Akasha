# Policy Engine — moteur de politique central (spec 49)

**Statut** : Spec et squelette (Phase 3 AI OS). Intégration complète outils/mémoire/plugins à étendre.

## 1. Objectif

Un moteur de politique unique qui décide « qui peut faire quoi » : agent, mémoire, plugin, outil. Les règles sont évaluées aux points d’application (avant outil, avant accès mémoire, avant chargement/invocation plugin). Option de masquage (redaction) des données sensibles dans logs et réponses.

**Note (plugins)** : l’invocation d’outils machine (`read_file`, `write_file`, `plugin.call`, etc.) côté worker conversation est **autorisée ou refusée par `tools_policy.yaml`** (et profils d’outils). Les métadonnées `routing_rules` des manifests de plugins **ne constituent pas** un second garde-fou d’exécution : elles peuvent enrichir le prompt à titre indicatif uniquement.

## 2. Modèle de règles

- **Acteur** : `agent_id` / `role` / `channel` (ex. `conversation`, `slack`, `api`).
- **Ressource** : `memory_scope`, `plugin_id`, `tool_name` (ex. `read_file`, `run_command`).
- **Action** : `read`, `write`, `execute`, `approve`.
- **Conditions** (optionnel) : contexte (session, workspace, heure).

Une règle : (acteur, ressource, action, conditions) → allow | deny.

## 3. Points d’application

| Point | Comportement actuel | Avec Policy Engine |
|-------|---------------------|--------------------|
| Outils | `ToolsPolicy` (allowed paths, require_approval) | Policy Engine peut compléter ou remplacer (règles par acteur/outil). |
| Mémoire | Scope déjà sur memory_entries | Filtres par acteur si policy le demande. |
| Plugins | Chargement selon SkillRegistry / trust | Vérification que l’acteur (channel/session) a le droit d’utiliser ce plugin. |
| Redaction | — | Règles « si ressource = vault ou champ X, redact dans logs / réponses ». |

## 4. Implémentation (squelette)

- Module `akasha-daemon/src/policy_engine.rs` : chargement de règles (YAML/JSON), fonction `evaluate(actor, resource, action) -> bool`.
- Les règles peuvent être lues depuis un fichier `policy_engine.yaml` (ou intégration avec `tools_policy` existant).
- Intégration progressive : d’abord appel depuis la couche outils (si policy engine configuré, l’utiliser en plus de ToolsPolicy), puis mémoire et plugins.

## 5. Fichiers

| Composant | Fichier |
|-----------|---------|
| Spec | `spec/49_policy_engine.md` |
| Module | `akasha-daemon/src/policy_engine.rs` |
| Tools | `akasha-daemon/src/api.rs` (appel avant exécution outil, si activé) |

## 6. Références

- [akasha-tools/src/policy.rs](crates/akasha-tools/src/policy.rs) — ToolsPolicy actuel.
- [memory_orchestrator.rs](crates/akasha-daemon/src/memory_orchestrator.rs) — scope mémoire.
