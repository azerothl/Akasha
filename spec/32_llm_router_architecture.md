# Architecture du LLM Router

## Vue d'ensemble

Le **LLM Router** est un composant central d'Akasha qui permet de :
- Router intelligemment les requêtes vers le modèle optimal
- Gérer plusieurs providers LLM simultanément
- Assurer la résilience via des fallbacks automatiques
- Optimiser coûts et performance

## Architecture Technique

```
┌────────────────────────────────────────────────────────────────┐
│                       Akasha Agent Layer                        │
└────────────────────────────────────────────────────────────────┘
                              ↓
┌────────────────────────────────────────────────────────────────┐
│                    Task Type Classifier                         │
│  (analyse la requête → détermine le task_type)                 │
└────────────────────────────────────────────────────────────────┘
                              ↓
┌────────────────────────────────────────────────────────────────┐
│                       LLM Router Core                           │
│                                                                 │
│  ┌──────────────────┐  ┌──────────────────┐                   │
│  │ Routing Config   │  │ Fallback Engine  │                   │
│  │ Manager          │  │                  │                   │
│  └──────────────────┘  └──────────────────┘                   │
│                                                                 │
│  ┌──────────────────┐  ┌──────────────────┐                   │
│  │ Provider Manager │  │ Metrics Collector│                   │
│  └──────────────────┘  └──────────────────┘                   │
└────────────────────────────────────────────────────────────────┘
                              ↓
         ┌────────────────────┴───────────────────┐
         ↓                    ↓                    ↓
┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐
│ Cloud Providers │  │ Local Providers │  │ Akasha Core     │
│                 │  │                 │  │                 │
│ • OpenAI        │  │ • Ollama        │  │ • Core Model    │
│ • Anthropic     │  │ • Local Custom  │  │ • Diagnostic    │
│ • OpenRouter    │  │                 │  │                 │
│ • Azure OpenAI  │  │                 │  │                 │
│ • Google AI     │  │                 │  │                 │
└─────────────────┘  └─────────────────┘  └─────────────────┘
```

## Composants

### 1. Task Type Classifier

Analyse la requête utilisateur et détermine le type de tâche.

**Classification basée sur:**
- Analyse du prompt (keywords, structure)
- Contexte conversationnel
- Métadonnées de la requête
- Hints utilisateur explicites

**Output:**
```json
{
  "task_type": "code_generation",
  "confidence": 0.95,
  "alternative_types": ["data_analysis"]
}
```

**Routage forcé (`preferred_task_type`)** : la `CompletionRequest` peut contenir un champ optionnel `preferred_task_type` (ex. `"system"`). Lorsqu’il est renseigné, le routeur **ne classe pas** le prompt et utilise directement cette catégorie.  
Pour la décomposition orchestrateur, la priorité est désormais `preferred_task_type: "orchestrator"` quand cette route existe dans `llm_router.yaml`; sinon fallback automatique sur `"system"` (compatibilité ascendante).

**Résolution par type d'agent (fallback vers rôle proche)** : lorsqu'une tâche a un `assigned_agent` (ex. `financial`, `documentalist`), le daemon appelle `resolve_task_type_for_agent(assigned_agent)` pour obtenir le `task_type` de routage. Si ce type n'a pas de route « custom » (primary différent de `akasha_embedded` et `akasha_core`), le routeur essaie les task_types « proches » dans l'ordre (ex. `financial` → `data_analysis`, puis `conversation`) et utilise le premier qui a une route custom. Sinon, le task_type de l'agent est conservé (route par défaut interne). Ainsi, un agent sans modèle dédié peut utiliser le modèle d'un agent au rôle proche configuré dans `llm_router.yaml`. Voir [33_agents_tools_orchestrator_skills.md](33_agents_tools_orchestrator_skills.md) § 5.4 pour la liste des types d'agents.

### 2. Routing Config Manager

Gère la configuration du routing pour chaque type de tâche.

**Responsabilités:**
- Chargement de la config utilisateur
- Validation des routes
- Override dynamique si nécessaire
- Cache des configurations actives

**Schéma de configuration (extrait pour un type de tâche).** Fichier complet : [llm_router.example.yaml](llm_router.example.yaml). Référence détaillée : [35_configuration_reference.md](35_configuration_reference.md).

```yaml
# Racine : global, providers, model_options, task_types
task_types:
  code_generation:
    primary:
      provider: anthropic
      model: claude-3.5-sonnet-20241022
      config: {}   # optionnel, objet libre (max_tokens, temperature, etc.)
    fallback:      # liste d'entrées (clé réelle = fallback, pas fallback_chain)
      - provider: openai
        model: gpt-4-turbo
      - provider: ollama
        model: deepseek-coder:33b
    constraints:   # optionnel
      max_cost_per_request: 0.05   # nombre (USD)
      max_latency_secs: 30         # entier (secondes)
```

### 3. Provider Manager

Interface unifiée pour tous les providers LLM.

**Architecture Provider:**
```rust
trait LLMProvider {
    fn name(&self) -> String;
    fn is_available(&self) -> bool;
    fn list_models(&self) -> Vec<ModelInfo>;
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse>;
    fn get_cost(&self, usage: TokenUsage) -> f64;
}
```

**Providers implémentés:**

#### OpenAI Provider
```rust
struct OpenAIProvider {
    api_key: SecretRef,
    base_url: String,
    organization: Option<String>,
}
```

#### Anthropic Provider
```rust
struct AnthropicProvider {
    api_key: SecretRef,
    version: String,
}
```

#### OpenRouter Provider
```rust
struct OpenRouterProvider {
    api_key: SecretRef,
    site_url: Option<String>,
    app_name: Option<String>,
}
```

#### Ollama Provider
```rust
struct OllamaProvider {
    base_url: String,  // Default: http://localhost:11434
    models_cache: Arc<RwLock<Vec<String>>>,
}
```

#### Akasha Core Provider
```rust
struct AkashaCoreProvider {
    model_path: PathBuf,
    runtime: LocalModelRuntime,
}
```
Utilise en priorité le modèle embarqué (Candle) quand la feature `embedded` est activée ; sinon message placeholder.

#### Akasha Embedded Provider
Provider explicite pour le modèle intégré (Qwen3 0.6B ou Baguettotron 321M). `akasha_embedded` / `default`. Voir [34_embedded_small_model.md](34_embedded_small_model.md). Par défaut le routeur utilise ce provider pour tous les types de tâche si aucune config utilisateur n’est définie.

### 4. Fallback Engine

Gère la logique de fallback automatique.

**Algorithme de fallback:**

```
1. Tenter primary_model avec retry (max 2 fois)
   ↓ [échec]
2. Si provider_alternative configuré → tenter
   ↓ [échec]
3. Essayer cross_provider_fallback (autre provider)
   ↓ [échec]
4. Basculer sur local_model (Ollama si dispo)
   ↓ [échec]
5. Fallback final → akasha_core (mode dégradé)
```

**Conditions de fallback:**

| Condition | Description | Action |
|-----------|-------------|--------|
| `ApiError` | Erreur HTTP 5xx | Fallback immédiat |
| `Timeout` | > 10s sans réponse | Fallback immédiat |
| `RateLimit` | 429 Too Many Requests | Fallback + retry après cooldown |
| `ModelUnavailable` | 503 Service Unavailable | Fallback immédiat |
| `CostThreshold` | Budget dépassé | Fallback vers modèle moins cher |
| `AuthError` | 401/403 | Log erreur + notification user |

**Retry Policy:**
```rust
struct RetryPolicy {
    max_attempts: u32,          // Default: 2
    initial_backoff: Duration,  // Default: 1s
    max_backoff: Duration,      // Default: 10s
    backoff_multiplier: f64,    // Default: 2.0
}
```

### 5. Metrics Collector

Collecte les métriques d'utilisation et performance.

**Métriques collectées:**

```rust
struct ModelMetrics {
    // Utilisation
    total_requests: u64,
    successful_requests: u64,
    failed_requests: u64,
    
    // Performance
    avg_latency_ms: f64,
    p95_latency_ms: f64,
    p99_latency_ms: f64,
    
    // Coûts
    total_tokens: u64,
    total_cost_usd: f64,
    
    // Fallbacks
    fallback_triggered: u64,
    fallback_success: u64,
    
    // Timestamps
    last_success: DateTime<Utc>,
    last_failure: DateTime<Utc>,
}
```

**Stockage:**
- Base SQLite locale
- Rétention 90 jours
- Agrégation par jour/semaine/mois

## Flow d'une Requête

```
1. User Request
   ↓
2. Task Type Classifier
   "code_generation" (95% confidence)
   ↓
3. Routing Config Manager
   Load config for "code_generation"
   → Primary: Anthropic Claude 3.5
   ↓
4. Provider Manager
   Get anthropic_provider
   ↓
5. Execute Request
   ┌─→ Success → Return response
   └─→ Failure → Fallback Engine
       ↓
       Try: OpenAI GPT-4
       ┌─→ Success → Return + Log fallback
       └─→ Failure → Fallback Engine
           ↓
           Try: Ollama Deepseek
           → Success → Return + Log fallback
   ↓
6. Metrics Collector
   Log: latency, tokens, cost, fallback_count
   ↓
7. Return to Agent Layer
```

## Configuration Utilisateur

### Interface CLI

```bash
# Lister les providers disponibles
akasha router providers list

# Configurer un modèle pour un type de tâche
akasha router config set code_generation \
  --primary anthropic/claude-3.5-sonnet \
  --fallback openai/gpt-4 \
  --fallback ollama/deepseek-coder

# Voir la configuration actuelle
akasha router config show

# Tester un provider
akasha router test anthropic

# Voir les métriques
akasha router metrics --last-7-days
```

### Interface UI

Akasha UI propose un panneau de configuration visuel:

```
┌─────────────────────────────────────────┐
│  LLM Router Configuration               │
├─────────────────────────────────────────┤
│                                         │
│  Task Type: Code Generation             │
│                                         │
│  Primary Model:                         │
│  [Anthropic ▼] [Claude 3.5 Sonnet ▼]   │
│                                         │
│  Fallback Chain:                        │
│  1. [OpenAI ▼] [GPT-4 Turbo ▼]         │
│  2. [Ollama ▼] [Deepseek Coder ▼]      │
│  3. [Akasha Core (always available)]    │
│                                         │
│  Constraints:                           │
│  Max cost/request: [0.05] USD          │
│  Max latency: [30] seconds             │
│                                         │
│  [Save Configuration]                   │
└─────────────────────────────────────────┘
```

### Fichier de Configuration

`~/.akasha/config/llm_router.yaml`

```yaml
version: "1.0"

# Configuration globale (clés réelles : default_timeout_secs, default_max_retries)
global:
  enable_metrics: true
  enable_fallback: true
  default_timeout_secs: 300
  default_max_retries: 2

# Configuration par type de tâche
task_types:
  code_generation:
    primary:
      provider: anthropic
      model: claude-3.5-sonnet-20241022
      config:
        max_tokens: 4096
        temperature: 0.7
    fallback:
      - provider: openai
        model: gpt-4-turbo
      - provider: ollama
        model: deepseek-coder:33b
    constraints:
      max_cost_per_request: 0.05
      max_latency_secs: 30

  creative_writing:
    primary:
      provider: anthropic
      model: claude-3-opus-20240229
      config:
        max_tokens: 8192
        temperature: 0.9
    fallback:
      - provider: openai
        model: gpt-4
      - provider: ollama
        model: llama3:70b

  scientific_analysis:
    primary:
      provider: openai
      model: gpt-4-turbo
    fallback:
      - provider: anthropic
        model: claude-3-opus
      - provider: google_ai
        model: gemini-1.5-pro

  conversation:
    primary:
      provider: openrouter
      model: anthropic/claude-3.5-sonnet
    fallback:
      - provider: ollama
        model: llama3:8b
      - provider: akasha_core
        model: core

# Configuration des providers
providers:
  openai:
    api_key_ref: vault://openai_api_key
    organization: null
    base_url: https://api.openai.com/v1

  anthropic:
    api_key_ref: vault://anthropic_api_key
    version: "2023-06-01"

  openrouter:
    api_key_ref: vault://openrouter_api_key
    site_url: https://akasha.local
    app_name: Akasha

  ollama:
    base_url: http://localhost:11434
    auto_pull_models: true

  akasha_core:
    model_path: ~/.akasha/models/core
    always_available: true
```

## Sécurité

### Gestion des Clés API

- ✅ Stockage dans le vault chiffré
- ✅ Jamais exposées dans les logs
- ✅ Rotation supportée
- ✅ Permissions ABAC par provider

### Isolation

- Chaque provider s'exécute avec des permissions limitées
- Pas d'accès direct au vault (passé par référence)
- Logs redactés automatiquement
- Timeout strict pour éviter blocage

## Performance

### Optimisations

- **Connection pooling** - Réutilisation connexions HTTP
- **Request batching** - Groupage requêtes quand supporté
- **Streaming** - Support streaming responses
- **Caching** - Cache embeddings et réponses similaires (optionnel)

### Benchmarks Attendus

| Provider | Latency p50 | Latency p95 | Cost/1M tokens |
|----------|-------------|-------------|----------------|
| OpenAI GPT-4 | 2-3s | 5-8s | $30 |
| Anthropic Claude 3.5 | 1-2s | 3-5s | $15 |
| Ollama (local) | 500ms-2s | 2-5s | $0 |
| Akasha Core | 200-500ms | 1s | $0 |

## Évolutions Futures

### Phase 1 (v1)
- ✅ Routing par type de tâche
- ✅ Multi-provider
- ✅ Fallback automatique
- ✅ Métriques de base

### Phase 2 (v1.5)
- [ ] Auto-learning des préférences
- [ ] Recommandations d'optimisation
- [ ] A/B testing automatique
- [ ] Cost optimization engine

### Phase 3 (v2)
- [ ] Fine-tuning adapters par tâche
- [ ] Multi-model ensemble
- [ ] Distributed routing (cluster mode)
- [ ] Advanced caching strategies