# Validation premier démarrage — artefacts release v0.11.0

Document de test d'installation **fraîche** (hors build CI du workspace). Complète l'issue [#141](https://github.com/azerothl/Akasha/issues/141), `spec/distribution.md` et `docs/user/installation.md`.

**Dernière mise à jour** : 2026-10-18 (agent cloud, Ubuntu x86_64).

---

## Matrice

| Artefact | Version | Arch | OS de test | Init | Daemon | `doctor` | Modèle embarqué (chemin / backend) | Premier message | UI desktop | Statut |
|----------|---------|------|------------|------|--------|----------|-------------------------------------|-----------------|------------|--------|
| `akasha-linux-x86_64.zip` | v0.11.0 | x86_64 | Linux (Ubuntu, VM cloud, **sans GPU**) | OK (~1 s) | OK (~1 s jusqu'à `/api/status`) | Partiel (voir notes) | Candle CPU, `akasha_embedded` dans `llm_router.yaml` | OK (~101 s, 101 tokens out) | Non testé | **Validé CPU Linux** |
| `akasha-full-linux-x86_64.zip` | v0.11.0 | x86_64 | — | — | — | — | — | — | — | **Non testé** |
| `akasha-ui-linux.zip` | v0.11.0 | x86_64 | — | — | — | — | — | — | — | **Non testé** |
| `akasha-macos-*.zip` / `akasha-full-macos-*.zip` | v0.11.0 | x86_64 / aarch64 | — | — | — | — | — | — | — | **Non testé** |
| `akasha-windows-x86_64.zip` / full | v0.11.0 | x86_64 | — | — | — | — | — | — | — | **Non testé** |
| `akasha-windows-x86_64-cuda.zip` / full CUDA | v0.11.0 | x86_64 + NVIDIA | — | — | — | — | — | — | — | **Non testé** (smoke GPU : workflow `gpu-smoke` séparé) |

Les cibles **non testées** ne doivent pas être présentées comme validées pour le premier démarrage.

---

## Procédure reproductible (Linux CPU)

Script : `scripts/smoke-release-artifact-linux.sh`

```bash
# Depuis la racine du dépôt (gh CLI requis pour le téléchargement)
RELEASE_TAG=v0.11.0 bash scripts/smoke-release-artifact-linux.sh
```

Variables utiles : `ARTIFACT_ZIP` (zip local), `SKIP_INFERENCE=1` (sans appel LLM), `INFERENCE_TIMEOUT_SECS` (défaut 240).

**CI** : job `release-artifact-first-boot-linux` dans `.github/workflows/ci.yml` (même script, tag `v0.11.0` par défaut).

---

## Résultat manuel — `akasha-linux-x86_64.zip` (2026-10-18)

Environnement : `HOME` temporaire vide, pas de `AKASHA_DATA_DIR` préexistant, extraction dans `/tmp/akasha-firstboot-test`.

| Étape | Commande | Résultat |
|-------|----------|----------|
| Version | `./akasha --version` | `akasha 0.11.0` |
| Init | `./akasha init --defaults` | data_dir `~/akasha`, `llm_router.yaml` avec `akasha_embedded` — **~0,9 s** |
| Daemon | `./akasha-daemon` (port éphémère) | `/api/status` OK — **~1 s** |
| Doctor | `./akasha doctor` | Santé daemon OK, `embedded_llm` OK (Candle, premier appel lent). **Attendu KO** : Ollama, Rbitnet. **Avertissement** : fichiers spec `09_event_model.yaml` / `10_data_model.yaml` absents du zip CLI (check local au répertoire d'extraction). |
| Message | `POST /api/message` + poll `GET /api/tasks/:id` | `status=completed`, `last_turn_model_used=embedded`, **~101 s** |

Extrait init :

```
Répertoire de données : /tmp/.../akasha
Mode --defaults : configuration minimale (Ollama uniquement).
  Fichier écrit : .../llm_router.yaml
```

Extrait doctor (daemon actif) :

```
  [OK] embedded_llm — Embedded model will load on first use (Candle CPU: first call may take 1–3 min)
  [KO] ollama — Ollama unreachable
```

---

## Bugs / écarts (non bloquants pour ce smoke)

Aucun **bloquant** identifié pour le parcours Linux CPU CLI : init, daemon, inférence embarquée et première réponse aboutissent.

Écarts documentés (pas corrigés dans la PR #141) :

1. **`akasha doctor` exit code ≠ 0** alors que le parcours embarqué est utilisable (Ollama / Rbitnet optionnels, spec YAML incomplets dans le zip CLI).
2. **Message d'init `--defaults`** indique « Ollama uniquement » alors que `llm_router.yaml` généré pointe sur `akasha_embedded` pour les tâches conversation — confusion doc/texte uniquement.

---

## Références

- Issue : [#141 — Valider le premier démarrage des artefacts publiés](https://github.com/azerothl/Akasha/issues/141)
- Smoke staging (build release, pas artefact GitHub) : `scripts/smoke-release-staging.sh`
