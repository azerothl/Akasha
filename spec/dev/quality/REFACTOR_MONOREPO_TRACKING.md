# Suivi refactor monorepo Akasha

Objectif: refactorisation progressive, PRs petites, sans régression fonctionnelle.

## Baseline (commandes de validation)

À exécuter depuis la racine du dépôt `Akasha` (Linux/macOS: préférer `CXX=g++` selon [AGENTS.md](../../AGENTS.md)).

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
| L3 | Découpage `api.rs` par domaines (tasks, config, memory, …) | À faire |
| L4 | `api_studio` / `api_workspace_graph` — alignement HTTP + services | À faire |
| L5 | Frontières crates (core / store / llm / tools) | À faire |
| L6 | Tests + doc dev finale | À faire |

## Fichiers monolithiques prioritaires

- `crates/akasha-daemon/src/api.rs`
- `crates/akasha-daemon/src/api_studio.rs`
- `crates/akasha-daemon/src/api_workspace_graph.rs`

## Métriques (à mettre à jour manuellement après chaque vague)

| Date | Lignes `api.rs` (approx.) | Notes |
|------|---------------------------|--------|
| 2026-04-27 | — | Extraction `api_http`, `api_path_utils` |
