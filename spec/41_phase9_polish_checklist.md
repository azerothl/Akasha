# Phase 9 — Polish UI — Checklist de vérification

Ce document liste les points à vérifier ou à compléter pour la Phase 9 (Polish UI) du [plan de rattrapage](40_plan_rattrapage_et_polish.md).

## Chat

- [x] Mise en forme des réponses (markdown, code blocks)
- [x] Indicateur de progression de tâche (chips ou barre)
- [x] Messages d’erreur clairs et actionnables

## État

- [x] Affichage « daemon connecté / déconnecté » dans l’UI
- [ ] (Optionnel) Statut des checks (health)

## Métriques routeur

- [x] Page ou panneau « Router » : requêtes, modèle utilisé, fallbacks, latence
- [x] L’API `GET /api/router/metrics` existe ; à exposer dans un onglet dédié (TUI et Web)

## Paramètres

- [x] Rappel des variables d’environnement ou lien vers la doc
- [x] Formulaire minimal (port, chemin data_dir) si pertinent

## Cohérence visuelle

- [ ] Thème unifié (clair/sombre)
- [ ] Typographie et espacements
- [ ] Feedback visuel : loading, succès, erreur

## Accessibilité

- [ ] Contrastes suffisants
- [ ] Focus clavier et ordre de tabulation
- [ ] Labels sur les champs et boutons

---

À traiter en priorité après les phases 3–8 et le scheduler.
