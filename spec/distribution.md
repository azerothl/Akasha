# Distribution aux utilisateurs (sans compiler)

Ce document décrit comment **obtenir et utiliser Akasha sans installer Rust ni compiler** — pour des utilisateurs finaux ou des testeurs.

---

## 1. Télécharger les binaires précompilés

### Via GitHub Releases (recommandé)

1. Allez sur la page **Releases** du dépôt (ex. `https://github.com/VOTRE_ORG/akasha/releases`).
2. Choisissez la dernière version (ex. **v0.1.0**).
3. Téléchargez l’archive correspondant à votre système :
   - **Windows** : `akasha-windows-x86_64.zip`
   - **Linux** : `akasha-linux-x86_64.zip`
   - **macOS Intel** : `akasha-macos-x86_64.zip`
   - **macOS Apple Silicon (M1/M2/M3)** : `akasha-macos-aarch64.zip`
   - **App desktop (Tauri)** : installateurs dans les pièces jointes (`.msi`/`.exe`, `.dmg`, `.deb`/`.AppImage`).
4. Décompressez l’archive dans un dossier (ex. `C:\Akasha` ou `~/Akasha`). Installez l'app desktop avec l'installateur de votre OS si besoin.

**Installation rapide (recommandé)** : après extraction, exécutez le script d’installation qui copie les binaires, lance l’init et enregistre le daemon au démarrage :
- **Windows** : `.\scripts\install.ps1` (PowerShell). Optionnel : `-InstallDir C:\Akasha`, `-NoAutoStart`.
- **Linux / macOS** : `./scripts/install.sh`. Optionnel : `--dir /usr/local/bin`, `--no-auto-start`.

Vous obtenez (dans l'archive CLI) :
- **akasha** (ou `akasha.exe`) — CLI : init, start, stop, doctor, tui, config…
- **akasha-daemon** — serveur 24/7 (lancé par `akasha start`)
- **akasha-tui** — interface en terminal (lancée par `akasha tui`)
- **docs/user_guide.md** — documentation utilisateur (guide pour les binaires uniquement)

**Documentation dans l’interface** : lancez `akasha start` depuis le dossier où vous avez extrait l’archive. L’onglet **Doc** des interfaces (TUI et Web) affiche alors cette documentation. Si le fichier `docs/user_guide.md` est absent du zip, l’onglet Doc affichera « Documentation non disponible ».

---

## 2. Premier lancement

### Avec le script d’installation (recommandé)

Après avoir extrait l’archive, exécutez le script : il déploie les binaires, lance `akasha init --defaults` et enregistre le daemon pour qu’il démarre au prochain logon.

- **Windows** : `.\scripts\install.ps1`
- **Linux / macOS** : `chmod +x scripts/install.sh && ./scripts/install.sh`

Puis démarrez le daemon (ou redémarrez la session pour le lancement automatique) : `akasha start`.

### Installation manuelle

### Windows (PowerShell ou CMD)

```powershell
cd C:\Chemin\Vers\Akasha
.\akasha.exe init
.\akasha.exe start
```

Pour l’interface en terminal : `.\akasha.exe tui`

### Linux / macOS

```bash
cd ~/Akasha
chmod +x akasha akasha-daemon akasha-tui
./akasha init
./akasha start
```

Pour l’interface en terminal : `./akasha tui`

---

## 3. (Optionnel) Mettre les binaires dans le PATH

Pour pouvoir lancer `akasha` depuis n’importe quel dossier :

- **Windows** : ajoutez le dossier contenant `akasha.exe` aux variables d’environnement **Path** (Paramètres → Système → À propos → Paramètres système avancés → Variables d’environnement).
- **Linux / macOS** : copiez ou liez les binaires dans un dossier déjà dans le PATH, par exemple :
  ```bash
  sudo cp akasha akasha-daemon akasha-tui /usr/local/bin/
  ```

---

## 4. Créer une release (mainteneurs)

Les binaires sont produits automatiquement par **GitHub Actions** à chaque tag de version.

1. Vérifier que le workflow `.github/workflows/release.yml` est présent.
2. Créer un tag et le pousser :
   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```
3. Attendre la fin du workflow (Actions).
4. La release apparaît sous **Releases** avec les archives par plateforme.

**Note** : Sous Windows, le daemon est compilé sans la mémoire long terme (ONNX) pour éviter les erreurs de liaison. Les utilisateurs Windows qui veulent la mémoire long terme peuvent utiliser WSL2 et la version Linux.

---

## 5. Application desktop (Tauri)

L’interface graphique **Akasha UI** (Tauri + React) est construite par le workflow Release et publiée sur la page Releases (installateurs en pièces jointes). Lancez le daemon avec `akasha start`. Pour construire en local :

```bash
cd apps/akasha-ui
npm install
npm run tauri build
```

Les artefacts sont dans `apps/akasha-ui/src-tauri/target/release/bundle/`. Icônes : `node scripts/gen-ico.js` à la racine si besoin.

L’utilisateur doit dans tous les cas **lancer le daemon** (ou l’avoir déjà lancé) pour que l’UI puisse communiquer avec l’API. À terme, le binaire daemon peut être fourni en **sidecar** de l’app Tauri pour démarrage automatique.

---

## 6. Déploiement du site après release (mainteneurs)

Lorsqu’un tag de version est poussé (ex. `v0.1.0`), le workflow **Release** crée la release GitHub puis envoie un **repository_dispatch** au dépôt [azerothl/Akasha_app](https://github.com/azerothl/Akasha_app). Ajouter le secret **AKASHA_APP_DISPATCH_TOKEN** (PAT ou token avec Actions read/write sur Akasha_app). Event : `new_release`. Payload : `version`, `tag`, `release_url`, `repository`, `release_date`, `release_type`, `title`, `description`, `download_url`, `changelog`. Pour qu'Akasha_app récupère les artefacts (zip) depuis le dépôt privé Akasha, ajouter dans Akasha_app le secret **AKASHA_RELEASE_READ_TOKEN** (lecture sur azerothl/Akasha). Exemple de workflow : `.github/akasha_app_update_release_workflow.example.yml`.

---

## 7. Résumé pour l’utilisateur lambda

| Étape | Action |
|-------|--------|
| 1 | Aller sur GitHub → Releases → dernière version |
| 2 | Télécharger le zip pour son OS (Windows / Linux / Mac) |
| 3 | Décompresser dans un dossier |
| 4 | Lancer `akasha init` (premier lancement) puis `akasha start` |
| 5 | Optionnel : `akasha tui` pour l’interface en terminal, ou ouvrir l’app desktop si installée |

Aucune compilation ni installation de Rust nécessaire.
