# akasha-embedded-llm

POC : petit modèle de langage intégré via **Candle** (Qwen3 0.6B), pour onboarding, diagnostics, réponses simples sans LLM externe.

## Utilisation

```rust
use akasha_embedded_llm::EmbeddedLlm;

let llm = EmbeddedLlm::new();
if EmbeddedLlm::is_available() {
    let reply = llm.complete("What is 2+2? Reply in one sentence.", None, Some(0.3))?;
    println!("{}", reply);
}
```

Au premier appel à `complete()`, le modèle est téléchargé depuis Hugging Face puis chargé (CPU uniquement).

## Plateformes

- **Linux / WSL2** : recommandé pour le POC. Build et exécution testés.
- **Windows natif** : le build peut réussir (Candle 0.9.2) ; en cas de souci (toolchain, runtime), utiliser WSL2.

Voir [spec/34_embedded_small_model.md](../../spec/34_embedded_small_model.md) pour l’objectif et les options techniques.

## Dépendances

- `candle-pipelines` 0.0.7 (CPU uniquement, pas de CUDA/Metal par défaut).
- Premier run : téléchargement du modèle Qwen3 0.6B depuis le Hub (cache local).

## Intégration (à venir)

- Daemon : fallback pour `/api/diagnostic/advice` quand aucun provider Ollama/OpenAI n’est configuré.
- Provider `akasha_embedded` dans le routeur LLM pour remplacer le stub `akasha_core:core`.
