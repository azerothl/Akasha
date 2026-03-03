# Proposition de stack technique

## A. Langages et composants

Core runtime (daemon 24/7 + supervision + bus + permissions)

Rust (recommandé) : perf, sécurité mémoire, binaire unique, idéal pour service 24/7 + sandboxing + crypto.

Alternative : Go si tu veux itérer plus vite côté réseau/concurrence (moins “hard-secure” que Rust mais très solide).

Orchestration agentique + logique produit

TypeScript (Node/Bun) ou Elixir (si tu veux du “télécom-grade”).

Mon choix :

Rust Core (supervision, bus, vault, log immuable, plugin host)

TypeScript (agent logic, skills, plugins, UI adapters)

Ça sépare “cœur critique” et “logique évolutive”.

UI / Desktop

Tauri (Rust + front React/TS) : léger, secure, local-first, parfait pour Akasha.

Alternative : Electron (plus lourd).

## B. Bus d’événements et orchestration

Interne single-machine

Bus événementiel in-process (Rust) + NATS embedded ou simple event-bus maison (channels + persistence).

Cluster multi-machines

NATS (top pour pub/sub + request/reply + simplicité cluster)

Alternative : Kafka (gros, costaud, mais plus lourd à opérer — utile si tu veux des garanties fortes à grande échelle)

👉 Reco Akasha : NATS (cluster local réseau, super latence, très bon fit “agent swarm”).

## C. Base de données + mémoire

Métadonnées / tâches / état / config

SQLite en standalone (simple, local, robuste)

PostgreSQL en cluster (réplication, concurrence, robustesse)

Mémoire sémantique (vector + full-text)

PostgreSQL + pgvector (cluster-friendly, unifie tout)

Full-text : Postgres FTS (ou SQLite FTS5 en mode local simple)

Append-only log immuable

Option 1 (simple et robuste) : WAL-like log dans des segments fichiers + hash chain + signatures

Option 2 : Postgres “event_store” + hash chain (bien mais dépend DB)

Option 3 : RocksDB (rapide, plus complexe)

👉 Reco Akasha :

Standalone : SQLite (state) + fichiers segments append-only (audit log)

Cluster : Postgres+pgvector + append-only log répliqué (ou “log leader + replicate”).

## D. Crypto + secrets

Chiffrement data-at-rest

AES-256-GCM pour données (confidentialité + intégrité)

Argon2id pour dériver la clé depuis mot de passe utilisateur (ou clé OS)

Vault local

Intégration OS :

Windows Credential Manager

macOS Keychain

Linux Secret Service / GNOME Keyring

Sinon : Vault maison chiffré (fichier) + rotation + “sealed by default”

Identité & signatures

Ed25519 pour signatures (plugins, log immuable, releases)

Certificat par plugin + “trust store” Akasha

Sécurité inter-nodes

mTLS (certs) + ACL NATS

Clés/Certs gérés par Akasha Vault (ou provisionnés)

## E. Sandboxing / isolation / permissions

Sandbox plugins

WASM (Wasmtime) : super approche “secure plugin runtime”

Alternative : conteneurs (Docker) mais trop lourd en local

Sandbox agents

Process isolation : agents spécialisés en processus séparés (limite crash contagieux)

Réseau “deny by default” (capability-based)

RBAC/ABAC

RBAC pour rôles (main/orchestrator/plugin/agent)

ABAC pour scopes dynamiques (ex: “peut appeler Slack uniquement pour ce workspace”)

## F. Clustering & HA

Leader election

Via NATS JetStream / KV store, ou algo Raft minimal (si tu veux full contrôle)

Supervision : watchdog local + heartbeat cluster

Répartition

Coordinator node : scheduling + state authority

Worker nodes : exécution agents

Tolérance de panne

Reprise tasks via event log + checkpoints

Mode dégradé automatique si quorum perdu