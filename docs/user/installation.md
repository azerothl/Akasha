# Installation

## ## 1. Obtenir et lancer Akasha


### Téléchargement

1. Rendez-vous sur la page **Releases** du dépôt Akasha (ex. GitHub).
2. Pour une installation complète en une étape, téléchargez l'archive **« Akasha full »** correspondant à votre système (ex. `akasha-full-windows-x86_64.zip`, `akasha-full-linux-x86_64.zip`, `akasha-full-macos-x86_64.zip`). Sinon, téléchargez l'archive CLI (akasha, daemon, TUI) et, si besoin, l'archive de l'application desktop (Tauri) séparément.
3. Décompressez l'archive dans un dossier (ex. `C:\Akasha` ou `~/Akasha`).

Vous obtenez les exécutables **akasha** (ou akasha.exe), **akasha-daemon** et **akasha-tui**, le dossier **scripts** (install et setup), **docs**, un sous-dossier **spec/** (fichiers d'exemple pour la configuration), et dans le zip « full » un sous-dossier **ui** contenant l'installateur de l'application desktop.

### Installation recommandée (installeur unifié)

Après avoir extrait le zip **full** :

- **Windows (PowerShell)** : exécutez `.\scripts\setup.ps1`. Le script vous demandera : installer l'application desktop (interface web) ? Démarrer le daemon à chaque connexion ? Il installe les binaires, lance le premier `akasha init`, démarre le daemon une fois et, si vous le souhaitez, enregistre le daemon au démarrage de Windows et installe l'app Tauri.
- **Linux / macOS** : exécutez `./scripts/setup.sh` (ou `bash scripts/setup.sh`). Mêmes choix (application desktop, daemon au démarrage). Le script installe les binaires, lance l'init, démarre le daemon une fois et, sous Linux (systemd) ou macOS (launchd), peut enregistrer le daemon pour qu'il démarre à chaque connexion.

**Redémarrage en cas de crash** : si vous avez activé le démarrage automatique, le daemon est relancé en cas d'arrêt — sous Windows via le superviseur intégré à `akasha start`, sous Linux/macOS via systemd ou launchd.

### Installation manuelle (sans setup)

**Windows (PowerShell ou CMD)**  
Ouvrez un terminal dans le dossier où vous avez extrait l'archive, puis :

```
.\scripts\install.ps1
.\akasha.exe start
```

(Optionnel : `-NoAutoStart` pour ne pas enregistrer le daemon à la connexion.)

**Linux / macOS**  
Dans un terminal, depuis le dossier d'extraction :

```bash
chmod +x akasha akasha-daemon akasha-tui scripts/*.sh
./scripts/install.sh
./akasha start
```

(Optionnel : `--no-auto-start` pour ne pas enregistrer le daemon au démarrage.)

Pour afficher l'interface en terminal : `akasha tui` (ou `.\akasha.exe tui` sous Windows).

### Prérequis

- **Modèle embarqué** : par défaut Akasha utilise un modèle LLM intégré (akasha_embedded). Aucune installation externe n'est obligatoire pour recevoir des réponses.
- **Ollama** (optionnel) : pour utiliser d'autres modèles locaux. Configurez-le lors de l'initialisation ou plus tard via `akasha config models set conversation ollama <modèle>`.
- **Cloud** (optionnel) : OpenAI ou OpenRouter, configurés lors de l'init (clés dans le vault ou variables d'environnement).
- **Rust** : inutile pour les binaires précompilés.
- **Node.js** (optionnel) : nécessaire seulement pour l’outil **navigateur géré** (Playwright). Les archives de release incluent le dossier `playwright-runner` à côté des exécutables ; installez [Node.js](https://nodejs.org/) (npm inclus) si vous utilisez cette fonctionnalité. Au premier lancement d’une tâche navigateur, le daemon peut exécuter `npm install` et télécharger Chromium — cela peut prendre plusieurs minutes selon la connexion. Pour désactiver l’installation automatique des dépendances Playwright, définissez `AKASHA_PLAYWRIGHT_AUTO_INSTALL=0` (variable d’environnement ou entrée dans `akasha.env`). Le diagnostic `akasha doctor` (daemon actif) indique si le runner, Node/npm et le paquet Playwright sont détectés.

**Important** : lancez `akasha start` depuis le dossier d'installation (ou après avoir ajouté ce dossier au PATH) afin que l'onglet **Doc** des interfaces affiche cette documentation.

---
