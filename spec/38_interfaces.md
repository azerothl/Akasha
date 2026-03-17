# Gestion des interfaces et poste client

Ce document décrit la **gestion des interfaces** logicielles (TUI, interface desktop Tauri) et les **interfaces matérielles du poste client** (clavier, affichage, souris, accessibilité, prérequis terminal). Il centralise les informations utiles pour choisir une interface, la lancer, utiliser les onglets et raccourcis, et pour les déploiements (postes partagés, SSH, bureaux distants).

---

## 1. Vue d’ensemble des interfaces logicielles

| Interface | Commande / lancement | Connexion au daemon |
|-----------|----------------------|----------------------|
| **TUI** (terminal) | `akasha tui` | Port 3876 (configurable via `AKASHA_PORT`) |
| **Desktop (Tauri)** | `npm run tauri dev` ou binaire packagé depuis `apps/akasha-ui` | Même port 3876 |

**Prérequis communs** : le daemon doit être démarré (`akasha start` ou `akasha start --foreground`). Sans daemon, les interfaces peuvent s’ouvrir mais les requêtes (chat, métriques, tâches, etc.) échouent.

---

## 2. Lancement

### TUI

```bash
cargo build -p akasha-cli -p akasha-tui
akasha tui
```

Variables d’environnement utiles :

- **`AKASHA_PORT`** : port du daemon (défaut : 3876).
- **`AKASHA_LANG`** : langue de l’interface (prioritaire sur `LANG`). Valeur commençant par `en` = anglais, sinon français.
- **`AKASHA_DATA_DIR`** : répertoire de données (vault, config) si différent du défaut (`~/akasha` ou `%USERPROFILE%\akasha`).

### Interface desktop (Tauri)

```bash
cd apps/akasha-ui
npm install
npm run tauri dev
```

En production : lancer le binaire installé (ex. `akasha-ui.exe` sous Windows). La même variable **`AKASHA_PORT`** est utilisée pour se connecter au daemon.

---

## 3. Onglets et fonctionnalités

| Onglet | TUI | Web (Tauri) | Description |
|--------|-----|-------------|-------------|
| **Chat** | Oui | Oui | Saisie de messages, réponses agent, commandes slash. |
| **Routeur** | Oui | Oui | Métriques du routeur LLM (requêtes, latence, fallback). |
| **Doc** | Oui | Oui | Documentation (user_guide, etc.) servie par le daemon. |
| **Tâches** | Oui | Oui | Liste des tâches, détail, statut, actions requises. |
| **Calendrier** | Oui | Oui | Événements et rappels planifiés. |
| **Mémoire** | Oui | Oui | Mémoire court terme / long terme. |
| **Paramètres** | Non | Oui | Affichage, système, profil agent, data ; RAG utilisateur. |

**Différences principales** :

- **Pièces jointes** (images, documents) : uniquement dans l’interface web (Tauri). En TUI, les messages sont envoyés sans pièces jointes.
- **Message vocal** : lorsque le daemon a un STT configuré (`data_dir/voice_router.yaml` avec `stt.base_url`), l’interface web affiche un bouton micro dans le Chat : premier clic = enregistrement, second clic = arrêt, transcription et envoi du message. Non disponible en TUI.
- **RAG utilisateur** (documents indexés pour le contexte) : gestion dans l’onglet Paramètres en web ; en TUI, possible via l’API (`GET/POST/DELETE /api/user-rag/documents`).
- **Liens cliquables** (fichiers/dossiers locaux dans les réponses) : interface web uniquement (ouverture dans l’explorateur / application par défaut). En TUI, les chemins restent du texte.
- **Affichage d’images générées** (vignettes, « Ouvrir le dossier ») : interface web uniquement.
- **Lecture audio** (réponses TTS en data URL) : interface web uniquement (lecteur audio dans le markdown).

---

## 4. Raccourcis clavier

### TUI

- **Tab** : basculer entre les onglets (Chat, Routeur, Doc, Activité / Tâches).
- **Entrée** : envoyer le message (onglet Chat).
- **↑ / ↓, PgUp / PgDn, Home / End** : défilement du contenu (Chat, Doc, détail des tâches).
- **R** : rafraîchir (métriques Routeur, Doc, liste des tâches).
- **Échap** ou **Ctrl+Q** : quitter.

### Interface web (Tauri)

- **1 à 7** : basculer vers l’onglet (1 = Chat, 2 = Routeur, 3 = Doc, 4 = Tâches, 5 = Calendrier, 6 = Mémoire, 7 = Paramètres). Inactif si le focus est dans un champ de saisie, une zone de texte ou une modale.
- Le focus est maintenu sur le champ de saisie du chat après envoi d’un message lorsque l’onglet Chat est actif.

### Commandes slash (Chat, TUI et Web)

**Les mêmes commandes sont disponibles en TUI et en Web (Tauri).** Exemples : `/help`, `/status`, `/doctor`, `/advice`, `/config list`, `/models`, `/plugins`, `/reload`, `/skills reload`, `/task create "msg"`, `/stop TASK_ID`, `/newsession`, `/restart`, etc. Voir la documentation dans l’onglet Doc ou [user_guide.md](user_guide.md) (section « Commandes slash »). Taper **/help** dans le chat pour la liste complète.

---

## 5. Paramètres d’affichage

- **Thème** : en TUI, thème géré par le terminal (couleurs, thème fichier si supporté). En web (Tauri), choix dans Paramètres → Affichage (thèmes clair/sombre, ex. dark_akasha, light, etc.).
- **Langue** : **`AKASHA_LANG`** (ou `LANG` / `LC_ALL`) pour la TUI ; l’interface Tauri peut réutiliser la même variable selon l’implémentation.
- **Taille de police** : dans la TUI, dépend du terminal. En web, selon les réglages du navigateur/rendu (zoom, préférences utilisateur).

---

## 6. Références croisées

- **Guide utilisateur** : [user_guide.md](user_guide.md) — commandes CLI, config, interfaces, slash, canaux.
- **Onboarding** : [onboarding.md](onboarding.md) — premier lancement, init, daemon, doctor.
- **Référence configuration** : [35_configuration_reference.md](35_configuration_reference.md) — formats et champs des fichiers de config.
- **Architecture UI** : [36_ui_architecture.md](36_ui_architecture.md) — onglets, Task Center, transport.

Pour une description détaillée des interfaces dans le guide utilisateur, voir la section « Interfaces » et « Lancement des interfaces et du daemon » de [user_guide.md](user_guide.md).

---

## 7. Interfaces matérielles du poste client

Les clients Akasha (TUI et application desktop) s’exécutent sur un **poste client**. Cette section décrit les interfaces matérielles attendues et les prérequis pour un usage correct (postes de travail, SSH, bureaux distants).

### 7.1 Clavier

- **Raccourcis globaux** : changement d’onglet (TUI : Tab ; Web : touches 1–7 lorsque le focus n’est pas dans un champ de saisie ou une modale).
- **Focus** : en interface web, le champ de saisie du chat reçoit le focus après envoi d’un message (onglet Chat actif) pour enchaîner sans recliquer.
- **Méthodes de saisie (IME)** : la TUI dépend du support IME du terminal (saisie de langues CJK, etc.). L’interface Tauri utilise le rendu natif du WebView ; le comportement suit l’OS et le moteur (IME standard).
- **Modificateurs** : raccourcis avec **Ctrl** (Windows/Linux) ou **Cmd** (macOS) selon les conventions de l’OS ; **Alt** pour certains raccourcis système. Les raccourcis documentés (ex. Ctrl+Q en TUI) peuvent varier selon la plateforme.

### 7.2 Affichage

- **Résolution** : une résolution minimale raisonnable est recommandée (ex. 1024×768 ou supérieur) pour que les onglets et le contenu restent lisibles. L’interface Tauri s’adapte au redimensionnement de la fenêtre.
- **Thème** : thème clair ou sombre selon les préférences (Paramètres en web ; terminal en TUI).
- **Taille de police** : en TUI, définie par le terminal. En web, selon les réglages de l’application et du rendu.
- **Multi-écran** : l’application Tauri s’ouvre sur l’écran par défaut (comportement standard de l’OS). Le positionnement de la fenêtre dépend du gestionnaire de fenêtres.

### 7.3 Souris et tactile

- **Zones cliquables** : en interface web, liens (URLs, chemins locaux rendus cliquables), boutons, onglets, paramètres. En TUI, la souris peut être utilisée selon les capacités du terminal (sélection, clic pour focus) ; la navigation reste largement au clavier.
- **Scroll** : molette ou tactile pour faire défiler le contenu (Chat, Doc, listes). En TUI, le scroll dépend du terminal (défilement clavier recommandé : PgUp/PgDn, flèches).
- **TUI** : dans un terminal sans support souris ou en session SSH, l’usage de la souris peut être limité ou absent ; la navigation au clavier est prévue.

### 7.4 Accessibilité

- **Lecteurs d’écran** : la TUI (terminal) est compatible avec les lecteurs d’écran dans la mesure où le terminal et le rendu texte le permettent. L’interface Tauri (WebView) peut exposer les rôles et textes pour les technologies d’assistance ; les liens et boutons sont sémantiques (ex. `role="button"` pour les liens d’action).
- **Contraste** : les thèmes proposés (clair/sombre) offrent un contraste adapté au contenu ; l’utilisateur peut choisir le thème dans Paramètres (web) ou via le terminal (TUI).
- **Navigation au clavier** : l’usage sans souris est possible en TUI (Tab, Entrée, flèches, R, Échap). En web, la navigation par onglets et les raccourcis 1–7 permettent de basculer entre les sections sans souris.

### 7.5 Prérequis terminal (TUI)

- **Taille minimale** : un terminal d’au moins 80 colonnes × 24 lignes est recommandé pour un affichage correct des panneaux et du texte. En dessous, le contenu peut être tronqué ou mal mis en forme.
- **Polices** : une police à chasse fixe (monospace) et si possible support des caractères étendus (Unicode) est recommandée pour les messages et la doc.
- **Couleurs** : la TUI utilise les couleurs du terminal (palette 256 ou true color selon le support). Un terminal en 16 couleurs peut suffire avec une dégradation visuelle possible.

Ces prérequis permettent aux utilisateurs et aux équipes (déploiements, postes partagés, accès SSH) de savoir quels périphériques et quelles configurations sont pris en charge pour les clients Akasha.
