# Documentation Akasha

Ce dossier est le point d'entrée vers la documentation du projet.

## Documentation publique (utilisateurs des binaires)

| Document | Description |
|----------|-------------|
| [user_guide_final.md](user_guide_final.md) | **Guide utilisateur final** : commandes, config, interfaces, slash, canaux, dépannage. Sans référence au code source. Livré dans le zip des releases sous `docs/user_guide.md` et affiché dans l'onglet Doc des interfaces. |

## Documentation technique (contributeurs, dépôt source)

| Document | Description |
|----------|-------------|
| [internal_release_0.8.md](internal_release_0.8.md) | **Notes internes 0.8.0** : delta depuis v0.7.0 (mission autonome, graphe projet, API, CI, doctor, specs) — hors zip utilisateur. |
| [user_guide.md](user_guide.md) | Renvoi vers le **guide utilisateur complet** ([spec/user_guide.md](../spec/user_guide.md)) : même contenu étendu (build, Rust, références aux specs). |
| [bench_prompt_results.md](bench_prompt_results.md) | Benchmarks des prompts et heuristiques agents, optimisations appliquées et propositions d'amélioration. |
| [tests_and_benchmarks.md](tests_and_benchmarks.md) | Vue d'ensemble des tests et benchmarks : ce qu'ils analysent et comment les lancer. |

**Références :**

- **Spécifications et architecture** : [spec/README.md](../spec/README.md) — index de tous les documents dans `spec/`
- **Premier lancement** : [spec/onboarding.md](../spec/onboarding.md)
- **Vue d'ensemble** : [README.md](../README.md) à la racine du projet

Quand le daemon tourne, le guide affiché via **GET /api/docs** et l’onglet **Doc** est : en développement depuis le dépôt, d’abord `spec/user_guide.md` ; sinon `docs/user_guide.md` (copie de `user_guide_final.md` dans les releases). Voir [spec/51_doc_vs_code_audit.md](../spec/51_doc_vs_code_audit.md) § 9.

Le site public **Akasha_app** (`docs.html`, anglais) est tenu manuellement ; checklist : `Akasha_app/docs/DOCUMENTATION_SYNC.md`.
