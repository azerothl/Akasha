# Guide utilisateur Akasha

Bienvenue dans la documentation intégrée d'Akasha. Utilisez le menu à gauche (interface desktop) ou les pages ci-dessous pour naviguer.

## Sommaire

| Page | Contenu |
|------|---------|
| Installation | Téléchargement, scripts setup, premier lancement |
| Configuration | Fichiers du data_dir, variables d'environnement, exemples YAML |
| Commandes | CLI `akasha` et commandes slash dans le chat |
| Interfaces | TUI et application desktop : onglets, raccourcis |
| Mission autonome | Heartbeats, objectifs de fond, rapports |
| Données et mémoire | RAG utilisateur, mémoire, graphe projet |
| Workspace | Cookbook, Comparer, Recherche, Notes (v0.9) |
| Skills, plugins et canaux | Extensions, galerie, Telegram, Matrix, etc. |
| Dépannage | `akasha doctor`, problèmes fréquents |
| Nouveautés | Changements récents |

## Parcours rapide

1. Téléchargez le zip **full** (CPU) ou **full CUDA** (Windows NVIDIA) — voir [Installation](installation.md)
2. `setup.ps1` / `setup.sh` puis `akasha start`
3. **Assistant premier lancement** (wizard) : statut embarqué, téléchargement GGUF si CUDA, premier message test
4. `akasha doctor` pour vérifier l'installation (`action: embedded-download` si GGUF manquant)
5. Chat dans l'application desktop ou `akasha tui`

Le daemon écoute par défaut sur le port **3876** (`AKASHA_PORT`).
