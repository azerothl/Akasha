# Vérification backend : parité « pi-ai / pi-agent » (streaming outils, tokens, coût)

Vérification effectuée sur le monorepo **Akasha** (crates `akasha-llm`, `akasha-daemon`) pour répondre au plan d’analyse **pi-mono** : *toolcall_delta*, métriques **tokens / coût** exposées aux UIs.

## Synthèse

| Capacité (réf. Pi) | État Akasha | Notes |
|--------------------|------------|--------|
| **`toolcall_delta`** (arguments JSON partiels en streaming) | **Non** | Pas de flux NDJSON/tool-call partiel vers le client. |
| **Appels d’outil** après génération | **Oui** | Parsing du texte assistant (`parse_tool_calls`) puis exécution ; boucle conversation dans `api.rs`. |
| **Tokens / coût agrégés par tâche** | **Oui** | `GET /api/tasks/:id` expose `tokens_used` et `cost_usd`. |
| **Streaming texte LLM** (chunks vers le worker) | **Partiel** | `akasha-llm` : `complete_stream` envoie des chunks **texte** via `chunk_tx` ; l’UI consomme surtout **progress** / réponse finale, pas un flux token-identique à pi-ai. |

## Détail : streaming des tool calls

- **Pi** (`@mariozechner/pi-ai`) : événements `toolcall_start` / `toolcall_delta` / `toolcall_end` pendant la réponse du modèle.
- **Akasha** : le routeur et les providers peuvent streamer du **texte** ; les **outils** au format `TOOL: ...` sont en général **extraits après** coup du texte complet de l’assistant via `parse_tool_calls` (voir `crates/akasha-daemon/src/api.rs`, chemins autour de `run_message_via_llm` et commentaires « parse_tool_calls »).
- **Conséquence** : les UIs **ne peuvent pas** aujourd’hui afficher l’équivalent d’un *toolcall_delta* sans extension du provider + du contrat de progression.

## Détail : tokens et coût (exposition API)

- **`TaskUsageStore`** (`api.rs`) : accumule par `task_id` et par `session_id` des totaux `(tokens, cost_usd)` ; alimenté après complétions LLM quand `resp` fournit tokens / `cost_usd`.
- **`get_task_status`** : corps JSON de `GET /api/tasks/:id` inclut :
  - `"tokens_used": …`
  - `"cost_usd": …`
- Les clients (Tauri `akasha-ui`, Code Studio) peuvent les afficher s’ils les lisent depuis la réponse tâche ; ce n’est **pas** le même mécanisme qu’un footer temps réel par tour comme dans le TUI `pi`.

## Détail : événements tâche (`/api/tasks/:id/events`)

- Le bus interne définit `ToolCallStarted` / `ToolCallFinished` dans `akasha-core` (`crates/akasha-core/src/events.rs`).
- La présence dans la **timeline API** dépend de l’**émission** effective depuis la boucle conversation / orchestrateur ; à traiter comme **enrichissement** si l’on veut la parité fine avec `tool_execution_*` Pi (voir [agent_client_event_contract.md](../runtime/agent_client_event_contract.md)).

## Recommandations (hors scope de ce document)

1. Si *toolcall_delta* devient une exigence : étendre le(s) provider(s) qui supportent les tool calls natifs streaming, puis faire remonter des événements normalisés dans `ProgressUpdate` ou le journal d’événements.
2. Exposer **usage du dernier tour** (tokens in/out, coût) dans `GET /api/tasks/:id` ou dans un événement dédié pour un footer type Pi sans multiplier les polls.
