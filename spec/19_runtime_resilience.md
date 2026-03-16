# Runtime & Résilience

## Supervision

- Watchdog interne surveillant tous les agents
- Health check périodique
- Heartbeat obligatoire pour chaque agent

## Redémarrage Automatique

- Redémarrage automatique des sous-agents en cas d’échec
- Limite de tentatives configurable
- Isolation des composants instables

## Auto-Fix

- Tentative de reconfiguration automatique
- Nettoyage mémoire volatile si corruption détectée
- Rechargement dynamique des plugins défaillants

## Persistence de Session

- Reprise d’état après redémarrage
- Restauration des tâches en cours
- Reconnexion automatique aux canaux externes

## Retry Policies (Phase 4 AI OS)

- Configuration (ex. `resilience.yaml` ou section dans config) : nombre de tentatives et backoff pour appels LLM et sous-tâches.
- En cas d’échec temporaire (timeout, 5xx), réessayer selon la policy avant de marquer `Failed`.

## Degraded Mode (Phase 4 AI OS)

- État « degraded » : perte du provider LLM principal ou mémoire indisponible ; le daemon reste actif et retourne une réponse explicite (« mode dégradé »).
- Health endpoint : `GET /api/doctor` retourne `degraded: true` et la cause.
- UI : bandeau « Mode dégradé » et désactivation des actions sensibles si besoin.

## Recovery State (Phase 4 AI OS)

- Au démarrage, après marquage des tâches interrompues, enregistrement « recovery » dans l’immutable log (ex. `recovery_started`, liste des task_id concernés).
- Optionnel : endpoint `GET /api/recovery` pour la dernière fenêtre de recovery.