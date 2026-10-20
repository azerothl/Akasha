# Observation Studio — issue #156

Implémentation sur la branche `codex/studio-observation`, empilée sur les capacités contextuelles. Aucun scénario navigateur, PTY ou MCP réel exécuté pour cette livraison.

## Contrats

- `list_tools [nom]` expose le manifeste courant. Les rôles planner/reviewer restent soumis à la même restriction au dispatch.
- MCP : un serveur stdio attaché à la fois ; JSON-RPC délimité par retour à la ligne, initialisation négociée, découverte paginée (16 pages, 256 outils maximum), arguments validés contre `inputSchema`. Les noms annoncés conservent une correspondance explicite serveur/outil. Une interruption invalide le catalogue. Rafraîchir via l’API `tools/list` après un changement de catalogue ; les notifications ne déclenchent pas encore de redécouverte automatique. HTTP/SSE et OAuth intégré restent hors de cette tranche.
- Le prompt Studio annonce au plus 32 outils MCP avec un schéma de 4 Kio maximum chacun. Un descripteur découvert n’est pas une preuve de réussite de son prochain appel. Les références de schéma externes sont refusées, avec un résolveur sans accès disque/réseau.
- Browser : Node, runner protocole 2, Playwright >=1.48 et Chromium préinstallés. Studio ne télécharge rien à la première navigation. `navigate`, `snapshot`, `screenshot`, `click`, `fill`, `wait`, `viewport`, `console`, `network`. La politique de domaines est transmise avant chaque action ; les routes HTTP et WebSocket sont filtrées, les service workers bloqués. Ceci ne constitue pas une sandbox réseau du système. Les popups ne sont pas pilotés par cette version.
- PTY agent : `terminal_session start [JSON]`, `list`, `read <id>`, `write <id> {"text":"commande\r"}`, `resize <id> cols rows`, `stop <id>`. Propriétaire projet+tâche contrôlé à chaque opération ; répertoire initial imposé. Permission de l’exécutable et approbation héritée de `run_command`. Un shell autorisé permet des commandes arbitraires : ce n’est pas une sandbox OS.
- PTY utilisateur : collection `/api/studio/projects/:project/terminal/sessions` (GET/POST), puis `/:id/output?cursor=0`, `/:id/input`, `/:id/resize`, DELETE `/:id`. Sortie non consommatrice pour plusieurs vues ; les sessions utilisateur restent distinctes des sessions agent. 6 sessions par projet, 24 au total ; expiration par défaut 900 s, fermeture à la suppression du projet.
- `GET /api/tasks/:task/artifacts` et `/:artifact` : métadonnées puis contenu base64 sur demande. PNG, JSON, texte uniquement ; noms générés, chemins bornés, 16 Mio par fichier et 64 preuves maximum par tâche. Les captures et journaux navigateur signalent une erreur si leur archivage échoue. Le terminal archive un extrait d’au plus 1 Mio à la fermeture explicite/fin de tâche ; cette collecte de nettoyage est best effort. L’expiration automatique ne garantit pas un extrait archivé.

Les codes de sortie PTY concernent le shell. Les réponses réseau ne prouvent pas la réussite d’un scénario. Les journaux navigateur sont bornés et masquent les motifs de secrets usuels, mais cette détection n’est pas exhaustive ; les preuves restent dans les données privées du daemon.

## Validation et acceptation restante

Compilation Windows : `cargo check -p akasha-daemon --no-default-features --features embedded`. Syntaxe runner : `node --check scripts/playwright-runner/run.mjs`. Pas de suites locales ni de recette réelle exécutées.

À valider avant clôture : serveur MCP conforme, pagination et révocation ; navigateur local desktop/mobile, redirection refusée, console et PNG ; PTY clavier, sortie volumineuse, fermeture/expiration, refus inter-tâches et rôle lecture seule ; installation empaquetée avec le runner à jour.

Transport de référence : <https://modelcontextprotocol.io/specification/2025-06-18/basic/transports>. Filtrage WebSocket Playwright : <https://playwright.dev/docs/api/class-browsercontext#browser-context-route-web-socket>.
