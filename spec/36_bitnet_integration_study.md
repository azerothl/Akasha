# Étude d’intégration BitNet pour les modèles locaux Akasha

Ce document consigne l’étude de faisabilité pour intégrer [BitNet](https://github.com/microsoft/BitNet) (Microsoft) comme option d’inférence locale dans Akasha : compatibilité API, positionnement par rapport à Ollama et au modèle embarqué Candle, possibilité de fourniture/installation à l’init, et recommandation.

---

## 1. Contexte

- **BitNet** : framework d’inférence pour LLMs 1-bit (BitNet b1.58), basé sur llama.cpp. Gains annoncés : 1,37x à 6,17x en vitesse sur CPU, 55–82 % de réduction d’énergie, exécution de modèles jusqu’à 100B sur un seul CPU (5–7 tok/s). Modèles officiels : 2B, 3B, 8B ; format GGUF après quantification (`setup_env.py`).
- **Akasha** : routeur LLM avec providers Ollama, OpenAI, OpenRouter, `akasha_embedded` (Candle in-process), `akasha_core`. Config dans `llm_router.yaml`, trait `LLMProvider` dans `akasha-llm`.

BitNet fournit **run_inference_server.py** qui lance le binaire **llama-server** (build C++ BitNet) avec un modèle GGUF sur un port (défaut 8080). L’API utilisée est celle du serveur **llama.cpp**, pas une API spécifique BitNet.

---

## 2. Note API — serveur BitNet / llama-server

Le serveur utilisé par BitNet est le serveur **llama.cpp** (ou dérivé dans le build BitNet). Les points suivants permettent d’intégrer un provider Akasha sans dépendre de Python côté runtime.

### 2.1 URL de base et endpoints

| Élément | Valeur |
|--------|--------|
| **URL de base** | Configurable ; par défaut `http://127.0.0.1:8080` (run_inference_server.py). Pour l’API OpenAI-compatible, la base utilisée côté client est en général `http://127.0.0.1:8080/v1` (préfixe `/v1`). |
| **Complétion (chat)** | `POST /v1/chat/completions` — format OpenAI-compatible (messages, max_tokens, temperature). |
| **Complétion (texte)** | `POST /completion` ou `POST /v1/completions` — format legacy ou OpenAI selon build. |
| **Streaming** | Supporté : `"stream": true` dans le body ; réponses en SSE (`data: {...}`), un objet JSON par chunk. |

Pour un provider Akasha, privilégier **`/v1/chat/completions`** afin de réutiliser le même schéma que pour OpenAI (messages, choices[0].message.content, usage).

### 2.2 Schéma de requête (aligné OpenAI)

```json
{
  "model": "bitnet-2b",
  "messages": [{"role": "user", "content": "Hello"}],
  "max_tokens": 1024,
  "temperature": 0.7,
  "stream": false
}
```

Avec `"stream": true`, le serveur renvoie des événements SSE ; chaque ligne `data: {...}` contient un objet (ex. `choices[0].delta.content`).

### 2.3 Schéma de réponse (non-streaming)

Extraction côté Akasha (comme pour OpenAI) :

- `choices[0].message.content` — texte généré
- `usage.prompt_tokens` / `usage.completion_tokens` — usage
- `model` — modèle utilisé

### 2.4 Disponibilité du serveur

- Aucun endpoint standard type `/health` obligatoire. Pour `is_available()`, on peut : faire un `GET` sur une route documentée (ex. `/v1/models` si disponible) ou un `POST /v1/chat/completions` minimal avec `max_tokens: 0` pour éviter de consommer du contexte.
- En pratique : un `GET {base_url}/health` ou `GET {base_url}/` avec timeout court (2 s) est souvent suffisant si le serveur l’expose ; sinon, considérer le serveur disponible si une requête de complétion ne renvoie pas d’erreur de connexion.

**Conclusion API** : un provider « BitNet » peut s’appuyer sur l’API OpenAI-compatible du serveur (base_url + `/v1/chat/completions`), avec ou sans streaming. Aucune dépendance Python côté runtime : seul le binaire serveur + un fichier GGUF sont nécessaires.

---

## 3. Choix d’intégration

### Option A — Nouveau provider `bitnet` dans le routeur (recommandé)

- Ajouter un provider (ex. `BitNetProvider`) dans `akasha-llm` qui appelle le serveur BitNet (URL configurable, ex. `http://127.0.0.1:8080`).
- Config : `providers.bitnet.base_url` dans `llm_router.yaml`, et `task_types.*.primary.provider: bitnet` avec un `model` (nom logique ou nom de fichier selon ce que le serveur expose).
- Avantages : expérience unifiée (comme Ollama), messages d’erreur et métadonnées explicites « BitNet ».
- Inconvénient : maintenir le client (schéma requête/réponse) si l’API llama.cpp évolue.

### Option B — Provider générique « OpenAI-compatible »

- Réutiliser un provider de type OpenAI avec `base_url` personnalisée (ex. `http://127.0.0.1:8080/v1`) et pas de clé API (ou clé vide).
- Moins de code dédié, mais moins de clarté pour l’utilisateur (nom du provider, messages, discovery).

### Option C — Documentation seule

- Guide : installer BitNet, lancer le serveur, configurer une URL dans `llm_router.yaml` (ex. via un provider existant si l’API colle).
- Aucun nouveau code, UX moins intégrée.

**Recommandation** : **Option A** — implémenter un provider `bitnet` avec `base_url` et appel à `/v1/chat/completions`, pour un parcours « modèle local performant sans GPU » clair et maintenable.

---

## 4. Comparatif performances (à compléter par mesures)

Les chiffres ci-dessous sont des ordres de grandeur à confirmer par des mesures sur une machine cible (laptop CPU seul).

| Backend | Modèle | Tokens/s (indicatif) | Premier token | RAM (indicatif) |
|---------|--------|------------------------|---------------|-----------------|
| akasha_embedded (Candle) | Qwen 0.6B / Baguettotron 321M | Faible (CPU) | Long après preload | Modéré |
| Ollama | 3B–7B classique | Variable (GPU/CPU) | Variable | Élevé |
| BitNet (llama-server) | BitNet 2B / 3B | 1,37x–6,17x vs baseline (claims) | À mesurer | À mesurer |

**Métriques à produire** (POC) : débit (tokens/s), latence au premier token, utilisation RAM, sur une même machine pour embedded, Ollama et BitNet (même prompt / même task_type). Optionnel : consommation énergétique.

---

## 5. UX et déploiement

- **Installation manuelle actuelle** : BitNet requiert Python, conda, cmake, clang, build C++. Étapes : clone, `setup_env.py` (téléchargement + conversion modèle), build, `run_inference_server.py`. Pour un utilisateur non tech, cette stack est lourde.
- **Modèles** : Hugging Face (et chemins officiels BitNet). Taille disque : de l’ordre de quelques centaines de Mo à ~1 Go pour un 2B en GGUF.
- **Config** : exemple minimal dans ce spec (voir § 7) pour utiliser le provider `bitnet` comme primary pour un task_type (ex. `conversation`).

---

## 6. BitNet fourni et installé par Akasha à l’init (utilisateur non tech)

Objectif : l’utilisateur n’a pas à installer Python, conda, cmake ni à builder BitNet ; Akasha fournit et installe au besoin lors de l’init (ou au premier usage).

### 6.1 Nécessaire au runtime

- Au **runtime** : binaire C++ du serveur (type `llama-server`) + fichier **modèle GGUF**. Le Python du repo BitNet sert à la préparation (setup_env, conversion, lancement script). Avec binaire + GGUF déjà disponibles, plus besoin de Python côté utilisateur.
- **Conclusion** : « Porter » ou « fournir » BitNet par Akasha = fournir ou télécharger le **binaire serveur** par plateforme + éventuellement un **modèle GGUF** (ou téléchargement guidé au premier lancement).

### 6.2 Faisabilité technique

| Aspect | Détail |
|--------|--------|
| **Binaire serveur** | BitNet ne publie pas forcément de binaires précompilés. Options : (1) construire en CI Akasha (build BitNet/llama-server par OS/arch) et les publier avec les releases, (2) s’appuyer sur des binaires communautaires/officiels s’ils apparaissent. Build cross-platform (Windows, macOS, Linux x86/ARM) à prévoir. |
| **Modèle GGUF** | Modèles disponibles (ex. Hugging Face). Soit héberger un GGUF par défaut (ex. 2B), soit déclencher un téléchargement au premier usage (barre de progression UI/CLI). Taille : de l’ordre de quelques centaines de Mo à ~1 Go pour un 2B. |
| **Installation à l’init** | Lors de `akasha init`, option du type « Installer l’IA locale (BitNet) » : (a) télécharger le binaire serveur pour l’OS (si absent), (b) proposer ou effectuer le téléchargement d’un modèle par défaut, (c) écrire dans `llm_router.yaml` le provider `bitnet` avec l’URL du serveur (ex. `http://127.0.0.1:8080`). |
| **Lancement du serveur** | Pour un utilisateur non tech, le serveur doit démarrer sans commande manuelle : soit le daemon Akasha lance et surveille un sous-processus « bitnet-server » (chemin binaire + modèle en config), soit l’app desktop (Tauri) le fait au démarrage lorsque BitNet est sélectionné. À trancher (maintenabilité, crash, mises à jour). |

### 6.3 Points de vigilance

- **Licences** : BitNet (MIT), llama.cpp (MIT) — redistribution de binaires en général possible ; vérifier les dépendances tierces du build.
- **Taille** : inclure binaire (et optionnellement modèle) alourdit l’installateur ; alternative = téléchargement à la demande à l’init ou au premier usage, avec message clair (« Téléchargement de l’IA locale… »).
- **Mises à jour** : suivre les releases BitNet/llama.cpp pour correctifs et perf ; stratégie à définir (version fixe par version d’Akasha vs mise à jour automatique du binaire).

### 6.4 Livrables / recommandation « BitNet à l’init »

- **Vérification** : existence ou non de binaires précompilés BitNet/llama-server par plateforme ; si non, estimation de l’effort de build + hébergement (CI, artefacts).
- **Design init** : parcours concret dans `akasha init` (ou équivalent UI) : choix « IA locale BitNet » → téléchargement binaire (+ modèle optionnel) → config `llm_router` → démarrage du serveur (manuel vs géré par le daemon/app).
- **Recommandation** : selon la faisabilité (build, taille, maintenance), soit « BitNet fourni et installé à l’init » est retenu comme objectif pour une prochaine itération (avec tâches détaillées), soit il est repoussé après une première intégration « BitNet en mode manuel » (utilisateur lance le serveur lui-même).

---

## 7. Exemple de configuration llm_router.yaml (provider bitnet)

```yaml
version: "1.0"
providers:
  bitnet:
    base_url: "http://127.0.0.1:8080"
task_types:
  conversation:
    primary:
      provider: bitnet
      model: "default"
    fallback:
      - provider: akasha_embedded
        model: default
```

Si le serveur BitNet expose l’API sous le préfixe `/v1`, le provider Akasha utilisera en interne `{base_url}/v1` pour les appels à `/v1/chat/completions` (voir implémentation du BitNetProvider).

---

## 8. Décision et prochaines étapes

- **Décision** : **Go** pour l’intégration d’un provider **bitnet** dans le routeur (Option A). La décision « BitNet fourni et installé à l’init » est conditionnelle aux résultats de la vérification des binaires précompilés et du design init.
- **Prochaines étapes** :
  1. Implémenter `BitNetProvider` dans `akasha-llm` (appel à `/v1/chat/completions`, base_url configurable, pas de clé API).
  2. Enregistrer le provider dans le daemon à partir de `llm_router.yaml` (providers.bitnet.base_url).
  3. Ajouter un check optionnel dans `akasha doctor` pour détecter un serveur BitNet sur le port configuré.
  4. Compléter le comparatif performances par des mesures réelles (benchmarks).
  5. Étudier la faisabilité « BitNet à l’init » (build/hebergement binaires, parcours init, lancement géré) et décider du périmètre pour la prochaine itération.

**Limite connue : stabilité du serveur** — Dans certains environnements, le binaire llama-server (BitNet) peut quitter brutalement après une requête (par ex. après « prompt done », pendant la génération). Contournements : réduire `-c` (contexte), éviter les prompts très longs, ou lancer le serveur dans une boucle de relance. Détails et script de relance : [Runbook BitNet, section Dépannage](runbooks/03_bitnet_server_setup.md#dépannage).

---

## Références

- [microsoft/BitNet](https://github.com/microsoft/BitNet) — dépôt officiel, run_inference_server.py, setup_env.py.
- [Runbook : construire et lancer le serveur BitNet](runbooks/03_bitnet_server_setup.md) — prérequis, build de l’exécutable llama-server, téléchargement d’un modèle GGUF, lancement du serveur sur le port 8080 et test avec Akasha. Scripts d’aide : `scripts/bitnet-build.sh` (Linux/macOS), `scripts/bitnet-build.ps1` (Windows).
- llama.cpp server — API OpenAI-compatible (`/v1/chat/completions`, streaming via `stream: true`).
- [32_llm_router_architecture.md](32_llm_router_architecture.md) — architecture du routeur Akasha.
- [35_configuration_reference.md](35_configuration_reference.md) — référence `llm_router.yaml`.
