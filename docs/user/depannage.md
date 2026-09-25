# Dépannage

## ## 12. Dépannage


- **Le daemon ne démarre pas** : vérifiez avec `akasha doctor`. Utilisez `akasha doctor --fix` pour créer le data_dir et les fichiers de config manquants.
- **Pas de réponses ou timeouts** : par défaut le modèle embarqué est utilisé ; vérifiez avec `akasha doctor` (section embedded_llm) ou `/embedded` dans le chat. Si vous utilisez Ollama, assurez-vous qu'il tourne ; pour le cloud, vérifiez les clés (vault ou variables). Consultez `akasha config models routes` et l'onglet Routeur pour voir les modèles actifs.
- **Réponses vides avec un modèle « thinking »** (ex. certains modèles OpenRouter) : augmentez `AKASHA_SYSTEM_TASK_MAX_TOKENS` (ex. 8192) via `akasha config env set AKASHA_SYSTEM_TASK_MAX_TOKENS 8192`, puis redémarrez le daemon.
- **L'onglet Doc est vide** : lancez `akasha start` depuis le dossier où vous avez extrait l'archive (celui qui contient le dossier `docs`). Vérifiez que le fichier `docs/user_guide.md` est bien présent.
- **Conseils personnalisés** : `akasha doctor --advice` (le daemon doit être démarré).

---

## ## 11. Où sont stockés les modèles


- **Modèles d'embeddings** (mémoire long terme) : dans le data_dir, sous `embedding_model/` (sous-dossiers type `models--<org>--<nom>/`).
- **Modèles LLM embarqués** :
  - **GGUF (llama-cpp, zip CUDA)** : `{data_dir}/models/embedded/*.gguf` (défaut `default.gguf`). Téléchargement : `akasha config models embedded-download` ou assistant UI.
  - **Candle (zip CPU)** : poids SafeTensors via Hugging Face au premier chargement — cache `~/.cache/huggingface/hub` (redirigeable avec **HF_HOME**).
  - Statut : `akasha doctor --json` (champ `action: embedded-download` si GGUF manquant), commande chat `/embedded`, wizard premier lancement.

---
