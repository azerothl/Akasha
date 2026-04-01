# Claude `src` (buddy / services / commands) vs Akasha — audit et specs

Référence : analyse des dossiers `buddy`, `services`, `commands` sous le dépôt Claude Code local, alignée sur l’implémentation Akasha (daemon Rust, `akasha-ui`).

## 1. Audit de parité (état constaté)

| Concept (Claude) | Akasha aujourd’hui | Écart |
|------------------|-------------------|--------|
| Export conversation `.txt` | Pas d’export du fil de chat ; export CSV/PNG pour **visualisations** plugins (cartes, etc.) uniquement | Ajout **Export transcript** dans l’onglet Chat (voir implémentation) |
| Titre de session / rename LLM | Tâches avec `initial_message` en base ([`akasha-store`](../../crates/akasha-store/src/tasks.rs)) ; pas de flux « générer un titre » depuis l’UI | Option future : endpoint daemon + bouton « Renommer » (spec ci‑dessous) |
| Résumé une ligne post-outils (Haiku) | Libellés **déterministes** dans l’UI (`tasks.tool_summary_prefix`, événements orchestration) ; pas de second appel LLM pour résumer un batch d’outils | Spec : heuristique locale **ou** modèle léger optionnel (voir §3) |
| Suggestions de prompts suivants | Non | Puces optionnelles sous le champ (contenu + réglage) |
| Tips onboarding | Non | `public/tips.json` + bandeau optionnel |
| MCP OAuth / transports complets | `PluginKind::Mcp` dans [`akasha-plugin-api`](../../crates/akasha-plugin-api/src/kinds.rs) ; hôte à finaliser | Roadmap §5 |
| Buddy / mascotte | Non | Ligne optionnelle type « compagnon » (réglage) |

## 2. Export et nom de fichier (spec)

- **Format** : texte brut, blocs `User:` / `Assistant:` / `System:` (lignes simples).
- **Nom de fichier** : `akasha-chat-YYYY-MM-DD-HHMMSS.txt` ou, si premier message utilisateur disponible, préfixe dérivé (sanitisation alphanum + tirets, max ~50 caractères) — implémenté dans `claudeStyleChat.ts`.
- **Markdown** : option documentée ; l’export actuel reste **.txt** pour simplicité et lisibilité hors ligne.

## 3. Résumé court après outils (spec)

- **Défaut recommandé** : **heuristique** (aucun coût token) : compter les appels par famille d’outil (`read_file`, `grep_content`, `run_command`, …) et produire une phrase du type « Lecture de fichiers, recherche, 1 commande ».
- **Option avancée** : appel LLM **petit contexte** sur le dernier lot d’outils (comme `toolUseSummaryGenerator.ts`), désactivé par défaut, déclenchable par préférence utilisateur ou mode expert — à brancher sur le routeur/modèle existant (`akasha-llm`), hors périmètre du patch UI initial.
- **Point d’accroche UI** : sous les chips de tâches actives ou dans la barre expert ; peut réutiliser les événements déjà streamés (`tool_invoked`, etc.).

## 4. Renommage de tâche (spec)

- **Court terme** : l’utilisateur renomme via liste des tâches si l’UI expose le libellé (sinon demande en langage naturel).
- **Moyen terme** : `POST` daemon `suggest_task_title` avec `task_id` → LLM sur `initial_message` + début de transcript — même idée que `generateSessionName.ts`.

## 5. MCP complet (roadmap)

Ordre de travail plausible :

1. Transport **stdio** et/ou **HTTP** vers processus MCP (config utilisateur).
2. **OAuth** pour serveurs qui l’exigent (cache token local chiffré ou keyring).
3. Cartographie `PluginKind::Mcp` → chargement dynamique dans [`akasha-plugin-host`](../../crates/akasha-plugin-host).
4. Tests avec un serveur MCP de référence (filesystem, fetch).

Fichier [`kinds.rs`](../../crates/akasha-plugin-api/src/kinds.rs) : variante `Mcp` déjà présente pour le typage ; l’intégration runtime reste à faire.

## 6. Buddy (spec)

- **Comportement** : option **purement UI** — courte ligne sous le champ (emoji + phrase d’accueil ou statique), sans second modèle.
- **Futur** : bulle séparée synchronisée sur le transcript (comme `companion_intro` dans Claude) nécessiterait un canal événement dédié côté daemon — hors scope du bandeau minimal.

---

*Document maintenu pour le suivi des todos « intégration inspirée Claude src ».*
