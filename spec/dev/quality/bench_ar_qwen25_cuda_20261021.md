# Validation CUDA locale — candidate v0.12.0

Date : 21 octobre 2026. Smoke d’inférence CUDA exécuté sur l’artefact Windows
`akasha-windows-x86_64-cuda` du run Release `39628627123`, avec le daemon et le
CLI isolés dans un répertoire temporaire.

## Environnement et provenance

- Windows 11 10.0.26220 ; NVIDIA GeForce RTX 4080 SUPER, 16 376 MiB.
- Pilote NVIDIA 610.88 ; toolkit CUDA 13.3.
- Backend `llama_cpp`, `engine_mode=cuda`, `n_gpu_layers=99`.
- Modèle Qwen2.5 1.5B Instruct Q4 ; SHA-256 GGUF
  `6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e`.
- Le binaire vient du commit `9654b76`. Les sources Cargo/Rust/Tauri testées
  n’ont pas changé entre ce build et le commit `8d402ed` qui a exécuté le
  protocole. Les commits suivants de cette PR ne changent que les workflows CI.
- SHA-256 de `akasha-daemon.exe` :
  `669c5f8686f8e612c028fd66f7f730cd391c279aac89ea46afda5c42f4db89f9`.

## Résultat

Le protocole réel a réussi sur les cinq prompts (`greet`, `doctor`, `json`,
`fr_explain`, `code_fib`) : 5/5, aucune erreur, modèle et provider vérifiés.
`nvidia-smi` a fourni 55 échantillons ; mémoire GPU observée entre 2 264 et
3 915 MiB, utilisation maximale 54 %. Le chargement initial mesuré est de
1,161 s ; le premier prompt a répondu en 9,392 s avec un TTFT de 9,044 s.

Les mesures détaillées sont dans
[`bench_ar_qwen25_cuda_20261021.json`](bench_ar_qwen25_cuda_20261021.json) et
[`bench_ar_qwen25_cuda_20261021_gpu_samples.csv`](bench_ar_qwen25_cuda_20261021_gpu_samples.csv).

L’initialisation locale avait détecté Ollama `tinyllama` et l’avait choisi
comme route. Le smoke réussi a utilisé l’exemple de route embarquée. Le
workflow GPU copie désormais explicitement cet exemple après `init --defaults`
afin que la présence d’Ollama sur un runner ne change pas le provider mesuré.

Cette validation porte sur l’inférence CUDA embarquée et ne remplace pas la
campagne de cinq parcours agentiques Ollama suivie par l’issue #143.
