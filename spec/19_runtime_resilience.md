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