# Blueprint d'Implémentation

## Phase 0 — Foundations (1–2 semaines)

* Repo mono + conventions
* Spec loader (parse YAML + validate)
* Event envelope standard
* CLI “akasha doctor” (diagnostic minimal)
* Skeleton Tauri UI (chat text)

Livrables

* Runtime minimal qui démarre/stop
* Healthcheck + log baseline

## Phase 1 — Core runtime 24/7 + self-healing (2–4 semaines)

* Watchdog (heartbeat, restart policies)
* Process isolation des agents
* Crash loop detection
* State persistence minimal (tasks in SQLite)
* Append-only log v1 (hash chain)

Critères

* Akasha redémarre seul après crash
* Aucune perte “task list” après restart

## Phase 2 — Agents v1 + Task system (2–4 semaines)

* Main Agent : ack + status + routing
* Orchestrator : decomposition basique
* Worker agent framework (process boundaries)
* Progress events temps réel

Critères

* Une tâche multi-étapes est suivie, reprise, terminée

## Phase 3 — Security hardening (2–5 semaines)

* Vault local (OS keychain + fallback encrypted file)
* RBAC/ABAC enforcement
* Output redaction (anti secret leakage)
* Prompt injection guardrails
* Plugin signing + trust store

Critères

* Aucun secret ne sort même sous attaque “prompt injection”
* Plugins non signés refusés

## Phase 4 — Multi-canal (2–6 semaines, incrémental)

* Slack adapter (MVP)
* Discord adapter
* Teams adapter
* Telegram adapter
* WhatsApp (selon contraintes API)

Critères

* Un même task_id est visible et pilotable depuis plusieurs canaux

## Phase 5 — Plugin framework (3–6 semaines)

* Plugin API stable (channel/tool/skill/memory/model/security)
* WASM sandbox (Wasmtime)
* Plugin reputation v1 (score + auto restrict)
* Marketplace local (catalog)

Critères

* Installer/désinstaller un plugin sans redémarrage complet
* Désactivation auto si plugin flanche

## Phase 6 — LLM Router + Model strategy + fallback + degraded mode (4–8 semaines)

### 6.1 — LLM Router Core (2–3 semaines)

* Task Type Classifier (6 types de tâches)
* Provider Manager (abstraction multi-provider)
* Routing Config Manager (configuration par tâche)
* Métriques de base (latence, coût, erreurs)

**Providers Phase 6.1:**
- OpenAI provider
- Anthropic provider
- Ollama provider (local)
- Akasha Core provider

### 6.2 — Fallback Engine (1–2 semaines)

* Fallback automatique à 5 niveaux
* Détection conditions (timeout, API down, rate limit, cost threshold)
* Retry policy configurable
* Cooldown et retry automatique
* Logging des switches

### 6.3 — Providers Additionnels (1–2 semaines)

* OpenRouter provider
* Azure OpenAI provider
* Google AI provider
* Configuration multi-tenancy par provider

### 6.4 — Mode Dégradé (1 semaine)

* Mode dégradé sécurisé (disable external calls, restrict plugins)
* Offline-first flows
* Basculement automatique sur modèles locaux uniquement

Critères

* Routing intelligent fonctionnel pour les 6 types de tâches
* Fallback automatique entre providers testée
* Configuration utilisateur (CLI + UI) opérationnelle
* Métriques collectées et visualisables
* Continuité de service sans internet
* Notifications claires utilisateur sur changement de modèle

## Phase 7 — Cluster mode (4–8 semaines)

* NATS cluster + mTLS
* Leader election
* Scheduling multi-nodes
* Réplication state/memory + log consistency
* Partition tolerance (mode dégradé si split brain)

Critères

* Perte d’un node sans perte de tâches
* Élection leader automatique

## Phase 8 — Akasha Core Model (continu, mais MVP en 2–4 semaines)

* RAG pack (spec + runbooks + API)
* “akasha doctor” branché sur core model
* Suite d’évals (security, hallucinations, runbooks)
* Fine-tune léger si nécessaire

Critères

* Onboarding guidé
* Diagnostics fiables
* Ne propose jamais d’action risquée sans garde-fous