# Baselines bench embarqué v0.10.0

Résultats mesurés le **2026-06-22** sur poste local.

## Machine

| Composant | Détail |
|-----------|--------|
| Appareil | Dell XPS (`loïcP-fasst-xps`) |
| GPU | NVIDIA GeForce RTX 3050 Ti Laptop GPU, **4 Go** VRAM (+ Intel Iris Xe) |
| CPU | Intel Core **i7-12700H** (12e gen, 14C/20T, 2,3 GHz base) |
| RAM | **32 Go DDR5-4800** dual-channel (2×16 Go Hynix, DIMM A+B) |
| Stockage | 858 / 954 Go utilisés (~90 % — surveiller swap si RAM saturée) |
| Backend bench | llama-cpp-4 + CUDA (release `akasha-daemon`) |
| Protocole | `POST /api/complete` avec route `akasha_embedded` forcée ; `AKASHA_EMBEDDED_PRELOAD=1` ; prompt créatif 50 lignes numérotées, `max_tokens=128` |

**Profil InsiderLLM** : laptop i7 + **DDR5 dual-channel** → bande passante mémoire ~77 GB/s théoriques ; plafond CPU llama.cpp estimé **~11–13 tok/s** (7B Q4), **~30–50+ tok/s** (0,8–3B Q4). GPU 4 Go = **sous le seuil 8 Go** où Insider recommande le CUDA.

## Seuils UX (référence produit)

| Niveau | tok/s (génération) | Ressenti |
|--------|-------------------|----------|
| Fluide | ≥ 15–20 | Streaming lisible en temps réel (cible ChatGPT-like) |
| Acceptable | ≥ 8–10 | Réponse courte OK ; long texte perceptiblement lent |
| Marginal | 5–8 | Utilisable pour `doctor --advice` ; chat long frustrant |
| Insuffisant | < 5 | UX non interactive (CPU Candle, gros contexte agent) |

Gate release Akasha (réf. v0.9) : **> 20 tok/s** sur GTX 3080+ avec Qwen2.5-1.5B Q4 chargé.

## Résultats modèles embarqués Akasha (manifeste v0.10)

| Modèle | Backend | Device | tok/s | Durée gen. (s) | ~tokens | UX |
|--------|---------|--------|-------|----------------|---------|-----|
| **Qwen2.5-1.5B Q4** (défaut prod) | llama_cpp | CUDA | **7,3** | 11,0 | 80 | Marginal |
| **SmolLM2-360M Q4** | — | — | — | — | — | Non testé (download HF échoué) |
| **Qwen3-0.6B** Candle ST | candle | CPU | **0,5** | 387,3 | 198 | Insuffisant |

## Résultats candidats recherche v0.10 (tier 1–2)

| Modèle | Backend | Device | tok/s | Durée gen. (s) | ~tokens | UX |
|--------|---------|--------|-------|----------------|---------|-----|
| **Qwen3.5-0.8B Q4** | llama_cpp | CUDA | **14,9** | 6,7 | 100 | Proche fluide |
| **Gemma 3 1B QAT Q4** | llama_cpp | CUDA | **4,7** | 15,2 | 71 | Insuffisant |
| **Qwen3-1.7B Q4** | llama_cpp | CUDA | **6,7** | 16,3 | 109 | Marginal |
| **Qwen3-0.6B GGUF Q4** | — | — | — | — | — | Non testé (download HF échoué) |

## Diagnostic modèle vs moteur

| Observation | Cause probable |
|-------------|----------------|
| Qwen3-0.6B Candle ~0,5 tok/s | **Moteur + hardware** : inférence CPU Candle, pas GGUF/GPU ; inadapté au chat interactif |
| Qwen2.5 / Gemma / Qwen3-1.7B 5–7 tok/s sur 3050 Ti 4 Go | **Hardware** (GPU mobile 4 Go) + taille modèle ; llama-cpp fonctionne, en dessous du gate 3080 |
| Qwen3.5-0.8B ~15 tok/s (meilleur) | **Modèle** plus récent / architecture hybrid mieux optimisée dans llama.cpp récent ; candidat bench v0.11 prometteur |
| Crash `GGML_ASSERT(n_tokens_all <= cparams.n_batch)` sur Qwen3.5 via pipeline agent | **Moteur** : réglage `n_batch` / contexte llama-cpp insuffisant pour arch `qwen35` ; à corriger avant prod |
| Bench via `/api/message` sans route forcée | **Intégration** : le classifieur route vers OpenRouter/Ollama avant l'embarqué ; ne mesure pas l'embarqué tel quel |

## Commandes

```powershell
# Bench direct (daemon + route akasha_embedded)
.\spec\dev\quality\bench_embedded_models.ps1

# Bench HTTP simple (attention : route utilisateur)
.\spec\dev\quality\bench_embedded.ps1 -Backend llama_cpp -Json

# Bench Rust direct (sans router)
cargo run -p akasha-embedded-llm --features llama-cpp-cuda --example throughput_bench --target x86_64-pc-windows-msvc
```

JSON machine-readable : `bench_embedded_results_run.json`

---

## Croisement InsiderLLM — CPU-only & seuils UX

Référence : [CPU-Only LLMs: What Actually Works](https://insiderllm.com/guides/cpu-only-llms-what-actually-works/) (InsiderLLM, mars 2026).

### Seuils InsiderLLM vs Akasha

| Source | Fluide | Acceptable | Marginal | Insuffisant |
|--------|--------|------------|----------|-------------|
| **Akasha** (bench v0.10) | ≥ 15–20 tok/s | ≥ 8–10 | 5–8 | < 5 |
| **InsiderLLM** | ≥ 10 tok/s (« plus vite que la lecture ») | 8–10 (7B Q4 laptop CPU) | 3–5 (batch OK, chat pénible) | < 2 (« screensaver ») |

Les deux convergent sur **~10 tok/s** comme plancher chat interactif. Akasha est plus exigeant sur le « fluide » (15–20), cohérent avec le gate release **> 20 tok/s** (GTX 3080+).

### Nos mesures vs références communautaires (llama.cpp / Ollama)

| Modèle | Akasha (mesuré) | InsiderLLM (réf. i7 laptop) | Écart / lecture |
|--------|-----------------|----------------------------|-----------------|
| Qwen3.5-0.8B Q4 | **14,9 tok/s** CUDA 3050 Ti 4 Go | **~50+ tok/s** CPU i7 DDR5 | Sous la ref CPU pure — probablement **VRAM 4 Go** + bench E2E daemon + arch `qwen35` hybrid ; reste **au-dessus du seuil 10 tok/s** Insider |
| Qwen2.5-1.5B Q4 | **7,3 tok/s** CUDA | 7B ~8–11 tok/s **CPU** ; 1.5B non listé (attendu 30–50+ CPU) | GPU mobile **≈ CPU 7B** → goulot **hardware**, pas qualité modèle |
| Gemma 3 1B QAT Q4 | **4,7 tok/s** CUDA | Gemma 3 1B cité comme pair BitNet 2B | Sous seuil interactif Insider (3–5 = batch) |
| Qwen3-0.6B Candle | **0,5 tok/s** CPU | 0.8B Qwen3.5 **~50+ tok/s** llama.cpp CPU | **Moteur** : Candle ≠ llama.cpp ; écart **~100×** — le fallback CPU prod doit rester **GGUF llama-cpp**, pas Candle seul |
| SmolLM2-360M | non testé | modèles 2–3B ~30–50 tok/s CPU attendus | À bench ; bon candidat **onboarding léger** si download HF OK |

### Enseignements pour Akasha v0.10 / v0.11

1. **GPU ≥ 8 Go** : InsiderLLM recommande d’utiliser le GPU quand disponible (3–6× vs CPU). Sur **3050 Ti 4 Go**, nos chiffres montrent un gain insuffisant vs attentes — le wizard doit continuer à **calibrer les attentes** (pas « GPU = fluide » sur laptop 4 Go).
2. **CPU fallback** : ne pas promettre chat sur **Candle Qwen3 0.6B** (0,5 tok/s = pire que « screensaver » Insider). Message wizard **1–3 min** validé ; chemin cible = **llama-cpp CPU** (`-ngl 0`) ou BitNet/Rbitnet en veille.
3. **Quant Q4_K_M** : confirmé comme sweet spot CPU et GPU (déjà dans `embedded_models.json` et recherche v0.10).
4. **Qwen3.5-0.8B** : meilleur bench local (14,9 tok/s) + aligné recommandation Insider « petit modèle récent » — **priorité bench v0.11**, après fix `n_batch` pour arch hybrid.
5. **BitNet / Rbitnet** (R0d) : Insider cite **~34 tok/s** BitNet 2B4T sur i7-13700H laptop, 0,4 Go RAM — pertinent pour spike v0.11 CPU ultra-léger, mais **hors llama.cpp stock** (bitnet.cpp standalone).
6. **Bande passante mémoire** : i7-12700H + **32 Go DDR5-4800 dual-channel** place cette machine dans la colonne « i7/DDR5 » d’InsiderLLM (~77 GB/s). Le CPU **pourrait** atteindre 30–50 tok/s sur Qwen3.5-0.8B en llama.cpp pur (`-ngl 0`) — **non mesuré** chez nous ; nos 14,9 tok/s CUDA suggèrent que le **GPU 4 Go** sous-exploite le potentiel CPU de cette config.
7. **Stratégie device sur XPS 4 Go** : avec 32 Go RAM et GPU 4 Go, un mode `auto` intelligent pourrait préférer **CPU llama-cpp** pour modèles &lt;1,5 Go Q4, et CUDA seulement si VRAM libre suffisante — à valider par bench A/B `ngl=0` vs `ngl=99`.

### Matrice décision produit (bench + InsiderLLM)

| Profil utilisateur | Modèle Akasha recommandé | tok/s attendu | UX |
|--------------------|--------------------------|---------------|-----|
| Laptop GPU 4 Go + CUDA | Qwen3.5-0.8B Q4 (candidat) ou Qwen2.5-1.5B | 7–15 | Marginal à acceptable |
| Laptop GPU 4 Go, pas de GGUF | Candle Qwen3 0.6B | < 1 | Onboarding uniquement |
| Desktop GPU 8–12 Go | Qwen2.5-1.5B Q4 | 20+ (gate v0.9) | Fluide |
| CPU seul, llama-cpp | Qwen3.5-0.8B ou SmolLM2-360M GGUF | 10–50 (selon RAM/canaux) | Acceptable à fluide |
| CPU seul, Candle actuel | Qwen3 0.6B | < 1 | Non chat |
