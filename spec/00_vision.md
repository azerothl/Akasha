# Projet Akasha

## Vision
Akasha est un assistant personnel sécurisé, local-first, conçu comme une infrastructure agentique autonome fonctionnant 24h/24 et 7j/7.

Il orchestre dynamiquement une flotte d’agents spécialisés, interagit via de multiples canaux (texte, voix, plateformes externes) et protège strictement les données sensibles.

## Principes Directeurs
- Privacy by design
- High-availability local system
- Self-healing architecture
- Plugin-first extensibility
- Zero secret exposure

## Principes UX
- **Chat** = interaction continue avec le main agent (toujours réactif).
- **Tâches** = objets de workflow qui vivent en parallèle et publient des événements.
- **Onglet « Tâches »** = suivi live (timeline, logs, étapes, erreurs, relance).
- **Onglet « Calendrier »** = vue des récurrences (edit/suppress, exceptions, historique).

## Extension Stratégique
Akasha peut fonctionner :
- En mode standalone local
- En mode cluster multi-machines
- En mode résilient avec fallback modèle local

Le système intègre par défaut un modèle interne local dédié à :
- Comprendre l’architecture Akasha
- Valider la cohérence structurelle
- Assister l’utilisateur lors de l’installation
- Supporter les mises à jour
- Diagnostiquer les erreurs runtime