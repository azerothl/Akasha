# Akasha Models — projet annexe

Services de modèles (LLM, TTS/STT) montés **à la demande** pour le daemon Akasha. Le daemon reste client et se connecte via les URL configurées dans `llm_router.yaml` et `voice_router.yaml`.

## Services

| Service   | Rôle        | Port (ex.) | Config Akasha                    |
|-----------|-------------|------------|----------------------------------|
| Ollama    | LLM local   | 11434      | `llm_router.yaml` → `providers.ollama.base_url` |
| BitNet    | LLM local   | 8080       | `llm_router.yaml` → `providers.bitnet.base_url` |
| TTS (Kyutai / Pocket TTS) | Synthèse vocale | 8765 | `voice_router.yaml` → `tts.base_url` |
| STT (Kyutai moshi-server) | Transcription | 8766 | `voice_router.yaml` → `stt.base_url` |

## Démarrage à la demande

Avec Docker Compose (profiles) :

```bash
# LLM uniquement (Ollama)
docker compose --profile ollama up -d

# TTS/STT uniquement (si images/services définis)
docker compose --profile voice up -d

# Tout
docker compose --profile ollama --profile voice up -d
```

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

## TTS/STT (Kyutai)

- **Modèles et configs** : [delayed-streams-modeling](https://github.com/kyutai-labs/delayed-streams-modeling/) (STT `kyutai/stt-1b-en_fr`, TTS, configs TOML).
- **Serveur Rust** : crate `moshi-server` (repo [kyutai-labs/moshi](https://github.com/kyutai-labs/moshi)).  
  - STT : `moshi-server worker --config configs/config-stt-en_fr-hf.toml` (ex. port 8766).  
  - TTS : `moshi-server worker --config configs/config-tts.toml` (ex. port 8765).
- **Pocket TTS** (CPU, 100M params) : `pip install pocket-tts`, puis lancer le serveur HTTP (voir doc Kyutai) et définir `tts.base_url` sur son URL.

Le daemon Akasha appelle :
- **TTS** : `POST {tts.base_url}/tts` avec `{"text": "..."}`, réponse = corps binaire WAV.
- **STT** : `POST {stt.base_url}/stt` avec corps = audio WAV, réponse JSON `{"text": "..."}`.

Adapter les endpoints si le service expose un autre schéma (ex. Unmute WebSocket) via un petit proxy ou une évolution du client dans le daemon.
