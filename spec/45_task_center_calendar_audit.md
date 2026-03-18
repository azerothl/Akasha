# Task Center et Calendrier — audit spec 36

Audit des onglets Tâches et Calendrier par rapport à [36_ui_architecture.md](36_ui_architecture.md).

## Task Center (Tâches)

| Élément spec 36 | Statut |
|-----------------|--------|
| Liste : tâches en cours, terminées, échouées | ✅ Filtres actifs / terminées, recherche |
| Détail par tâche : graphe sous-tâches, timeline, logs, artefacts | ✅ Événements (timeline), détail dans modale Calendrier pour une tâche |
| Actions : relance, pause, reprendre, annuler | ✅ **Annuler** (bouton + /stop, /cancel) ; **Relancer** (bouton → Chat avec message) ; **Pause** (POST /api/tasks/:id/pause) ; **Reprendre** (POST /api/tasks/:id/resume) ; tâches **interrompues** (après redémarrage daemon) : GET /api/tasks?status=interrupted, action Relancer = resume |
| Mise à jour temps réel | ✅ SSE + refresh liste |

## Calendrier

| Élément spec 36 | Statut |
|-----------------|--------|
| Vue : récurrences + prochaines occurrences (mois / semaine / jour) | ✅ Grille jour/semaine/mois, onglet Récent, onglet Récurrences |
| CRUD récurrences (edit, supprimer, pause) | ✅ Détail récurrence : nom, état (activée/pause), prompt, rrule ; /schedule delete ; enregistrer prompt |
| Exceptions (skip une occurrence) | ✅ Backend : GET/POST/DELETE `/api/schedules/:id/exceptions` ; scheduler applique les exceptions ; UI affichage/édition à brancher |
| Lien runs historiques (task_run, task_id) | ✅ Clic sur événement → détail tâche (run + statut) |

## Références

- [36_ui_architecture.md](36_ui_architecture.md)
- Plan d’avancement étape 10.
