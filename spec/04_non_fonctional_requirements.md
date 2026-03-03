# Non Functional Requirements

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