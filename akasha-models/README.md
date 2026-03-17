# Akasha Models — projet annexe

Services de modèles (LLM, TTS/STT) montés **à la demande** pour le daemon Akasha. Le daemon reste client et se connecte via les URL configurées dans `llm_router.yaml` et `voice_router.yaml`.

## Services

| Service   | Rôle        | Port (ex.) | Config Akasha                    |
|-----------|-------------|------------|----------------------------------|
| Ollama    | LLM local   | 11434      | `llm_router.yaml` → `providers.ollama.base_url` |
| BitNet    | LLM local   | 8080       | `llm_router.yaml` → `providers.bitnet.base_url` |
| TTS       | Synthèse vocale (edge-tts) | 8765 | `voice_router.yaml` → `tts.base_url` |
| STT       | Transcription (faster-whisper) | 8766 | `voice_router.yaml` → `stt.base_url` |

## Démarrage à la demande

Avec Docker Compose (profiles) :

```bash
# LLM Ollama
docker compose --profile ollama up -d

# LLM BitNet (après avoir placé un modèle GGUF dans le volume ; voir ci-dessous)
docker compose --profile bitnet up -d

# TTS + STT (construction des images tts/ et stt/ au premier lancement)
docker compose --profile voice up -d

# Ollama + voix
docker compose --profile ollama --profile voice up -d
```

**BitNet** : le service part du répertoire `bitnet/` (Dockerfile qui clone et compile [BitNet](https://github.com/microsoft/BitNet)). Il faut fournir un modèle GGUF dans le volume monté sur `/models`. Par défaut le compose utilise un volume nommé `bitnet_models`. Pour utiliser un dossier local, remplacer dans `docker-compose.yml` le volume du service `bitnet` par exemple par `./bitnet-models:/models`, puis :
```bash
# Télécharger le modèle (une fois)
pip install huggingface_hub && huggingface-cli download microsoft/BitNet-b1.58-2B-4T-gguf --local-dir ./bitnet-models/BitNet-b1.58-2B-4T
docker compose --profile bitnet up -d
```

**TTS** (`tts/`) : serveur Python (edge-tts + pydub) qui expose `POST /tts` avec `{"text": "..."}` et renvoie du WAV. Variable d’environnement `TTS_VOICE` (défaut : `fr-FR-DeniseNeural`).

**STT** (`stt/`) : serveur Python (faster-whisper) qui expose `POST /stt` avec le corps audio WAV et renvoie `{"text": "..."}`. Variable `WHISPER_MODEL` (défaut : `base` ; mettre `tiny` pour un démarrage plus rapide).

Sans Docker : lancer les binaires/serveurs manuellement et pointer les `base_url` vers les hôtes/ports utilisés.

## URLs pour Akasha

Une fois les services démarrés, configurez le **data_dir** d’Akasha :

**llm_router.yaml** (déjà documenté dans `spec/llm_router.example.yaml`) :

```yaml
providers:
  ollama:
    base_url: "http://localhost:11434"   # ou http://<host>:11434
  bitnet:
    base_url: "http://localhost:8080"
```

**voice_router.yaml** (copier depuis `spec/voice_router.example.yaml`) :

```yaml
tts:
  base_url: "http://localhost:8765"   # service TTS (Pocket TTS / moshi-server TTS)
stt:
  base_url: "http://localhost:8766"   # service STT (moshi-server STT)
```

Si les services tournent sur une autre machine, remplacer `localhost` par l’IP ou le hostname.

## TTS/STT (Docker ou alternatives)

Ce dépôt fournit des images Docker pour TTS et STT compatibles avec le daemon Akasha :

- **TTS** (`./tts`) : edge-tts (Microsoft) + pydub → `POST /tts` avec `{"text": "..."}` → corps WAV.
- **STT** (`./stt`) : faster-whisper → `POST /stt` avec corps audio WAV → `{"text": "..."}`.

Le daemon Akasha appelle :
- **TTS** : `POST {tts.base_url}/tts` avec `{"text": "..."}`, réponse = corps binaire WAV.
- **STT** : `POST {stt.base_url}/stt` avec corps = audio WAV, réponse JSON `{"text": "..."}`.

**Alternatives hors Docker** (Kyutai) :
- **moshi-server** (Rust) : [kyutai-labs/moshi](https://github.com/kyutai-labs/moshi) — STT : `moshi-server worker --config configs/config-stt-en_fr-hf.toml` (8766), TTS : `moshi-server worker --config configs/config-tts.toml` (8765).
- **Pocket TTS** : `pip install pocket-tts`, puis serveur HTTP et `tts.base_url` dans `voice_router.yaml`.
