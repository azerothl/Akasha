# Observabilité et diagnostics

Cette page résume ce qu'un **utilisateur final** peut consulter sans lire la doc développeur (`spec/`).

## Diagnostic rapide

| Action | Usage |
|--------|--------|
| `akasha doctor` | Vérifie binaires, data_dir, vault, modèle embarqué, connecteurs |
| `akasha doctor --json` | Même chose en JSON (scripts, intégrations) |
| `akasha doctor --advice` | Conseils contextualisés (daemon **doit** tourner) |
| `akasha doctor --fix` | Crée data_dir et fichiers YAML minimaux manquants |
| `/doctor` dans le chat | Équivalent interactif côté daemon |

Détails et dépannage : page **Dépannage**.

## Métriques routeur LLM

- **TUI / web** : onglet **Routeur** (latence, requêtes, fallbacks).
- **CLI** : `akasha router metrics`
- **Chat** : commande `/metrics`
- **Configuration** : `global.enable_metrics` dans `llm_router.yaml` (voir **Configuration**)

## Usage tokens et coût (interface web)

Dans l'application desktop, le panneau **Usage** (tableau de bord 7 / 30 jours) agrège tokens et coût estimé par provider — utile pour surveiller la consommation cloud.

## Runbooks (opérateurs avancés)

Les procédures d'exploitation détaillées (redémarrage daemon, diagnostic approfondi) sont livrées dans l'archive de release sous `spec/runbooks/` (ex. diagnostic, redémarrage). Elles complètent `akasha doctor` pour les installations serveur ou headless.

## Métriques système (développeurs)

Pour Prometheus, traces OpenTelemetry et métriques daemon internes, voir la documentation technique `spec/observability.md` dans le dépôt source ou le zip release.
