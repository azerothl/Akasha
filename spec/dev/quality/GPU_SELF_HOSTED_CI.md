# Checklist — runner GPU self-hosted (v0.10)

Objectif : valider une vraie génération CUDA en CI (non disponible sur GitHub-hosted sans `nvcuda.dll`).

## Prérequis machine

- Windows ou Linux avec GPU NVIDIA + pilotes récents
- Label GitHub Actions suggéré : `self-hosted`, `gpu`, `nvidia`
- Binaire `akasha-daemon` build CUDA + GGUF téléchargé dans le runner

## Mise en service

1. Installer [GitHub Actions runner](https://docs.github.com/actions/hosting-your-own-runners) avec labels ci-dessus.
2. Copier artefact `akasha-windows-x86_64-cuda.zip` ou build local CUDA.
3. `akasha config models embedded-download`
4. Workflow livré : [`.github/workflows/gpu-ci.yml`](../../../.github/workflows/gpu-ci.yml), job `gpu-smoke` sur `main` / PR ciblées.
5. Le job démarre explicitement le daemon avec `AKASHA_EMBEDDED_BACKEND=llama_cpp` et `AKASHA_EMBEDDED_N_GPU_LAYERS=99`, vérifie `engine_mode=cuda`, puis exécute le protocole live de cinq prompts.
6. Le job échoue si une réponse est vide, si le provider ou le modèle actif diffère, ou si les métriques de tokens, chargement, TTFT et génération manquent.
7. Il conserve le JSON, les logs daemon et les échantillons `nvidia-smi` en artefact Actions 30 jours. Le JSON contient le pilote, la version du toolkit, le modèle/hash, les métriques d'inférence et le résumé VRAM/utilisation.
8. La matrice multi-modèle reste disponible en déclenchement manuel (`run_model_matrix=true`) ; elle ne remplace pas le gate réel sur le modèle par défaut.

## Statut v0.10

- [ ] Runner provisionné (opérateur — labels `self-hosted`, `gpu`, `nvidia`)
- [x] Job CI activé dans `.github/workflows/gpu-ci.yml`
- [x] Baselines locales documentées dans `bench_embedded_results.md` (2026-06-22, RTX 3050 Ti)
- [ ] Dernière exécution bench **runner self-hosted** collée dans `bench_embedded_results.md`
- [ ] `embedded_profile_baselines.json` mis à jour depuis le runner GPU

## Statut v0.11 — 20 octobre 2026

- Le protocole `bench_ar_protocol.py --live` vérifie directement la complétion et les mesures du moteur ; le health check seul n'est pas accepté comme inférence.
- Une machine locale RTX 4080 SUPER (16 376 MiB) a exécuté le daemon Windows CUDA avec `llama_cpp`, 99 couches GPU et le modèle Qwen2.5 1.5B Q4. Les cinq prompts fixes ont répondu via `akasha_embedded`; débits mesurés de 164 à 210 tok/s, premier chargement à 1,137 s, échantillons GPU de 2 061 à 3 661 MiB et pic d'utilisation de 53 %. Le JSON détaille l'OS, le pilote, CUDA, le hash du modèle et les mesures. Cette exécution locale a utilisé un worktree modifié ; la révision exacte des sources n'est donc pas établie dans le JSON historique.
- Le run GitHub Actions de la PR #165 reste en file d'attente sur `self-hosted,gpu,nvidia`. Le nouveau job exécute désormais le même protocole et conserve les preuves ; il n'est pas encore passé sur le runner du dépôt.
- La référence publiée Qwen2.5 sur RTX 3050 Ti indique 7,3 tok/s, mais le matériel et le protocole diffèrent. Ces deux valeurs ne permettent pas de revendiquer un gain comparatif.
