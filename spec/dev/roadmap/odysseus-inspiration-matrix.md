> **Archive:** Ce document est archivé. Source de vérité active : [`ROADMAP_FINAL_REGISTRY.md`](./ROADMAP_FINAL_REGISTRY.md).

# Matrice d'inspiration — Odysseus ↔ Akasha

**Version:** 1.0.0  
**Date:** 2026-06-03  
**Référence amont:** [pewdiepie-archdaemon/odysseus](https://github.com/pewdiepie-archdaemon/odysseus)

Document de suivi des idées empruntées à Odysseus sans diluer le positionnement daemon/opérateur d'Akasha. Complète [`reference-products-parity-matrix.md`](./reference-products-parity-matrix.md) (Hermes, OpenClaw, etc.).

---

## Légende

| État Akasha | Signification |
|-------------|---------------|
| **Existe · haute** | Livré, UX acceptable |
| **Partiel · moyenne** | Backend ou UX incomplet |
| **Plan · phase X** | Décision prise, implémentation en cours ou planifiée |
| **Absent · évalué** | Hors scope court terme ; doc d'évaluation si pertinent |
| **Absent** | Non retenu |

| Phase plan | Contenu |
|------------|---------|
| **A** | Fondations UI (navigation chat-first, tokens, permissions surface) |
| **B** | Patterns workspace (compare, utility model, presets) |
| **C** | Capacités lourdes (cookbook-lite, deep research, notes) |
| **D** | Hébergement équipe (auth multi-user) |

---

## 1. Synthèse positionnement

| Axe | Odysseus | Akasha |
|-----|----------|--------|
| Promesse | Workspace IA type ChatGPT self-hosted | Daemon opérateur 24/7, agents, policy, MCP |
| Stack UI | SPA vanilla + FastAPI | Tauri + React (`apps/akasha-ui`) |
| Modèles locaux | Cookbook (llmfit) intégré | `llm_router.yaml`, Rbitnet, embarqué |
| Sécurité | Auth, 2FA, privilèges par outil | Poste local ; file permissions UI |

**Principe:** emprunter les **patterns UX** et quelques **surfaces produit** ; ne pas reproduire le monolithe Python ni la suite bureautique complète.

---

## 2. Matrice fonctionnelle

| Domaine | Odysseus | Akasha (avant) | État Akasha | Phase | PR / fichiers cibles |
|---------|----------|----------------|-------------|-------|----------------------|
| Chat multi-sessions | Sessions, dossiers, recherche | Threads `localStorage`, SSE | Partiel · moyenne ; colonne centrée + `ChatRenderer` | A | `App.tsx`, `ChatRenderer.tsx` |
| Agent + outils | Toggle shell/web/MCP | Policy YAML, MCP runtime | Existe · haute ; barre `ChatCompositionBar` | A | `ChatCompositionBar.tsx` |
| Compare modèles | Blind test, synthèse | Rôle agent `compare` sans UI | **Existe · haute** (2026-06-03) | B | `api_routes_workspace.rs`, `ComparePanel.tsx` |
| Deep Research | Rapport visuel multi-sources | `web_crawl` partiel | **Existe · haute** (IterResearch, web_search, sous-agents, rapport riche) | C | `deep_research/`, `POST /api/research/deep/start`, `DeepResearchPanel.tsx` |
| Cookbook | VRAM scan, download, serve | Rbitnet séparé | **Existe · moyenne** (cookbook-lite RAM/GPU hints) | C | `GET /api/cookbook/*`, `CookbookPanel.tsx` |
| Modèle utilitaire | Compaction, titres, mémoire fichier | `task_type: system` | **Existe · haute** (`task_types.utility`) | B | `llm_router.example.yaml`, `api.rs` |
| Presets / personas | Température, system prompt | Profils agent | Partiel · moyenne | B | Settings agent |
| Mémoire vectorielle | ChromaDB + keyword | LTM SQLite + graphe | Existe · haute | — | — |
| Documents / notes | Éditeur multi-onglets | Code Studio (dev) | Partiel · moyenne | C | Notes légères (roadmap) |
| Email IMAP | Triage IA | Absent | **Absent · évalué** | — | [`caldav-email-evaluation.md`](../integrations/caldav-email-evaluation.md) |
| CalDAV | Sync calendriers externes | Calendrier interne seulement | **Absent · évalué** | — | idem |
| Thèmes éditables | Éditeur couleurs + effets | 5 thèmes fixes | Partiel · moyenne ; tokens densité | A+ | `styles.css` |
| PWA / mobile | Responsive, SW | Tauri desktop | Absent | D+ | — |
| Auth multi-user | Comptes, 2FA | Single-user local | Absent | D | — |
| Permissions outils | Admin-gated | File queue API | **Existe · haute** (`PermissionsBell`) | A | `PermissionsBell.tsx` |
| Setup in-app | `/setup` wizard | `akasha init`, `doctor`, wizard Tauri 4 étapes | Partiel · moyenne | B | `OnboardingWizard.tsx` (doctor --fix, profil) ; pas parité `/setup` Odysseus |
| Incognito | Sans mémoire/historique | — | **Existe · haute** (wave 7) | — | `incognito` / `no_memory` on `POST /api/message` ; bannière Tauri `App.tsx` |
| Floutage secrets | Masque tokens en sortie | — | **Existe · haute** (wave 7) | — | `blurSecrets` in `ChatRenderer.tsx` |

---

## 3. Frontend — patterns Odysseus → Akasha

| Pattern Odysseus | Implémentation Akasha | Phase |
|------------------|----------------------|-------|
| Navigation chat-first | Groupes sidebar (`AppNavigation.tsx`) | A · livré |
| Colonne `--chat-max` centrée | Tokens CSS + `.chat-messages-column` | A · livré |
| Settings overlay | Drawer settings (conservation onglet legacy) | A/P1 · partiel |
| Sidebar repliable + densité | `data-density`, `--sidebar-width` | A · livré |
| Permissions badge global | `PermissionsBell` dans header | A · livré |
| Module `chatRenderer` | `ChatRenderer.tsx` | A · livré |
| Hash routing deep-links | `useHashRoute.ts` | A · livré |
| Raccourcis 1–9 | Mise à jour `spec/38_interfaces.md` | A · livré |

---

## 4. Ce qu'Akasha préserve (ne pas « devenir Odysseus »)

- Daemon Rust, `tools_policy.yaml`, sandbox, plugins WASM
- Task Center (graphe, timeline, swarm Code Studio)
- Canaux Slack/Discord/Telegram/Teams, webhooks
- Mission autonome, migration OpenClaw
- TUI + specs FR

---

## Changelog

- **1.0.0** (2026-06-03): création initiale ; liens implémentation Phase A–C.
- **1.0.1** (2026-06-03): états mis à jour après livraison Phase A/B/C (UI modulaire, routes workspace, évaluation CalDAV/email).
- **1.0.2** (2026-06-03): Deep Research IterResearch (moteur daemon, poll UI, viz sous-agents, export HTML enrichi).
- **1.0.3** (2026-06-06): resync setup in-app — wizard Tauri `OnboardingWizard.tsx` (4 étapes) documenté ; reste Partiel vs `/setup` Odysseus.
- **1.0.4** (2026-06-06): incognito + floutage secrets → **Existe · haute** (wave 7) — `App.tsx` incognito banner, `ChatRenderer.tsx` `blurSecrets`, `POST /api/message` flags.
