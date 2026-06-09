# Notes internes — release 0.9.0 (depuis v0.8.0)

Document à l’usage des **contributeurs** et de l’équipe release. Complète [tests_and_benchmarks.md](../quality/tests_and_benchmarks.md), [AGENTS.md](../../../AGENTS.md) et l’index [README.md](README.md). Le guide utilisateur embarqué est désormais **multi-pages** sous `docs/user/` (plus le monolithe `user_guide_final.md` pour les auteurs).

---

## Périmètre

Référence git : état du dépôt étiqueté **v0.8.0** → **0.9.0** (`[workspace.package].version`). Thèmes majeurs : **workspace UI** (Cookbook, Comparer, Recherche, Notes), **catalogue Cookbook** (12 recettes JSON + API), **documentation embarquée multi-pages**, renforts release engineering (staging `docs/user/`, smoke API index).

---

## Workspace UI (desktop)

- **Navigation sidebar** : groupe Workspace avec onglets Comparer, Recherche approfondie, Cookbook, Notes (raccourcis clavier 2–5 selon [38_interfaces.md](../../38_interfaces.md)).
- **Cookbook** :
  - Sous-vues **Modèles** (pull HF / Ollama / BitNet, filtres, pricing lookup) et **Recettes** (12 workflows guidés).
  - API daemon : `GET /api/cookbook/recipes`, `GET /api/cookbook/recipes/:id` ; registre embarqué via `cookbook_recipes.rs` + `spec/cookbook/`.
  - Actions recettes : ouvrir Comparer, essayer dans le chat, liens modèles.
- **Comparer** : panel blind A/B avec préremplissage depuis Cookbook (`ComparePanel.tsx`).
- **Recherche approfondie** : workflow RAG / deep research (panel dédié).
- **Notes** : panel notes utilisateur (API notes daemon).

---

## Documentation embarquée multi-pages

- **Sources auteur** : `docs/user/*.md` + `docs/user/index.json` ; génération depuis le monolithe via `scripts/build-user-docs.py`.
- **API HTTP** :
  - `GET /api/docs` → `{ pages, default }`
  - `GET /api/docs/:page_id` → `{ id, title, content }`
  - `GET /api/docs?legacy=1` → `{ content }` (compat TUI / clients anciens)
- **Implémentation** : `crates/akasha-daemon/src/user_docs.rs`, routage dans `api.rs`.
- **UI desktop** : sidebar navigation Doc (`App.tsx`), commandes Tauri `get_docs_index` / `get_docs_page`.
- **TUI** : pagination Doc avec `[` / `]` entre pages ; titre de bloc affiche page courante.
- **Release packaging** : copie `docs/user/` dans l’archive (`release.yml`) ; smoke staging vérifie index + page `accueil`.

---

## Release engineering

- **Version** : `scripts/sync-release-version.py 0.9.0` (5 fichiers produit Akasha).
- **Smoke staging** : `docs/user/index.json` requis ; `GET /api/docs` doit retourner `"pages"`.
- **E2E API** : `e2e_api_smoke.rs` teste index + page par défaut.
- **Playwright** : captures étendues (`ui-cookbook-*.png`, `ui-compare.png`, `ui-research.png`, `ui-mission.png`, `ui-notes.png`, `ui-docs.png` avec sidebar doc).

---

## Akasha Code Studio (dépôt séparé)

- Package npm **`akasha-code-studio@0.9.0`** : bin `akasha-code-studio`, serveur statique `dist/` + proxy `/api` → daemon 3876.
- Pas de dépendance runtime dans le daemon ; mention dans notes release et `docs/user/extensions.md`.

---

## Site marketing Akasha_app

- Pastilles **v0.9.0** ; section What's new 0.9 (Cookbook, workspace, doc multi-pages).
- **Plugins** : `plugins-page.js` — GitHub raw PRIMARY (jsDelivr fallback) pour éviter catalogue stale (5 plugins).
- **Captures** : copie `docs/screenshots/*.png` → `assets/screenshots/` ; galerie `docs.html` / `index.html`.

---

## Fichiers et crates utiles

| Sujet | Emplacements |
|--------|----------------|
| Cookbook API | `cookbook_recipes.rs`, `api_routes_workspace.rs`, `spec/cookbook/` |
| Doc multi-pages | `user_docs.rs`, `docs/user/`, `scripts/build-user-docs.py` |
| UI workspace | `App.tsx`, `CookbookPanel.tsx`, `ComparePanel.tsx`, `CookbookRecipesView.tsx` |
| Validation recettes | `scripts/validate-cookbook-recipes.py`, CI |

---

## Maintenance doc

- **Binaires** : archive contient `docs/user/` + `docs/user_guide.md` (legacy fallback).
- **Dépôt source** : `spec/user_guide.md` reste pour contributeurs ; contenu utilisateur canonical = `docs/user/`.
- **Regénération** : `python scripts/build-user-docs.py` après édition structurelle du guide.

---

## Historique 0.8.0 (rappel)

Mission autonome, graphes projet multi-workspaces, formalité agent, permission center, budget — voir [internal_release_0.8.md](internal_release_0.8.md).
