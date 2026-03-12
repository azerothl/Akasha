# Génération d’images et affichage dans les interfaces

Ce document décrit la prise en charge de la **génération d’images** par IA (modèle dédié ou multimodal) et l’**affichage des images générées** dans le chat et les interfaces (TUI, Tauri/Web). Il s’appuie sur l’architecture des agents et outils ([33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md)), le routeur LLM ([32_llm_router_architecture.md](32_llm_router_architecture.md)), l’UI ([36_ui_architecture.md](36_ui_architecture.md)) et la configuration ([35_configuration_reference.md](35_configuration_reference.md)).

---

## 1. Vue d’ensemble

### Objectif

Permettre à l’utilisateur de demander une **image générée par IA** (ex. « dessine un chat », « génère une illustration de … ») et afficher cette image dans le fil du chat, comme pour la photo capturée par webcam.

### Périmètre

- **Modèle dédié** (image-only, ex. API Images OpenAI, DALL·E) ou **modèle multimodal** (texte + image en sortie).
- Aucun changement du flux général : orchestrateur → agent → outil ou provider.
- Affichage dans les interfaces existantes (Tauri/Web, TUI) en réutilisant les mécanismes déjà en place pour les images (data URL, Markdown).

---

## 2. Options techniques (modèle)

### Option A — Modèle dédié (API images)

- **Principe** : provider externe (OpenAI Images, DALL·E, ou équivalent OpenRouter/Anthropic s’ils exposent une API images). Nouveau type de requête ou route dédiée (ex. `image_generation`) ; appel API spécifique (prompt → image binaire ou URL) ; résultat renvoyé au daemon puis à l’UI.
- **Intérêt** : APIs matures, qualité contrôlée, pas de charge locale.
- **Inconvénient** : intégration spécifique par provider, coût par image.

### Option B — Modèle multimodal (génération image intégrée)

- **Principe** : modèle capable de renvoyer à la fois du texte et des images (réponse avec pièce jointe image ou URL). Le routeur / le provider existant serait étendu pour parser les réponses « avec image » et exposer l’image au même titre que le texte.
- **Intérêt** : un seul modèle pour texte et image.
- **Inconvénient** : offre limitée aujourd’hui, parsing et format de réponse à standardiser.

### Option C — Outil dédié `generate_image` (recommandé)

- **Principe** : un **outil machine** (comme dans [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md)) : l’agent invoque `generate_image <prompt> [options]` ; le daemon appelle un service (API ou modèle local) et renvoie l’image (base64 ou chemin). Aligné avec l’existant : même flux que `device_invoke local_media camera capture` → image en base64 dans la réponse.
- **Intérêt** : réutilisation du flux actuel (réponse assistant = texte + éventuelle image en Markdown/data URL). Pas de changement du routeur LLM pour la completion texte ; l’agent « creative » (ou dédié) appelle l’outil et injecte l’image dans sa réponse.
- **Référence code** : capture webcam dans `api.rs` — image capturée renvoyée en `![Photo capturée](data:image/jpeg;base64,...)` dans le texte de la réponse.

**Recommandation** : Option C (outil `generate_image`) pour réutiliser le flux actuel et le format d’affichage déjà supporté par les interfaces.

---

## 3. Intégration avec le routeur et les agents

### Classification

- Détection des demandes « génération d’image » (mots-clés, intent) pour router vers l’**agent créatif** (creative) ou un type dédié `image_generation` si besoin.
- L’orchestrateur peut affecter la tâche à l’agent creative qui a accès à l’outil `generate_image`.

### Router LLM

- **Avec Option C** : pas de route dédiée « images » dans le routeur texte. L’outil `generate_image` appelle l’API images **en dehors** du flux chat/completion classique (client HTTP dédié ou module `akasha-llm` étendu avec un provider « images »).
- **Avec Option A** : on pourrait ajouter un type de tâche `image_generation` et un provider « images » dans [32_llm_router_architecture.md](32_llm_router_architecture.md) ; la réponse serait alors une image (binaire ou URL) au lieu d’un texte. Implémentation plus lourde.

### Agent

- L’agent **creative** (ou un agent dédié) doit pouvoir invoquer l’outil `generate_image` et recevoir l’image pour l’injecter dans la réponse utilisateur (Markdown ou champ structuré).
- Format d’appel : `TOOL: generate_image <prompt> [size|style|n]` (syntaxe à préciser selon l’API cible).

---

## 4. Format de sortie côté daemon

### Inline base64 (recommandé pour la v1)

- Comme pour `device_invoke local_media camera capture`, l’image générée est renvoyée en **data URL** (base64) et insérée dans le texte de la réponse en Markdown : `![Description](data:image/png;base64,...)`.
- **Avantages** : aucun changement côté UI (le rendu Markdown affiche déjà les images data URL). Simple et cohérent.

### Alternative : fichier + chemin

- Fichier sauvegardé dans `data_dir` (ex. `data_dir/generated_images/`) avec nom déterministe ou horodaté ; chemin ou URL relative retourné à l’UI pour affichage.
- **Avantages** : persistance, pas de surcharge du flux (gros base64). Tauri peut afficher via lecture du fichier (ex. `read_file_as_data_url` ou équivalent, comme mentionné dans [39_browser_automation.md](39_browser_automation.md) pour les screenshots).
- **Inconvénient** : l’UI doit gérer l’affichage à partir d’un chemin ou d’une URL locale.

La spec retient les **deux** : inline base64 pour la v1 (simplicité) ; option de sauvegarde fichier pour persistance et limites de taille (configurable).

---

## 5. Affichage dans les interfaces

### Tauri / Web

- Le chat affiche déjà les images en Markdown (data URL) pour la photo webcam. **Aucun changement majeur** si l’image générée est renvoyée sous la même forme (`![...](data:image/...)`).
- Option future : affichage des images en pièce jointe structurée (champ `attachments` ou `images[]` dans la réponse de l’API message) si l’API est étendue pour distinguer texte et médias.

### TUI

- Contrainte : terminal sans rendu image natif dans tous les environnements.
- **Comportement proposé** :
  - Si le TUI supporte les images (ex. protocole iTerm2 / Kitty) : affichage inline si disponible.
  - Sinon : message « Image générée : <chemin> » avec chemin du fichier sauvegardé (si option fichier activée) et possibilité d’ouvrir le fichier avec l’application par défaut (lien cliquable ou instruction).
  - Pour une réponse en base64 seule : afficher un placeholder texte du type « [Image générée — ouvrir l’interface Web pour l’affichage] » ou écrire temporairement le fichier dans un répertoire connu et afficher le chemin.

Référence : [36_ui_architecture.md](36_ui_architecture.md) (onglet Chat), [38_interfaces.md](38_interfaces.md) (TUI vs desktop).

---

## 6. Configuration et sécurité

### Configuration

- **Clé API ou endpoint** pour le service de génération d’images : stockage dans le **vault** (ex. `openai_api_key` réutilisé pour DALL·E) ou config dédiée dans `llm_router.yaml` / fichier séparé (ex. `image_generation` avec `provider`, `api_key_ref`, `model`).
- Exemple de structure (indicatif) :
  ```yaml
  image_generation:
    provider: openai   # ou openrouter, local, etc.
    api_key_ref: vault://openai_api_key
    model: dall-e-3
    default_size: 1024x1024
  ```

### Sécurité et modération

- **Politique de contenu** : refus de contenu inapproprié (filtres côté API ou modération optionnelle).
- **Limites** : taille max d’image, nombre d’images par requête ou par période, pour éviter abus et coûts.
- **Approbation utilisateur** : optionnellement placer `generate_image` dans `require_approval` (comme pour `run_command`, etc. dans [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md)) pour valider chaque génération avant appel au provider.

Référence : [35_configuration_reference.md](35_configuration_reference.md), [07_security_model.md](07_security_model.md).

---

## 7. Résumé et critères d’acceptation

| Critère | Description |
|--------|-------------|
| **Outil ou route** | Génération d’image opérationnelle via outil `generate_image` (recommandé) ou route dédiée image_generation. |
| **Retour au client** | Image renvoyée (base64 inline et/ou chemin fichier) et incluse dans la réponse de l’agent (texte Markdown ou champ dédié). |
| **Affichage Tauri/Web** | Image affichée dans le fil du chat (déjà supporté pour data URL en Markdown). |
| **Affichage TUI** | Comportement défini : chemin fichier + ouverture externe, ou placeholder si base64 seul. |
| **Config et sécurité** | Configuration (provider, clé), limites optionnelles, `require_approval` optionnel. |

### Références

- [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md) — Outils machine, format TOOL:, creative agent.
- [32_llm_router_architecture.md](32_llm_router_architecture.md) — Router, providers, task types.
- [36_ui_architecture.md](36_ui_architecture.md) — Onglet Chat, flux UI.
- [35_configuration_reference.md](35_configuration_reference.md) — Formats de config, vault.
- [39_browser_automation.md](39_browser_automation.md) — Affichage screenshots (base64 / chemin) dans l’UI.
