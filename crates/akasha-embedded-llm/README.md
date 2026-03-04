# akasha-embedded-llm

POC : petit modèle de langage intégré via **Candle**, pour onboarding, diagnostics, réponses simples sans LLM externe.

- **Par défaut** : Qwen3 0.6B (candle-pipelines).
- **Baguettotron 321M** : pour configs à faible ressource ou usage conversation ; activer la feature `baguettotron` et définir `AKASHA_EMBEDDED_MODEL=baguettotron`. Modèle : [PleIAs/Baguettotron](https://huggingface.co/PleIAs/Baguettotron).

### Tester avec Baguettotron au lieu de Qwen

1. **Compiler le daemon avec le support Baguettotron** :
   ```bash
   cargo build -p akasha-daemon --features embedded-baguettotron
   ```
2. **Au lancement, définir la variable d’environnement** :
   ```bash
   AKASHA_EMBEDDED_MODEL=baguettotron ./target/debug/akasha-daemon
   ```
   Ou pour un test rapide : `AKASHA_EMBEDDED_MODEL=baguettotron akasha start` (si le binaire est dans le PATH).
3. Le premier appel téléchargera le modèle [PleIAs/Baguettotron](https://huggingface.co/PleIAs/Baguettotron) (~321M) depuis Hugging Face, puis les requêtes utiliseront Baguettotron.

## Utilisation

```rust
use akasha_embedded_llm::EmbeddedLlm;

let llm = EmbeddedLlm::new();
if EmbeddedLlm::is_available() {
    let reply = llm.complete("What is 2+2? Reply in one sentence.", None, Some(0.3))?;
    println!("{}", reply);
}
```

Au premier appel à `complete()`, le modèle est téléchargé depuis Hugging Face puis chargé. Les fichiers sont mis en cache dans le **cache Hugging Face** par défaut (`~/.cache/huggingface/hub` ou `%USERPROFILE%\.cache\huggingface\hub` sous Windows) ; vous pouvez définir **`HF_HOME`** pour utiliser un autre répertoire (ex. `HF_HOME=$AKASHA_DATA_DIR/hf_cache`). Par défaut l’inférence utilise le **CPU** ; avec la feature **`cuda`**, le modèle utilise le **GPU** (CUDA) si la machine en dispose d’un compatible (sinon repli sur CPU à l’exécution).

## Vérifier que le modèle est prêt

- **Depuis la TUI** : tapez `/embedded`. Si le modèle est compilé et chargé, vous verrez « Modèle embarqué : disponible et prêt ».
- **Depuis le CLI** : `akasha doctor` affiche un check `embedded_llm` (OK si prêt). Pour un détail côté routeur : `curl http://127.0.0.1:PORT/api/router/embedded-status` (remplacer PORT par le port du daemon, ex. 3784).
- **Compilation** : le daemon doit être compilé avec la feature `embedded` (c’est le cas par défaut pour `akasha-daemon`). Si `embedded_llm` est KO au doctor, vérifier que vous lancez bien le binaire daemon compilé avec `--features embedded` ou la default du crate.

## Plateformes

- **Linux / WSL2** : recommandé pour le POC. Build et exécution testés.
- **Windows natif** : le build peut réussir (Candle 0.9.2) ; en cas de souci (toolchain, runtime), utiliser WSL2.

Voir [spec/34_embedded_small_model.md](../../spec/34_embedded_small_model.md) pour l’objectif et les options techniques.

## GPU (CUDA)

Pour utiliser le GPU quand la machine a une carte NVIDIA compatible :

1. **Compiler avec la feature `embedded-cuda`** (nécessite le toolkit CUDA installé) :
   ```bash
   cargo build -p akasha-daemon --features embedded-cuda
   # ou avec Baguettotron : --features "embedded-baguettotron,embedded-cuda"
   ```
2. À l’exécution, si CUDA est disponible le modèle tourne sur le GPU ; sinon il utilise le CPU. **Sous WSL2** la compilation CUDA échoue souvent (nvcc/driver) ; dans ce cas utiliser l’option MKL ci‑dessous.

**Optimisation CPU sans CUDA (Intel MKL, optionnel)** :
```bash
cargo build -p akasha-daemon --features embedded-mkl
# ou avec Baguettotron : --features "embedded-baguettotron,embedded-mkl"
```
Accélère les opérations matricielles sur CPU (Intel MKL). **Attention** : sur certaines configs (ex. WSL2 avec MKL fourni par ocipkg) le link peut échouer avec `undefined symbol: hgemm_` (demi-précision non incluse dans le build MKL). Dans ce cas, **ne pas** utiliser `embedded-mkl` et compiler uniquement avec `--features embedded` (ou `embedded-baguettotron`) ; le CPU par défaut fonctionnera, ou utiliser Ollama pour de meilleures perfs.

Sous Windows, la compilation avec CUDA peut exiger Visual Studio (`cl.exe`). Linux ou WSL2 sont recommandés pour le build avec `embedded-cuda` ; en cas d’échec sur WSL2, utiliser **`embedded-mkl`**.

## Dépendances

- **candle** (défaut) : `candle-pipelines` 0.0.7, modèle Qwen3 0.6B au premier run.
- **baguettotron** (optionnel) : `candle-transformers`, `hf-hub`, `tokenizers` ; modèle PleIAs/Baguettotron (321M) au premier run.
- **cuda** (optionnel) : active CUDA pour candle-pipelines et candle-transformers ; inférence sur GPU si disponible.
- **mkl** (optionnel) : active Intel MKL pour candle-core/candle-nn/candle-transformers ; accélération CPU sans CUDA (Linux/WSL2, Intel).

## Intégration (à venir)

- Daemon : fallback pour `/api/diagnostic/advice` quand aucun provider Ollama/OpenAI n’est configuré.
- Provider `akasha_embedded` dans le routeur LLM pour remplacer le stub `akasha_core:core`.
