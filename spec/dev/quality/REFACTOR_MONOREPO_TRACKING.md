# Suivi refactor monorepo Akasha

Objectif: refactorisation progressive, PRs petites, sans régression fonctionnelle.

## Baseline (commandes de validation)

À exécuter depuis la racine du dépôt `Akasha` (Linux/macOS: préférer `CXX=g++` selon [AGENTS.md](../../../AGENTS.md)).

| Étape | Commande |
|-------|------------|
| Check daemon (lib) | `cargo check -p akasha-daemon --lib` |
| Check workspace | `cargo check` |
| Tests daemon (Windows, sans ONNX par défaut) | `cargo test -p akasha-daemon --no-default-features --features embedded --lib` |
| Tests ciblés `api_http` / `api_path_utils` | `cargo test -p akasha-daemon --no-default-features --features embedded api_http --lib` (idem `api_path_utils`) |
| Clippy | `cargo clippy -p akasha-daemon --lib -D warnings` (optionnel strict) |

## Checklist PR de refactor

- [ ] Comportement inchangé pour les endpoints / flux concernés (ou changelog explicite).
- [ ] `cargo check -p akasha-daemon --lib` OK.
- [ ] Tests ciblés ou nouveaux tests pour la logique extraite.
- [ ] Pas de duplication involontaire de helpers HTTP (préférer `api_http`).

## Lots et statut

| Lot | Description | Statut |
|-----|-------------|--------|
| L0 | Baseline + ce document | Fait |
| L1 | Module `api_http` (réponses JSON HTTP, parse requête) | Fait |
| L2 | Module `api_path_utils` (read_file, chemins verbatim, apostrophes) | Fait |
| L3 | Découpage `api.rs` par domaines (tasks, config, memory, …) | En cours avancé (parser+dispatch extraits: `api_tool_parser`, `api_tool_dispatch`)  |
| L4 | `api_studio` / `api_workspace_graph` — alignement HTTP + services | En cours (`api_studio/mod.rs` + `handlers.rs`, `api_workspace_graph/handlers.rs`) |
| L5 | Frontières crates (core / store / llm / tools) | En cours (doc `CRATE_BOUNDARIES` enrichie) |
| L6 | Tests + doc dev finale | À faire |

## Fichiers monolithiques prioritaires

- `crates/akasha-daemon/src/api.rs` (toujours volumineux ; `handle_api` délègue désormais vers `api_routes_*` / `api_security`)
- `crates/akasha-daemon/src/api_studio/` (`mod.rs` + `handlers.rs` — ancien `api_studio.rs`)
- `crates/akasha-daemon/src/api_workspace_graph.rs` + `api_workspace_graph/handlers.rs`

## Métriques (à mettre à jour manuellement après chaque vague)

| Date | Lignes `api.rs` (approx.) | Notes |
|------|---------------------------|--------|
| 2026-04-27 | — | Extraction `api_http`, `api_path_utils` |
| 2026-04-27 | ~17000 | Suite phase 1-3 : CSRF/query → `api_security.rs` ; automation / terminal / MCP / mission / profils → `api_routes_*.rs` ; cache agent → `agent_profile.rs` ; studio → `api_studio/` ; workspace graph → `api_workspace_graph/handlers.rs` (`wc -l` / PowerShell `Measure-Object -Line` sur les fichiers sources) |
