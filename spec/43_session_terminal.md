# Session terminal (PTY) — optionnel (spec 33)

## Statut

**Session terminal interactive (PTY)** : **tranche 1** exposée côté daemon (HTTP + `portable-pty`). Voir `GET /api/terminal/capabilities` (`pty_api`).

## Actuellement

- **run_command** / **run_terminal** : exécution d’une commande avec timeout et politique (`allowed_commands`). Idéal pour une commande unique (ex. `ls`, `git status`).
- **run_command_background** : lancement en arrière-plan avec `session_id` ; **process poll/kill** pour suivre ou arrêter.

L’agent peut donc déjà « utiliser le terminal » au sens d’exécuter des commandes et d’en récupérer la sortie.

## API HTTP (daemon)

| Méthode | Chemin | Rôle |
|--------|--------|------|
| `POST` | `/api/terminal/pty/sessions` | Créer une session. Corps JSON : `argv?`, `cwd?`, `cols`, `rows`. Réponse : `{ "session_id" }`. |
| `GET` | `/api/terminal/pty/sessions/{id}/output?max=8192` | Lire jusqu’à `max` octets depuis le tampon de sortie (retour `data_b64`). |
| `POST` | `/api/terminal/pty/sessions/{id}/input` | Corps : `{ "text": "..." }` et/ou `bytes_b64`. |
| `POST` | `/api/terminal/pty/sessions/{id}/resize` | Corps : `{ "cols": 80, "rows": 24 }`. |
| `DELETE` | `/api/terminal/pty/sessions/{id}` | Fermer la session (tue le processus enfant). |

CLI opérateur : `akasha terminal capabilities` (requiert le daemon).

## Suite (optionnel)

- Persistance / reprise de session, transcript disque, timeouts d’inactivité côté daemon.
- Outil agent `terminal_session` branché sur cette API (aujourd’hui message d’orientation vers HTTP + `run_command`).
- UI pour afficher/saisir (Tauri, TUI, Code Studio).

## Références

- [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md) — capacité « Utiliser le terminal » (optionnel).
- Outils actuels : `run_command`, `run_terminal`, `run_command_background`, `process`.
