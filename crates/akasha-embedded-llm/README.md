# akasha-embedded-llm

POC : petit modèle de langage intégré via **Candle**, pour onboarding, diagnostics, réponses simples sans LLM externe.

- **Par défaut** : Qwen3 0.6B (candle-pipelines).
- **Baguettotron 321M** : pour configs à faible ressource ou usage conversation ; activer la feature `baguettotron` et définir `AKASHA_EMBEDDED_MODEL=baguettotron`. Modèle : [PleIAs/Baguettotron](https://huggingface.co/PleIAs/Baguettotron).

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

- **candle** (défaut) : `candle-pipelines` 0.0.7, modèle Qwen3 0.6B au premier run.
- **baguettotron** (optionnel) : `candle-transformers`, `hf-hub`, `tokenizers` ; modèle PleIAs/Baguettotron (321M) au premier run.

## Intégration (à venir)

- Daemon : fallback pour `/api/diagnostic/advice` quand aucun provider Ollama/OpenAI n’est configuré.
- Provider `akasha_embedded` dans le routeur LLM pour remplacer le stub `akasha_core:core`.
