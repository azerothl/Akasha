# Session terminal (PTY) — optionnel (spec 33)

## Statut

**Session terminal interactive (PTY)** : prévue pour une version ultérieure. Décision documentée.

## Actuellement

- **run_command** / **run_terminal** : exécution d’une commande avec timeout et politique (`allowed_commands`). Idéal pour une commande unique (ex. `ls`, `git status`).
- **run_command_background** : lancement en arrière-plan avec `session_id` ; **process poll/kill** pour suivre ou arrêter.

L’agent peut donc déjà « utiliser le terminal » au sens d’exécuter des commandes et d’en récupérer la sortie.

## Prévu (optionnel)

- Session **PTY** interactive : stdin/stdout/stderr, timeout, isolation.
- Exposition via outil dédié (ex. `terminal_session start` / `terminal_session send` / `terminal_session stop`) ou API.
- UI pour afficher/saisir (Tauri, TUI).

Livrable cible : agent peut ouvrir une session terminal interactive, envoyer des lignes, recevoir la sortie, fermer la session.

## Références

- [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md) — capacité « Utiliser le terminal » (optionnel).
- Outils actuels : `run_command`, `run_terminal`, `run_command_background`, `process`.
