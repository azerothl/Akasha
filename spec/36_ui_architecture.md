# Architecture UI — Chat, Tâches, Calendrier

Ce document décrit l’architecture des interfaces utilisateur (TUI et Web) : onglets, flux temps réel et transport. Il s’appuie sur les [exigences fonctionnelles FR-025 à FR-029](03_fonctional_requirements.md) (chat non bloquant, onglet Tâches, suivi temps réel, tâches récurrentes, onglet Calendrier) et sur [NFR-008](04_non_fonctional_requirements.md) (temps réel UI).

---

## 1. Onglets

### 1.1 Chat

- **Contenu** : messages utilisateur / main agent + mini « status chips » (ex. « Task #1234 running (35 %) »).
- **Commandes rapides** : stop task 1234, show tasks, schedule …, selon le contexte.
- **Comportement** : le chat reste toujours réactif ; une tâche longue ne bloque pas l’interface (voir [comportement conversationnel](33_agents_tools_orchestrator_skills.md)).

### 1.2 Tâches (Task Center)

- **Liste** : tâches en cours, terminées, échouées ; filtres et recherche.
- **Détail par tâche** : graphe des sous-tâches, timeline d’événements, logs, artefacts, actions (relance, pause, reprendre, annuler selon permissions).
- **Mise à jour** : flux temps réel (progress_update en streaming, pas de refresh manuel).

Référence : FR-026, FR-027.

### 1.3 Calendrier

- **Vue** : récurrences + prochaines occurrences (mois / semaine / jour).
- **Actions** : CRUD sur les récurrences (edit, supprimer, pause) ; exceptions (skip une occurrence).
- **Lien** : accès aux runs historiques (task_run, task_id).

Référence : FR-029.

---

## 2. Transport temps réel

- **Local (standalone)** : WebSocket interne ou SSE entre daemon et UI ; mises à jour de progression et d’état sans polling.
- **Cluster** : NATS (pub/sub) côté backend → gateway → WebSocket ou SSE vers l’UI ; les clients UI se connectent au gateway, pas directement à NATS.

Les mises à jour de progression doivent arriver en **&lt; 1 s** en local et **&lt; 2 s** en cluster LAN (NFR-008).

---

## 3. Synthèse

| Onglet      | Contenu principal                          | Référence   |
|-------------|--------------------------------------------|-------------|
| Chat        | Messages + chips tâches + commandes rapides | FR-025      |
| Tâches      | Liste, détail, timeline, logs, actions      | FR-026, FR-027 |
| Calendrier  | Récurrences, occurrences, CRUD, exceptions | FR-028, FR-029 |

Transport : WS/SSE (local), NATS → gateway → WS/SSE (cluster).
