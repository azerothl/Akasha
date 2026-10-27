# Exécuteur NVIDIA pour la CI de main

## Installation Windows

Le lanceur utilise Windows x64, NVIDIA avec toolkit CUDA, Visual Studio 2022
C++, LLVM, CMake, Python 3.11+, PowerShell 7 et GitHub CLI connecté avec les
droits d'administration du dépôt. Prévoir 35 Gio libres sur le disque du runner.

```powershell
pwsh -NoProfile -ExecutionPolicy Bypass -File scripts/ci/start-nvidia-runner.ps1 -Root C:/AkashaCI -InstallStartup
```

Le paquet officiel Actions Runner 2.338.2 est vérifié par SHA-256. Un
gestionnaire local inscrit un exécuteur à usage unique avec l'étiquette
`akasha-nvidia-main`, puis le renouvelle après chaque job. Le gestionnaire
redémarre à l'ouverture de session grâce au raccourci utilisateur
`Akasha NVIDIA CI`. Il n'installe pas de service administrateur.

Les caches, outils, profils et journaux restent dans `C:/AkashaCI`. Le
worker reçoit uniquement les variables système nécessaires et utilise des
répertoires CI pour ses configurations. Le jeton GitHub d'administration
n'est pas transmis au worker. Le dossier est accessible au compte courant,
à SYSTEM et aux administrateurs. Les journaux locaux du runner sont conservés
dans le dossier d'application et `logs/`.

Avant le checkout, le hook installé hors de l'application vérifie le dépôt,
`refs/heads/main`, l'événement push/manuel et l'empreinte courante de `main`
relue sur GitHub. Une PR, une autre branche ou un ancien commit est refusé
avant toute étape du job. Ce contrôle ne constitue pas une machine virtuelle :
les processus tournent sous le compte Windows courant et n'exécutent que le
code accepté sur `main`. Ne pas ajouter les étiquettes génériques `gpu,nvidia` :
les anciennes exécutions qui les demandent ne doivent pas atteindre ce runner.

Le workflow utilise un profil neuf et un port loopback libre. Il vérifie que
le processus qui écoute est le daemon qu'il vient de lancer. L'application
personnelle sur 3876 reste indépendante. Le matériel doit rester allumé et la
session ouverte pour que le gestionnaire soit disponible ; un ordinateur
éteint laisse les tâches en attente sur GitHub.

Déclenchement manuel : `gh workflow run gpu-ci.yml --ref main`. Le résultat
exige cinq générations réelles CUDA et conserve les réponses, empreintes,
mesures de tokens, latence et échantillons NVIDIA dans les artefacts du run.
L'option `run_model_matrix` utilise `bench_gpu_matrix.py` : les modèles du
catalogue sont téléchargés et vérifiés dans le profil CI, puis mesurés avec
0 et 99 couches GPU via des daemons distincts. Une erreur de téléchargement
ou une génération manquante fait échouer la matrice ; aucun modèle n'est
recherché dans le profil personnel.

Références : [exécuteurs éphémères](https://docs.github.com/en/actions/reference/runners/self-hosted-runners#ephemeral-runners-for-autoscaling),
[hooks avant les jobs](https://docs.github.com/en/actions/how-tos/manage-runners/self-hosted-runners/run-scripts).

## Historique v0.10

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
- Une machine locale RTX 4080 SUPER (16 376 MiB) a exécuté le daemon Windows CUDA depuis le commit propre `1a82a6b`, avec `llama_cpp`, 99 couches GPU et Qwen2.5 1.5B Q4. Les cinq prompts fixes ont répondu via `akasha_embedded`, à 146–218 tok/s ; le premier chargement a pris 1,166 s. Le JSON inclut le hash du daemon et du modèle, le pilote 610.88, CUDA 13.3, TTFT, génération et tokens réels. Les échantillons VRAM sont globaux au GPU (2 071–12 057 MiB) et comprennent d'autres processus ; ils ne sont pas attribués à Akasha.
- Le rapport et ses échantillons sont conservés dans [`bench_ar_qwen25_cuda_20261020.json`](bench_ar_qwen25_cuda_20261020.json), [`bench_ar_qwen25_cuda_20261020.log`](bench_ar_qwen25_cuda_20261020.log) et [`bench_ar_qwen25_cuda_20261020_gpu_samples.csv`](bench_ar_qwen25_cuda_20261020_gpu_samples.csv). La baseline RTX 3050 Ti à 7,3 tok/s est décrite dans `bench_embedded_results.md`, mais le protocole et le matériel diffèrent : aucun gain relatif n'est revendiqué.
- Le run GitHub Actions de la PR #165 reste en file d'attente sur `self-hosted,gpu,nvidia`. Le nouveau job exécute désormais le même protocole et conserve les preuves ; il n'est pas encore passé sur le runner du dépôt.
- La référence publiée Qwen2.5 sur RTX 3050 Ti indique 7,3 tok/s, mais le matériel et le protocole diffèrent. Ces deux valeurs ne permettent pas de revendiquer un gain comparatif.
