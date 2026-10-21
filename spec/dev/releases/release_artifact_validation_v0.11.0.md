# Validation premier démarrage — artefacts release v0.11.0

Document de test d'installation **fraîche** (hors build CI du workspace). Complète l'issue [#141](https://github.com/azerothl/Akasha/issues/141), `spec/distribution.md` et `docs/user/installation.md`.

**Dernière mise à jour** : 2026-10-21. Les tests manuels publics ci-dessous sont distincts du build de candidate `publish=false`.

---

## Matrice des artefacts publics

| Artefact | Version | Architecture | OS essayé | Init | Daemon | `doctor` | Modèle / premier message | UI desktop | Statut d'acceptation |
|---|---|---|---|---|---|---|---|---|---|
| `akasha-linux-x86_64.zip` | v0.11.0 | x86_64 | Ubuntu, VM cloud sans GPU (18 oct.) | OK (~0,9 s) | OK (~1 s) | Partiel : les specs YAML manquent | Candle CPU ; réponse historique réussie en ~101 s | non testée | **Non validé** : re-smoke GitHub #39585465251 bloqué par les specs manquantes |
| `akasha-full-linux-x86_64.zip` | v0.11.0 | x86_64 | non testé | — | — | — | — | — | Non testé |
| `akasha-windows-x86_64.zip` | v0.11.0 | x86_64 | Windows 11 Pro Insider Preview 10.0.26220, 21 oct. | OK | OK, `/api/status` renvoie `status=ok` sur port isolé | **Échec** : `spec/09_event_model.yaml` et `spec/10_data_model.yaml` absents | Configuration `ollama/tinyllama`; premier message non testé | sans objet | **Non validé** |
| `akasha-full-windows-x86_64.zip` | v0.11.0 | x86_64 | non testé | — | — | — | — | — | Non testé |
| `akasha-macos-x86_64.zip`, `akasha-full-macos-x86_64.zip` | v0.11.0 | x86_64 | non testé | — | — | — | — | — | Non testé |
| `akasha-macos-aarch64.zip`, `akasha-full-macos-aarch64.zip` | v0.11.0 | aarch64 | non testé | — | — | — | — | — | Non testé |
| `akasha-ui-linux.zip` | v0.11.0 | x86_64 | non testé | — | — | — | sans objet | non testée | Non testé |
| `akasha-ui-macos.zip` | v0.11.0 | x86_64 / aarch64 | non testé | — | — | — | sans objet | non testée | Non testé |
| `akasha-ui-windows.zip` | v0.11.0 | x86_64 | Windows 11 Pro Insider Preview 10.0.26220, 21 oct. | sans objet | — | — | sans objet | Archive MSI/NSIS inventoriée et MSI extrait en temporaire ; application non lancée | Non testé |
| `akasha-windows-x86_64-cuda.zip` / `akasha-full-windows-x86_64-cuda.zip` | v0.11.0 | x86_64 + NVIDIA | Windows 11 Pro Insider Preview 10.0.26220, RTX 4080 SUPER, 21 oct. | OK | OK | **Échec** : mêmes specs absentes | Inférence CUDA réelle Qwen2.5 1.5B ; réponse exacte `OK` en ~20 s | sans objet | Inférence validée, installation non validée ; validation CUDA suivie séparément par #142 |

Aucune cible non testée n'est comptée comme validée. Une réponse réelle ne compense pas l'échec du diagnostic requis.

---

## Procédure reproductible (Linux CPU)

Script : `scripts/smoke-release-artifact-linux.sh`

```bash
RELEASE_TAG=v0.11.0 bash scripts/smoke-release-artifact-linux.sh
```

`ARTIFACT_ZIP` sélectionne une archive locale, `SKIP_INFERENCE=1` saute l'appel LLM et `INFERENCE_TIMEOUT_SECS` règle le délai (240 s par défaut). Le job correspondant est `release-artifact-first-boot-linux` dans `.github/workflows/ci.yml`.

---

## Détails des constats

### Linux x86_64 — archive publique v0.11.0

L'essai manuel du 18 octobre a démarré le daemon et obtenu une réponse Candle CPU en environ 101 s. `doctor` était seulement partiel : il signalait l'absence de `09_event_model.yaml` et `10_data_model.yaml`. Le 21 octobre, le smoke GitHub du zip public (#39585465251) a échoué avant `init` sur ces deux fichiers. La réponse historique reste une observation valide, mais le parcours complet ne satisfait pas le critère `doctor`.

### Windows x86_64 — archive publique v0.11.0

Archive identifiée par SHA-256 `EF49BA83085851E1DA8E2889B249964993EEF64C6DA19B639FED1D3B8535998D`. Depuis un répertoire de données neuf, `akasha init --defaults` réussit. Le daemon de l'archive démarre sur un port HTTP isolé et `/api/status` répond `{"status":"ok"}`. `doctor` signale les deux specs manquantes. Le binaire v0.11.0 retourne malgré cela le code processus 0 ; le texte de diagnostic dit bien que certains contrôles ont échoué. Le premier message réel n'a pas été essayé sur cet artefact.

Le rapport expurgé de ce contrôle est [`release_first_boot_windows_cpu_v0.11.0_20261021.json`](../quality/release_first_boot_windows_cpu_v0.11.0_20261021.json). Le commentaire de suivi est [lié à #141](https://github.com/azerothl/Akasha/issues/141#issuecomment-6315016258).

### Candidate de release non publiée

Le run manuel [39590849411](https://github.com/azerothl/Akasha/actions/runs/39590849411), lancé avec `publish=false` sur le code ensuite fusionné dans `main`, teste la chaîne de build et le staging corrigé ; il ne modifie pas les archives publiques de v0.11.0.

L'artefact candidat `akasha-windows-x86_64` a été téléchargé depuis ce run et vérifié dans un répertoire de données temporaire neuf sur Windows 11 Pro Insider Preview 10.0.26220 : les deux YAML sont présents ; `akasha init --defaults` réussit ; le daemon démarre sur un port isolé et `/api/status` renvoie `ok` ; `akasha doctor` affiche `All checks passed.` et retourne 0. Cela confirme le packaging Windows CPU candidat. La première réponse modèle n'a pas encore été testée sur cette candidate ; les artefacts UI ne sont pas lancés dans cette validation.

Cette candidate n'est pas une release publique. Elle ne suffit pas pour fermer #141 : chaque archive publique CPU doit passer `doctor` et produire une première réponse réelle ; les bundles desktop disponibles doivent être lancés.
---

## Écarts et statut

Les archives publiques Linux et Windows testées ne contiennent pas `spec/09_event_model.yaml` et `spec/10_data_model.yaml`. Le diagnostic actuel le détecte ; le vieux binaire Windows v0.11.0 affiche l'échec mais conserve un code retour 0. Les contrôles du staging de la pipeline actuelle vérifient les fichiers de l'artefact construit ; ils ne modifient pas les assets déjà publiés.

Le smoke `publish=false` ne crée pas de release publique. #141 reste donc ouvert jusqu'à la publication et à la validation d'une nouvelle release complète avec sa matrice CPU/desktop. Les cibles CUDA sont documentées séparément et ne remplacent pas les contrôles CPU.

## Références

- [Issue #141 — Valider le premier démarrage des artefacts publiés](https://github.com/azerothl/Akasha/issues/141)
- [`spec/distribution.md`](../../distribution.md)
- [`docs/user/installation.md`](../../../docs/user/installation.md)
- Staging release : `scripts/smoke-release-staging.sh`