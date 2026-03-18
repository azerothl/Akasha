# Spécification — Automation navigateur (outil browser)

Ce document décrit l’intégration d’une capacité d’**automation navigateur** dans Akasha, sur le modèle d’OpenClaw : un navigateur géré (headless ou visible) piloté par l’agent via des actions explicites (navigate, click, fill, screenshot, snapshot), avec politique de sécurité et intégration à l’outil `browser` déjà déclaré dans le daemon.

---

## 1. Contexte et objectifs

### 1.1 Besoin

Permettre à l’agent d’effectuer des actions dans un navigateur web de manière autonome :

- Ouvrir une URL et naviguer vers une page.
- Remplir des formulaires (champs texte, sélection, etc.).
- Cliquer sur des boutons, liens ou éléments interactifs.
- Capturer une capture d’écran de la page.
- Extraire le contenu textuel ou la structure (DOM) de la page pour l’injecter dans le contexte du LLM.

L’équivalent visé est le **navigateur géré** d’OpenClaw (sans Browser Relay) : une instance Chromium/Chrome lancée et contrôlée par le runtime, en mode **headless** (sans fenêtre) ou **headed** (fenêtre visible sur la machine de l’utilisateur).

### 1.2 État actuel dans Akasha

- L’outil **`browser`** est déjà déclaré dans la liste `AVAILABLE_TOOLS` du daemon ([api.rs](../crates/akasha-daemon/src/api.rs), vers L561), avec la description :  
  `browser navigate <url> | browser screenshot | browser snapshot — automation navigateur (non implémenté, prévu phase 3)`.
- Le handler associé renvoie actuellement :  
  `[browser] browser automation not implemented (planned Phase 3)`.

La présente spécification précise le **comportement attendu**, l’**architecture** et la **politique de sécurité** pour une implémentation future.

---

## 2. Périmètre fonctionnel

### 2.1 Actions de l’outil `browser`

L’agent invoque l’outil via des lignes `TOOL: browser <sous-commande> [arguments...]`. Les sous-commandes suivantes sont spécifiées :

| Sous-commande | Arguments | Description |
|---------------|-----------|-------------|
| `navigate` | `<url>` | Ouvrir l’URL dans l’instance de navigateur gérée. L’URL doit appartenir à un domaine autorisé par la politique (`browser_allowed_domains`). |
| `click` | `<selector>` | Cliquer sur un élément identifié par un sélecteur CSS (ou équivalent). Ex. `#submit`, `button.primary`, `a[href="/login"]`. |
| `fill` | `<selector>` `<value>` | Remplir un champ (input, textarea) avec la valeur fournie. Le sélecteur cible l’élément ; la valeur peut être échappée si elle contient des espaces (convention à définir, ex. guillemets ou JSON). |
| `screenshot` | [optionnel : chemin ou `inline`] | Capturer une capture d’écran de la page courante. Si un chemin est fourni (sous `allowed_write_paths`), y enregistrer l’image ; sinon stocker dans un répertoire dédié (ex. `data_dir/browser_screenshots/`) et retourner le chemin ou un identifiant. Option `inline` : retourner une représentation base64 pour affichage dans l’UI. |
| `snapshot` | — | Extraire le contenu textuel ou une représentation simplifiée du DOM de la page courante (texte visible, structure des liens et formulaires) et le renvoyer dans la réponse de l’outil pour alimenter le contexte du LLM. |
| `wait` (optionnel) | `<selector>` [timeout_ms] | Attendre qu’un élément correspondant au sélecteur soit présent (ou visible). Timeout optionnel en millisecondes. Utile pour les pages à chargement dynamique (JavaScript). |

Le format exact des arguments (séparateurs, échappement) est à fixer lors de l’implémentation (alignement avec le parsing existant des autres outils dans le daemon).

### 2.2 Modes d’exécution

- **Headless (défaut)** : le navigateur s’exécute sans fenêtre visible. Adapté aux tâches en arrière-plan, extraction de données, tests automatisés.
- **Headed** (`browser_headless: false` dans la politique) : une fenêtre de navigateur est affichée sur la machine de l’utilisateur. Adapté au débogage et aux cas où l’utilisateur doit voir les actions de l’agent (connexion à un site, remplissage de formulaire).

### 2.3 Durée de vie de l’instance

- **Une instance de navigateur par tâche** (ou par session de conversation) : chaque tâche qui utilise l’outil `browser` dispose de sa propre instance. En fin de tâche (succès, échec ou annulation), l’instance est fermée pour libérer les ressources.
- Alternative envisageable : une instance partagée avec un timeout d’inactivité (plus complexe à gérer, risque de fuite de contexte entre tâches). La spec recommande **une instance par tâche** pour la V1.

---

## 3. Architecture et options techniques

### 3.1 Option A — Playwright (recommandé pour la V1)

- **Principe** : utiliser [Playwright](https://playwright.dev/) pour lancer un navigateur Chromium (ou Chrome/Edge), exécuter les actions (navigate, click, fill, screenshot) et récupérer les résultats.
- **Intégration possible** :
  - **Sous-processus** : le daemon invoque le CLI Playwright ou un script (Node/Python) qui exécute les actions et renvoie le résultat (stdout, fichier screenshot). Dépendance : binaire Playwright + Node ou Python sur la machine.
  - **Crate Rust** : si une crate Rust stable existe pour piloter Playwright (ou CDP directement), l’intégration peut se faire en natif dans le daemon ou dans un crate dédié `akasha-browser`.
- **Avantages** : aligné avec les pratiques OpenClaw, gestion robuste des attentes (wait for selector), sélecteurs, screenshots ; documentation abondante.
- **Inconvénients** : dépendance externe (Node/Python + Playwright ou crate Rust) ; installation initiale des navigateurs (ex. `npx playwright install chromium`).

### 3.2 Option B — CDP (Chrome DevTools Protocol)

- **Principe** : le daemon lance un processus Chromium avec un port CDP exposé (ou se connecte à un serveur local type « browser relay »), puis envoie des commandes CDP (Page.navigate, DOM.querySelector, Input.dispatchMouseEvent, etc.) via une bibliothèque Rust (ex. crate `chrome-devtools-protocol` ou équivalent).
- **Avantages** : pas de dépendance à Playwright ; contrôle fin ; possibilité à terme de se connecter au navigateur de l’utilisateur (relay) si une extension ou un serveur local expose CDP.
- **Inconvénients** : plus de code à maintenir (gestion des attentes, sélecteurs, screenshots via CDP) ; API CDP volumineuse et évolutive.

### 3.3 Recommandation

- **V1** : privilégier **Playwright** (sous-processus ou crate) pour réduire le temps de développement et bénéficier de la maturité de l’écosystème. La décision finale (Node vs Python vs crate Rust) pourra être prise selon les contraintes de déploiement (éviter Node/Python si l’écosystème Akasha est 100 % Rust).
- **Emplacement du code** : logique dans le **daemon** (module dédié ou crate `akasha-browser`) ; le flux d’orchestration existant reste inchangé : l’agent appelle l’outil `browser` comme les autres outils, le daemon délègue à un exécuteur (Playwright ou CDP) et renvoie le résultat dans le message de l’outil.

### 3.4 Installation automatique des navigateurs (daemon)

Lorsque l’initialisation Chromium échoue faute de binaire Playwright (message d’erreur typique : exécutable introuvable), le daemon exécute **une fois** (verrou global si plusieurs tâches concurent) dans le répertoire du runner (`scripts/playwright-runner`, parent de `run.mjs`) :

1. `npm install --no-audit --no-fund`
2. `npx playwright install chromium`

Puis il **réessaie** la création de session. Nécessite **Node.js** et **npm** sur la machine hôte et un accès réseau pour le téléchargement.

- **Désactivation** : variable d’environnement `AKASHA_PLAYWRIGHT_AUTO_INSTALL=0` — dans ce cas, seul le message d’erreur initial est retourné (comportement manuel : `npm install` + `npx playwright install chromium` dans le répertoire du runner).

---

## 4. Sécurité et politique

### 4.1 Extension de `tools_policy.yaml`

Les clés suivantes sont ajoutées à la politique des outils (fichier `data_dir/tools_policy.yaml`). Référence : [35_configuration_reference.md](35_configuration_reference.md), [tools_policy.example.yaml](tools_policy.example.yaml).

| Clé | Type | Défaut | Description |
|-----|------|--------|-------------|
| `browser_enabled` | booléen | `false` | Activer l’outil `browser`. Si absent ou false, toute invocation de `browser` est refusée. |
| `browser_allowed_domains` | liste de strings | `[]` | Domaines autorisés pour la navigation (`navigate`). Ex. `["*"]` pour tout autoriser, ou `["example.com", "login.example.com"]` pour une liste blanche. |
| `browser_blocked_domains` | liste de strings | `[]` | Domaines interdits ; prioritaire sur `browser_allowed_domains`. Ex. `["internal.corp", "localhost"]` pour bloquer des domaines sensibles. |
| `browser_headless` | booléen | `true` | Exécuter le navigateur en mode headless (true) ou afficher une fenêtre (false). |
| `browser_action_timeout_secs` | entier | `30` | Timeout par action (navigate, click, fill, etc.) en secondes. |
| `browser_session_timeout_secs` | entier | `300` | Timeout global de la session navigateur (5 min par défaut) ; au-delà, l’instance est fermée. |

L’outil `browser` peut en outre être ajouté à la liste **`require_approval`** pour exiger une confirmation utilisateur avant chaque navigation ou avant chaque séquence d’actions (choix produit à trancher en implémentation).

### 4.2 Isolation

- Le navigateur géré **ne partage pas** les cookies, l’historique ni les sessions du navigateur personnel de l’utilisateur. Un **profil dédié** (répertoire temporaire ou sous `data_dir`) est utilisé pour chaque instance.
- Aucune lecture des données du navigateur système (mots de passe, cookies existants) n’est effectuée.

### 4.3 Timeouts et processus

- Chaque action (navigate, click, fill, screenshot, snapshot) est limitée par `browser_action_timeout_secs`.
- La session navigateur (une instance par tâche) est limitée par `browser_session_timeout_secs`. À l’expiration ou à la fin de la tâche, le processus du navigateur est terminé proprement pour éviter les processus fantômes.

---

## 5. Intégration au flux existant

### 5.1 Parsing des invocations

- Le daemon parse déjà les lignes de la forme `TOOL: nom arg1 arg2 ...`. Pour `browser`, le parsing doit accepter une **sous-commande** en premier argument :  
  `TOOL: browser navigate https://example.com`  
  `TOOL: browser click "button#submit"`  
  `TOOL: browser fill "input#email" "user@example.com"`  
  `TOOL: browser screenshot`  
  `TOOL: browser snapshot`
- Le format exact (guillemets, échappement des espaces dans les valeurs) est à définir de manière cohérente avec les autres outils (ex. `search_replace` avec séparateur ` | `).

### 5.2 Retour et message de l’outil

- Le résultat de chaque action est renvoyé sous forme de **message texte** dans la réponse de l’outil, par ex. :
  - `[browser] Navigated to https://example.com (title: Example Domain).`
  - `[browser] Clicked element button#submit.`
  - `[browser] Screenshot saved to <path> | or [browser] Screenshot (inline): <base64>.`
  - `[browser] Snapshot: <extrait texte ou structure simplifiée>.`
- En cas d’erreur (timeout, domaine non autorisé, sélecteur introuvable), un message d’erreur explicite est renvoyé (ex. `[browser] Domain not allowed: ...`, `[browser] Timeout waiting for selector ...`).

### 5.3 Affichage des captures d’écran dans l’UI

- Si une capture d’écran est produite et stockée dans un chemin connu (ou renvoyée en base64), l’**interface Tauri** peut l’afficher dans le fil de conversation, de la même manière que les images générées (voir implémentation existante des chemins d’images et `read_file_as_data_url`). Option : lien cliquable vers le fichier ou affichage inline selon le choix d’implémentation.

---

## 6. Dépendances et déploiement

### 6.1 Binaire Chromium / navigateur

- Une **installation de Chromium** (ou Chrome/Edge compatible) est nécessaire sur la machine. Avec Playwright, l’installation se fait typiquement via :
  - `npx playwright install chromium` (Node),
  - ou `playwright install chromium` (Python),
  - ou l’équivalent fourni par la crate Rust si utilisée.
- La spec recommande de **documenter** cette dépendance dans le guide utilisateur et, si pertinent, de proposer une commande d’installation (ex. `akasha browser install` ou étape dans `akasha init`) pour vérifier ou installer les binaires Playwright/Chromium.

### 6.2 Côté Rust

- Si l’implémentation utilise un **sous-processus** (Node/Python + Playwright), aucune dépendance Cargo supplémentaire pour Playwright ; en revanche, l’environnement d’exécution doit disposer de Node ou Python et du module Playwright.
- Si une **crate Rust** est utilisée (CDP ou binding Playwright), l’ajouter dans `Cargo.toml` du daemon ou du crate dédié.

---

## 7. Phases d’implémentation

| Phase | Périmètre |
|-------|------------|
| **Phase 1** | Implémentation minimale : `browser navigate <url>` + `browser snapshot` (extraction texte/DOM) en headless. Politique : `browser_enabled`, `browser_allowed_domains`, `browser_blocked_domains`. Pas d’affichage UI des screenshots. |
| **Phase 2** | Ajout de `browser click`, `browser fill`, `browser screenshot` ; option `browser_headless: false` ; stockage des screenshots et affichage dans l’UI (lien ou inline) si possible. Timeouts et fermeture de session. |
| **Phase 3** | Optimisations ; `browser wait` pour contenu dynamique ; gestion multi-onglets si pertinent ; option avancée type « browser relay » (connexion à un navigateur existant via CDP) pour réutilisation du navigateur de l’utilisateur. |

---

## 8. Références et mises à jour des autres documents

### 8.1 À mettre à jour lors de l’implémentation

- **[33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md)** : remplacer la ligne du tableau concernant `browser` (actuellement « Prévu phase 3 ») par une description des sous-commandes et un renvoi vers la présente spec :  
  `browser | browser navigate <url> \| click <selector> \| fill <selector> <value> \| screenshot \| snapshot [\| wait <selector>] — automation navigateur (voir 39_browser_automation.md).`
- **[35_configuration_reference.md](35_configuration_reference.md)** : dans la section **tools_policy.yaml**, ajouter les clés `browser_enabled`, `browser_allowed_domains`, `browser_blocked_domains`, `browser_headless`, `browser_action_timeout_secs`, `browser_session_timeout_secs` avec type, défaut et description.
- **tools_policy.example.yaml** : ajouter un bloc commenté d’exemple pour les options `browser_*`.

### 8.2 Documents liés

- [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md) — Outils machine et politique.
- [35_configuration_reference.md](35_configuration_reference.md) — Référence des fichiers de configuration.
- [tools_policy.example.yaml](tools_policy.example.yaml) — Exemple de politique des outils.
