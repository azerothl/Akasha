# Audit de couverture documentaire (user + dev)

Date: 2026-04-27

Objectif: verifier que la documentation utilisateur finale (`docs/`) et la documentation developpeur (`spec/`) couvrent l'ensemble des fonctionnalites et options disponibles.

## 1) Perimetre verifie

- CLI: commandes daemon, config, vault, routeur, plugins, update
- API: statut, docs, taches, mission autonome, MCP runtime/status
- Interfaces: TUI, web/desktop, onglets, raccourcis
- Configuration: `llm_router.yaml`, `tools_policy.yaml`, `voice_router.yaml`, `akasha.env`, `connectors.env`, `agent_profile.json`, `autonomous_mission.yaml`
- Runtime avance: terminal PTY, hooks, cache HTTP GET
- Integrations: MCP, webhooks, canaux externes
- Qualite/exploitation: tests, benchmarks, runbooks

## 2) Couverture doc utilisateur (`docs/`)

Statut global: **Complete pour usage final**.

Points couverts:
- installation et demarrage (zip, setup scripts, pre-requis)
- commandes principales et reference rapide
- variables d'environnement et configuration
- interfaces et commandes slash
- politique d'outils, skills, canaux, depannage
- nouveautes recentes et options avancees utiles aux utilisateurs

## 3) Couverture doc developpeur (`spec/`)

Statut global: **Complete pour dev/impl** avec regroupement par blocs fonctionnels.

Points couverts:
- architecture coeur, securite, runtime, memoire, interfaces
- integrations, plugins, operations, qualite, releases
- plans/audits/roadmaps et migration documentaire
- schemas YAML et exemples de configuration
- runbooks techniques et references de validation

## 4) Ecarts identifies et traitement

- Ecart: references de chemins historiques `docs/...` dans docs techniques migrees.
  - Traitement: mise a jour vers chemins `spec/dev/...`.
- Ecart: index `docs/README.md` melangeait user et dev.
  - Traitement: refonte en index utilisateur final uniquement.
- Ecart: absence d'un registre unique de suivi d'evolution des fonctionnalites.
  - Traitement: creation de `spec/feature_evolution_tracking.md`.

## 5) Decision de conformite

- Separation de responsabilites: **OK**
- Regroupement dev par blocs fonctionnels: **OK**
- Documentation utilisateur sans dependance fonctionnelle a `spec/`: **OK**
- Suivi d'evolution des fonctionnalites (fichier central + resume index): **OK**
