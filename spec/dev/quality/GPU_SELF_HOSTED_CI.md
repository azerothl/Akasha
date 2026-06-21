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
4. Ajouter job workflow (exemple) :

```yaml
gpu-smoke:
  runs-on: [self-hosted, gpu, nvidia]
  steps:
    - uses: actions/checkout@v4
    - name: Smoke HTTP CUDA daemon
      run: |
        ./akasha-daemon.exe &
        sleep 5
        curl -sf http://127.0.0.1:3876/api/status
    - name: Bench tok/s
      run: |
        pwsh ./spec/dev/quality/bench_embedded.ps1 -Backend llama_cpp -Strict -Json
```

5. Documenter l'URL du runner et la fréquence (release tags uniquement recommandé).

## Statut v0.10

- [ ] Runner provisionné
- [ ] Job CI activé dans `.github/workflows/release.yml` ou `ci.yml`
- [ ] Dernière exécution bench collée dans `bench_embedded_results.md`
