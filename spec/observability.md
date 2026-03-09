# Observabilité — traces, logs structurés, métriques

Ce document décrit le modèle de traces (spans), le format des logs structurés et les métriques exposées pour diagnostiquer et surveiller le daemon Akasha.

---

## 1. Modèle de traces (spans)

Les spans `tracing` permettent de suivre une requête de bout en bout (request tracing).

### Hiérarchie des spans

- **delegation** : une délégation (demande d'un parent vers un enfant). Champs : `requesting_task_id`, `child_task_id`, `assigned_agent`.
- **task** : traitement d'une tâche par le worker conversation. Champs : `task_id`, `parent_task_id`, `assigned_agent`, `session_id`.
- **llm_call** : un appel au routeur LLM (complétion). Champs : `task_type`, `provider`, `model`. Enfants : events latence, tokens, fallback.

Les spans sont créés dans :

- `run_delegation_handler` (api.rs) : span `delegation` par requête, puis span `task` dans le spawn qui attend la complétion.
- Worker conversation (run_message_via_llm) : span `task` autour du traitement de la tâche.
- Routeur LLM (router.rs) : span `llm_call` autour de chaque `complete` / `complete_stream`.

### Export

Les logs sont émis via le crate `tracing`. Pour un export vers un backend (fichier JSON, stdout JSON, OTLP), configurer un `Subscriber` approprié (ex. `tracing_subscriber::fmt::Layer` avec `json` ou `tracing-opentelemetry`). Les champs structurés (task_id, session_id, provider, latency_ms, error_kind) sont inclus dans les events pour faciliter l'agrégation.

---

## 2. Logs structurés

Pour les événements critiques, utiliser des champs structurés plutôt que du texte libre :

- **task_id** (Uuid ou string) : identifiant de la tâche.
- **session_id** (string) : session de conversation.
- **provider**, **model** (string) : provider et modèle LLM.
- **latency_ms** (u64) : latence en millisecondes.
- **error_kind** (string) : type d'erreur (timeout, network, parse, etc.).

Exemple : `tracing::error!(task_id = %id, error_kind = "timeout", "Delegation timeout")`.

---

## 3. Métriques (routeur LLM)

- **ModelMetrics** (par provider/model) : total_requests, successful_requests, failed_requests, total_latency_ms, total_tokens, total_cost_usd, fallback_triggered, fallback_success.
- **Percentiles de latence** : P50, P95, P99 par provider/model (échantillon récent des latences).
- **Endpoint** : `GET /api/metrics/summary` (ou intégré à l'existant) : taux d'erreur par provider, coût par période, P95 latence.

Voir [crates/akasha-llm/src/metrics.rs](../../crates/akasha-llm/src/metrics.rs) et l'API daemon pour les routes exposées.

---

## 4. Cache RAG / runbooks (spécification)

Pour réduire les appels répétés au retrieval RAG (runbooks, spec) :

- **Cache** : en mémoire, par clé `hash(query + pack_id)` (ou chemin du pack).
- **TTL** : 60–300 s (configurable). Au-delà, le résultat est considéré expiré et recalculé.
- **Invalidation** : à la modification du pack (détection du mtime du répertoire ou du fichier index) ou via un webhook si implémenté. Après invalidation, la prochaine requête recalcule et remet en cache.

Comportement à documenter pour éviter les surprises (données obsolètes si le pack change sans redémarrage).

---

## 5. NFR-008 : mise à jour de progression (< 1 s)

La cible est une latence perçue **< 1 s** entre un événement côté daemon (progress, task_completed, pending_human_input) et son affichage dans l'UI (Tauri, TUI). Aujourd'hui les clients utilisent du **polling** (GET /api/tasks/:id, GET pending human-input) avec un intervalle typique de 1–2 s. Pour valider la cible : mesurer l'intervalle de poll et la latence entre émission de l'événement et prochain poll. Si la cible n'est pas atteinte, un mécanisme **SSE** (Server-Sent Events) ou **WebSocket** peut être ajouté pour pousser les événements en temps réel.
