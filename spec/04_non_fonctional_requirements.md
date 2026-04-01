# Non Functional Requirements

> **Spécification détaillée** — une section par NFR. Résumé liste seule : [02_non_functional_requirements.md](02_non_functional_requirements.md). *Nom de fichier : orthographe historique « fonctional », aligné sur [03_fonctional_requirements.md](03_fonctional_requirements.md).*

NFR-001 — Disponibilité continue
> Le système doit fonctionner 24h/24 et 7j/7.

NFR-002 — Redémarrage automatique
> En cas de crash d’un composant, celui-ci doit redémarrer automatiquement.

NFR-003 — Self-Healing
> Le système doit détecter les erreurs critiques et tenter une correction automatique avant escalade.

NFR-004 — Résilience Agentique
> Le crash d’un sous-agent ne doit jamais affecter le main agent.

NFR-005 — Multi-Canal
> L’agent doit supporter plusieurs canaux d’interaction simultanément.

NFR-006 — Extensibilité par Plugin
> Tout besoin non standard doit pouvoir être implémenté via plugin sans modification du core.

NFR-007 — Sécurité
> Les mécanismes d’auto-restart ne doivent jamais exposer les secrets.

NFR-008 — Temps réel UI
> Les updates de progression doivent arriver en < 1s en local, < 2s en cluster LAN.

NFR-009 — Robustesse scheduler
> Une tâche récurrente ne doit pas être perdue après crash/restart (persistence).

NFR-010 — Exactly-once (at-least-once + dédup)
> Les occurrences récurrentes doivent être déclenchées avec déduplication (run_id).