# Documentation utilisateur Akasha

Ce dossier contient uniquement la documentation destinee aux utilisateurs finaux des binaires Akasha.

## Documents disponibles

| Document | Description |
|---|---|
| [user_guide_final.md](user_guide_final.md) | Guide utilisateur principal (installation, configuration, interfaces, commandes, depannage). |
| [user_guide.md](user_guide.md) | Copie diffusee dans les releases et utilisee par l'onglet **Doc** des interfaces. |
| [screenshots/README.md](screenshots/README.md) | Informations sur les captures d'ecran utilisateur. |
| [constitution.yaml.example](constitution.yaml.example) | Gabarit gouvernance mémoire (E1) — copier vers `~/akasha/constitution.yaml` pour activer le filtre recall. |

## Portee

- Cette documentation couvre l'utilisation produit cote utilisateur final.
- Les informations de developpement interne ne sont pas inclues dans ce dossier.

## Registre des fonctionnalites (contributeurs)

L'etat d'implementation et de documentation par bloc fonctionnel est suivi dans [`spec/feature_evolution_tracking.md`](../spec/feature_evolution_tracking.md) (source de verite). Avant chaque release mineure : y ajouter les preuves listees dans ce fichier et executer `python3 scripts/validate-feature-registry.py`.
