# Recherche modèles embarqués v0.10.0 (R0)

Document de veille **R0a–R0d** pour la release Akasha 0.10.0. Complète [roadmap_v0.10.0.md](../releases/roadmap_v0.10.0.md).

**Statut** : publié (cycle v0.10)  
**Dernière mise à jour** : 2026-06-21

---

## Synthèse exécutive

| Axe | Décision v0.10 |
|-----|----------------|
| **R0a AR** | **Statu quo défaut** Qwen2.5-1.5B Q4_K_M (CUDA) ; **variantes wizard** SmolLM2-360M Q4 + Qwen3-0.6B (Candle) ; **shortlist élargie** (Qwen3.5 Small, Gemma 3 1B, Qwen3-1.7B) pour bench v0.11 ; **veille** Qwen3.6 / ROCmFP4 / MTP (hors manifeste v0.10) |
| **R0b DLM** | **Surveiller seulement** — report spike v0.11 (latence steps, pas de runtime Rust/Windows mature) |
| **R0c Moteurs** | **Patterns produit v0.10** : statuts evidence-gated dans wizard/doctor ; veille Camelid/MLX/vLLM ; pas de nouveau backend prod |
| **R0d Rbitnet** | **Spike documenté no-go prod v0.10** ; bench comparatif optionnel v0.11 ; llama-cpp-4 reste backend GPU par défaut |

---

## R0a — Modèles autoregressifs (AR)

Veille HF **juin 2026** : familles Qwen3.5 Small, Qwen3.6 dense/MoE, quant community (bartowski, unsloth) et formats expérimentaux (ROCmFP4, MTP grafté). La shortlist est découpée en **trois tiers** selon la contrainte onboarding (&lt; ~1,5 Go download) et la compatibilité **llama-cpp-4 stock** (release Akasha).

### Tier 1 — Onboarding (&lt; ~1,5 Go, llama.cpp stock)

| Modèle | Repo HF (ex.) | Quant | Taille ~ | Backend | FR chat | Décision v0.10 |
|--------|---------------|-------|----------|---------|---------|----------------|
| Qwen2.5-1.5B-Instruct | [Qwen/Qwen2.5-1.5B-Instruct-GGUF](https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct-GGUF) | Q4_K_M | ~986 Mo | llama_cpp CUDA | Bon | **Défaut prod** (`embedded_models.json`) |
| SmolLM2-360M-Instruct | [HuggingFaceTB/SmolLM2-360M-Instruct-GGUF](https://huggingface.co/HuggingFaceTB/SmolLM2-360M-Instruct-GGUF) | Q4_K_M | ~250 Mo | llama_cpp | Correct EN, FR limité | **Variante wizard** (manifeste) |
| Qwen3-0.6B | bundlé repo | Candle ST | CPU zip | candle | Correct | **Info wizard** (pas de GGUF) |
| Qwen3.5-0.8B | [unsloth/Qwen3.5-0.8B-GGUF](https://huggingface.co/collections/unsloth/qwen35) · [diodel/Qwen3.5-0.8B-Q4_K_M-GGUF](https://huggingface.co/diodel/Qwen3.5-0.8B-Q4_K_M-GGUF) | Q4_K_M | ~529 Mo | llama_cpp | À valider | **Candidat bench** — plus récent que Qwen2.5, multimodal natif (mmproj séparé) |
| Qwen3.5-2B / 4B | [unsloth/Qwen3.5-2B-GGUF](https://huggingface.co/unsloth/Qwen3.5-2B-GGUF) · 4B ~7 Go Q4 | Q4_K_M | ~1,5–7 Go | llama_cpp | Meilleur que 0.8B | **Hors onboarding** ; bench qualité si tier 1 insuffisant |
| Qwen3-0.6B GGUF | [Qwen/Qwen3-0.6B-GGUF](https://huggingface.co/Qwen/Qwen3-0.6B-GGUF) | Q4_K_M | ~400 Mo | llama_cpp | Correct | **Candidat bench** — alignement famille Qwen3 vs Candle bundlé |

### Tier 2 — Bench v0.11 (borderline 1,5–2,5 Go, llama.cpp stock)

| Modèle | Repo HF (ex.) | Quant | Taille ~ | Backend | Notes | Décision |
|--------|---------------|-------|----------|---------|-------|----------|
| Qwen3-1.7B-Instruct | [bartowski/Qwen_Qwen3-1.7B-GGUF](https://huggingface.co/bartowski/Qwen_Qwen3-1.7B-GGUF) | Q4_K_M | ~1,28 Go | llama_cpp | Meilleur FR que 1.5B | **Bench prioritaire** — légèrement au-dessus contrainte onboarding |
| Gemma 3 1B IT (QAT) | [bartowski/google_gemma-3-1b-it-qat-GGUF](https://huggingface.co/bartowski/google_gemma-3-1b-it-qat-GGUF) | Q4_K_M | ~806 Mo | llama_cpp | Architecture `gemma3`, texte seul en GGUF courant | **Bench prioritaire** — bon ratio taille/qualité |
| Phi-4-mini-instruct | [microsoft/Phi-4-mini-instruct-GGUF](https://huggingface.co/microsoft/Phi-4-mini-instruct-gguf) (community quants) | Q4_K_M | ~2,5 Go | llama_cpp | Fort en code EN | **Report** — hors contrainte download wizard |
| Qwen3.5-9B | [unsloth/Qwen3.5-9B-GGUF](https://huggingface.co/unsloth/Qwen3.5-9B-GGUF) | UD-Q4_K_XL | ~9 Go | llama_cpp | 256K ctx, hybrid thinking | **Qualité référence** P2b — pas candidat embarqué desktop |

### Tier 3 — Veille frontière HF (hors manifeste v0.10)

Modèles récents **non compatibles** ou **hors cible** release Windows CUDA / onboarding, à suivre pour v0.11+ et veille moteurs (R0c).

| Modèle | Repo HF | Quant / format | Taille ~ | Blocker Akasha v0.10 | Intérêt veille |
|--------|---------|----------------|----------|----------------------|----------------|
| Qwen3.6-40B Deckard MTP | [plunderstruck/Qwen3.6-40B-Deckard-MTP-ROCmFP4-GGUF](https://huggingface.co/plunderstruck/Qwen3.6-40B-Deckard-MTP-ROCmFP4-GGUF) | `q4_0_rocmfp4` / `q4_0_rocmfp4_fast` | ~22 Go | **Fork obligatoire** [charlie12345/ROCmFPX](https://github.com/charlie12345/ROCmFPX) — tensors `q4_0_rocmfp4*` **incompatibles llama.cpp stock**, Ollama, LM Studio | MTP grafté (head 27B), vision Qwen3-VL (`mmproj`), ~25 t/s Strix Halo ; frankenmerge communautaire — **veille AMD ROCm / speculative decode** |
| Qwen3.6-40B Deckard (Q6/Q8) | [DavidAU/Qwen3.6-40B-…-NEO-CODE-Di-IMatrix-MAX-GGUF](https://huggingface.co/DavidAU/Qwen3.6-40B-Claude-4.6-Opus-Deckard-Heretic-Uncensored-Thinking-NEO-CODE-Di-IMatrix-MAX-GGUF) · [PiehSoft/Qwen3.6-40B-Deckard-MTP-Q6_K](https://huggingface.co/PiehSoft/Qwen3.6-40B-Deckard-MTP-Q6_K) | Q6_K / Q8_0 | 40–54 Go | Taille + licence derivative | Source des quants ROCmFP4 ; MTP injection documentée |
| Qwen3.6-27B | [unsloth/Qwen3.6-27B-GGUF](https://huggingface.co/unsloth/Qwen3.6-27B-GGUF) · [bartowski/Qwen_Qwen3.6-27B-GGUF](https://huggingface.co/bartowski/Qwen_Qwen3.6-27B-GGUF) | Q4_K_M | ~16,8 Go | VRAM 24 Go+ ; pas onboarding | Dense flagship ≤30B, hybrid thinking, mmproj multimodal — **référence qualité** si utilisateur power GPU |
| Qwen3.5-35B-A3B (MoE) | [Qwen/Qwen3.5-35B-A3B](https://huggingface.co/Qwen/Qwen3.5-35B-A3B) + GGUF community | Q4 | ~22 Go actifs ~3B | MoE + taille | Actif ~3B/token — intéressant perf/taille à moyen terme |
| Qwen3.6-122B-A10B (MoE) | HF + GGUF (emerging) | Q4 | 100 Go+ | Hors desktop | Veille serveur / Ollama externe |

**Leçons Deckard / ROCmFP4 (juin 2026)** :

- Quant custom **ROCmFP4** (~4,5 bpw) cible **AMD Strix Halo (gfx1151)** — pas le stack NVIDIA CUDA release Akasha.
- **MTP self-speculative** (`--spec-type draft-mtp`) : gain throughput mesuré sur fork dédié ; Akasha n’expose pas encore draft/MTP dans llama-cpp-4.
- **Vision** : `mmproj` séparé + `--image-min-tokens 1024` — pattern à généraliser si Akasha embarque Qwen3.5/3.6 multimodal (hors v0.10).
- **Qualité** : frankenmerge + imatrix mesurée (KL/PPL vs Q8 ref) mais **pas de bench coding absolu** — ne pas promouvoir en prod sans protocole P2b.

### Quantification

- **Q4_K_M** : meilleur compromis qualité/tok/s pour chat court et `doctor --advice` — conservé.
- **IQ4_XS** : ~15 % plus petit, légère régression qualité FR — candidat bench P2b, pas swap défaut.
- **Q8_0** : qualité +, taille ×2 — hors contrainte onboarding.

### Matrice décision modèle

| Option | v0.10 |
|--------|-------|
| Swap défaut CUDA | Non — Qwen2.5-1.5B Q4_K_M validé v0.9 |
| Variantes manifeste v0.10 | Oui — SmolLM2-360M ; pas d’ajout Qwen3.5/Gemma avant bench |
| Bench v0.11 (tier 1–2) | Qwen3.5-0.8B, Qwen3-0.6B GGUF, Gemma 3 1B, Qwen3-1.7B |
| Veille tier 3 | Qwen3.6-27B/40B, Deckard MTP ROCmFP4, MoE Qwen3.5 — doc only |

---

## R0b — Modèles par diffusion (DLM)

Référence : [Awesome-DLMs](https://github.com/VILA-Lab/Awesome-DLMs).

### Familles cartographiées

| Famille | Exemples | Weights + inférence | Pertinence Akasha |
|---------|----------|-------------------|-------------------|
| Discrets | LLaDA, Dream, Fast-dLLM | HF + Python mostly | TTFT élevé (steps), streaming non AR |
| Continus | — | Recherche | Hors scope embarqué desktop |
| Multimodaux | — | Lourd | Hors onboarding |

### Adéquation cas d'usage

- **Onboarding / premier message** : latence TTFT et génération séquentielle AR favorisent llama-cpp/Candle.
- **Doctor --advice** : réponses courtes structurées — AR suffisant.
- **Streaming UI** : DLM génère par blocs/steps — UX incompatible sans refonte chat.

### Candidats spike

| Candidat | Blockers |
|----------|----------|
| LLaDA-8B-Instruct (quant) | >1,5 Go ; inférence Python ; pas Windows natif Rust |
| Dream-7B | Idem |

**Décision R0b** : **surveiller seulement** / **report v0.11**. Pas de spike P5 DLM en v0.10.

---

## R0c — Moteurs d'inférence

### Matrice stacks

| Moteur | Rust natif | Windows release | GGUF | OpenAI API | CPU | CUDA |
|--------|------------|-----------------|------|------------|-----|------|
| Candle (Akasha) | Oui | Oui | Non | via daemon | Oui | legacy |
| llama-cpp-4 (Akasha) | binding | Oui | Oui | via daemon | Oui | Oui |
| Ollama (externe) | non | Oui | Oui | Oui | Oui | Oui |
| Camelid | Oui | partiel | audit | Oui | Oui | ? |
| MLX | non | non | Oui | — | Apple | Metal |
| vLLM | non | non | — | Oui | non | Linux server |

### UMA / CXL / PMEM

- **v0.10** : veille uniquement — pas de hardware UMA sur runners CI GitHub ni cible Tauri Windows desktop.
- **Leçons applicables sans UMA** : quant early (Q4), buffer unique modèle, réduire copies CPU↔GPU (déjà via llama-cpp CUDA).

### Correctness & transparence (Camelid-inspired)

**Patterns produit v0.10** (implémentés) :

- `embedded-status` : champs `gguf_present`, `llama_cpp_compiled`, `action`, `ready_for_chat`
- `doctor --json` : `action: "embedded-download"` si llama_cpp compilé sans GGUF
- Wizard : pas de « prêt » GPU sans GGUF quand build CUDA

### Ollama vs embarqué

- **Ollama** : provider optionnel (`init --defaults` si service local détecté).
- **`akasha_embedded`** : chemin first-use sans service externe — inchangé comme défaut sans Ollama.
- Convergence possible v0.11 : pull modèle style Ollama via manifeste — hors v0.10.

### Shortlist moteurs spike

| Candidat | Décision |
|----------|----------|
| Camelid (audit GGUF) | Veille — pas d'intégration v0.10 |
| vLLM / MLX | Hors cible Windows embarqué |

**Décision R0c** : **patterns produit v0.10** + **veille seule** pour nouveaux backends.

---

## R0d — Runtimes Akasha (Rbitnet)

Évaluation du dépôt [Rbitnet](../../../../../Rbitnet) (bitnet-core, GGUF, Qwen3 loaders).

| Critère | llama-cpp-4 | Rbitnet |
|---------|-------------|---------|
| Intégration Akasha | ✅ prod v0.9 | Effort élevé (crate séparé, API différente) |
| Windows CUDA release | ✅ | En développement (GPU_NATIVE_ROADMAP) |
| Perf Qwen2.5 1.5B Q4 | Baseline v0.9 >20 tok/s GTX 3080+ | Non benchmarké dans Akasha |
| Maintenance | llama-cpp-rs | Équipe Rbitnet |

**Décision R0d** : **no-go prod v0.10** ; llama-cpp reste backend GPU ; spike P5 limité à note technique (pas de remplacement par défaut).

---

## Protocole bench candidats AR

Entrées dans `spec/embedded_models.json` ; scripts `bench_embedded.ps1` / `.sh`.

### 5 prompts qualitatifs fixes

1. Salutation : « Bonjour, présente-toi en une phrase. »
2. Doctor-like : « Mon daemon ne répond pas sur le port 3876, que vérifier ? »
3. JSON court : « Réponds uniquement avec un JSON {"ok":true}. »
4. FR : « Explique en français ce qu'est Akasha en 2 phrases. »
5. Code 10 lignes : « Écris une fonction Python fibonacci(n) en 10 lignes max. »

---

## Risques

| Risque | Mitigation v0.10 |
|--------|------------------|
| Régression FR (swap modèle) | Statu quo Qwen2.5-1.5B |
| Taille download | SmolLM variante optionnelle ; défaut <1 Go |
| UX premier message | Baselines P2b + wizard calibré |
| « Supported » trompeur | evidence-gated doctor/status |

---

## Plan v0.11 (issues reportées)

- Bench tier 1–2 : Qwen3.5-0.8B, Qwen3-0.6B GGUF, Gemma 3 1B QAT, Qwen3-1.7B, Phi-4-mini (hors onboarding)
- Évaluer swap défaut si Qwen3.5-0.8B ou Gemma 3 1B bat Qwen2.5-1.5B sur protocole FR P2b
- Veille tier 3 : compatibilité Qwen3.6 dans llama-cpp-4 ; MTP/speculative decode ; formats ROCmFP4 vs release NVIDIA
- Spike DLM discret si runtime Rust/Windows mature
- Intégration Rbitnet POC avec bench comparatif
- Camelid-style `/api/capabilities` pour modèles GGUF (+ flags vision/mmproj)
