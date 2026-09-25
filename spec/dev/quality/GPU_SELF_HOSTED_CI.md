# Checklist — runner GPU self-hosted (v0.10)

Objectif : smoke HTTP daemon CUDA + bench tok/s en CI (non disponible sur GitHub-hosted sans `nvcuda.dll`).

## Prérequis machine

- Windows ou Linux avec GPU NVIDIA + pilotes récents
- Label GitHub Actions suggéré : `self-hosted`, `gpu`, `nvidia`
- Binaire `akasha-daemon` build CUDA + GGUF téléchargé dans le runner

## Mise en service

1. Installer [GitHub Actions runner](https://docs.github.com/actions/hosting-your-own-runners) avec labels ci-dessus.
2. Copier artefact `akasha-windows-x86_64-cuda.zip` ou build local CUDA.
3. `akasha config models embedded-download`
4. Workflow livré : [`.github/workflows/gpu-ci.yml`](../../../.github/workflows/gpu-ci.yml) (job `gpu-smoke`, déclenché sur `main` / PR ciblées).

## Statut v0.10

- [ ] Runner provisionné (opérateur — labels `self-hosted`, `gpu`, `nvidia`)
- [x] Job CI activé dans `.github/workflows/gpu-ci.yml`
- [x] Baselines locales documentées dans `bench_embedded_results.md` (2026-06-22, RTX 3050 Ti)
- [ ] Dernière exécution bench **runner self-hosted** collée dans `bench_embedded_results.md`
- [ ] `embedded_profile_baselines.json` mis à jour depuis le runner GPU
