# Plan d'avancement — Phases non développées

Suivi des étapes du plan de mise en place (phases non développées). Voir [40_plan_rattrapage_et_polish.md](40_plan_rattrapage_et_polish.md) et [41_phase9_polish_checklist.md](41_phase9_polish_checklist.md). Cocher chaque étape lorsque **tous** les éléments de celle-ci ont été développés.

---

## Priorité haute

- [x] **Étape 1 — Scheduler : RRULE / cron (spec 37)**  
  Calcul des créneaux dus à partir de `rrule` (iCal) ou expression cron ; sinon conserver `interval_seconds`. Parser RRULE/cron en Rust ; dans `scheduler.rs`, si `schedule.rrule` non vide, calculer `due_slots` via ce parser entre `start_at` et `end_at`. Livrable : récurrences RRULE/cron exécutées au bon moment.

- [x] **Étape 2 — Phase 9 : Polish UI (spec 40, 41)**  
  Compléter la checklist 41 : Chat (formatage, indicateur de progression, erreurs claires) ; état daemon visible ; métriques routeur complètes + TUI si besoin ; page/panneau Paramètres (env, port, data_dir) ; thème et feedback visuel ; accessibilité (contrastes, focus, labels). Livrable : UI cohérente, lisible et accessible.

---

## Priorité moyenne

- [x] **Étape 3 — Transport temps réel (spec 36)**  
  Endpoint SSE (ou WebSocket) dans le daemon diffusant tâches, statut, device pending ; UI Tauri abonnée au flux ; polling en fallback. Livrable : mises à jour < 1 s.

- [x] **Étape 4 — Onboarding guidé dans l’UI**  
  Détecter premier lancement ; wizard ou étapes guidées (vault, Ollama, llm_router, doctor) ; lien vers onboarding / user_guide. Livrable : nouvel utilisateur guidé.

- [x] **Étape 5 — Génération d’images (spec 42)**  
  Outil `generate_image` (ou route dédiée) ; appel API images (ex. OpenAI) ; image en data URL dans la réponse ; config + sécurité. Livrable : image générée visible dans le chat.

---

## Priorité basse / optionnel

- [x] **Étape 6 — Modèle petit intégré (spec 34)**  
  Stack Candle ou GGUF ; petit modèle pour doctor/advice et réponses courtes sans LLM externe. Livrable : conseils sans Ollama/cloud.

- [x] **Étape 7 — Outils browser, image, pdf (spec 33)**  
  Remplacer les stubs par implémentations réelles (screenshot, extract, PDF text/metadata) dans api.rs ou akasha-tools. Livrable : outils utilisables par l’agent.

- [x] **Étape 8 — Session terminal (spec 33)**  
  Session terminal interactive (PTY, timeout, isolation) ; exposition via outil ou API ; UI pour afficher/saisir. Livrable : agent peut utiliser une session terminal.

- [x] **Étape 9 — WhatsApp**  
  Adapter WhatsApp (API Business/partenaire) ou stub / doc « hors scope v1 ». Livrable : adapter ou décision documentée.

- [x] **Étape 10 — Task Center et Calendrier (détail)**  
  Auditer UI (Tâches, Calendrier) ; affichage/édition des exceptions de récurrence ; graphe/timeline ; actions relance/pause/annuler. Livrable : Task Center et Calendrier alignés spec 36.

---

## Références

- [Blueprint_implémentation.md](Blueprint_implémentation.md)
- [40_plan_rattrapage_et_polish.md](40_plan_rattrapage_et_polish.md)
- [41_phase9_polish_checklist.md](41_phase9_polish_checklist.md)
- [37_scheduler_design.md](37_scheduler_design.md)
- [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md)
- [36_ui_architecture.md](36_ui_architecture.md)
- [42_image_generation.md](42_image_generation.md)

---

## Notes

- **Étapes 1–5** : réalisées (scheduler RRULE, polish UI, SSE, onboarding, generate_image).
- **Étapes 6–10** : réalisées — stub modèle intégré (spec 34), outils browser/image/pdf (pdf extraction réelle), session terminal (spec 43), WhatsApp hors scope v1 (spec 44), Task Center / Calendrier (spec 45, actions Annuler/Relancer).
