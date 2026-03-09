# API et intégration — infrastructure agentique

Ce document décrit les points d’intégration stables pour utiliser Akasha comme **brique locale** pour construire des solutions agentiques (autre UI, canal personnalisé, scripts, observabilité).

---

## 1. Positionnement

- **Local-first** : le daemon et les modèles (Ollama, embedded) tournent sur la machine de l’utilisateur ; les données restent sous son contrôle.
- **Vault** : secrets (clés API, tokens) stockés de manière sécurisée (data_dir) et exposés aux agents via la config.
- **Politique d’outils** : `tools_policy.yaml` définit chemins autorisés, commandes, domaines web, et option `require_approval` pour les outils sensibles.
- **Multi-canal** : une même instance peut exposer l’API HTTP, la TUI, l’app Tauri, et optionnellement Discord / Slack / Telegram.
- **Événements et traces** : logs structurés (tracing), métriques routeur (latence, coût, fallback), et événements par tâche pour l’observabilité.

---

## 2. Endpoints principaux

| Méthode | Chemin | Description |
|--------|--------|-------------|
| GET | `/` | Santé (status ok) |
| POST | `/api/message` | Envoyer un message (body: message, session_id?, image_data_urls?, priority?) ; priority "high" traite la tâche avant les autres. |
| POST | `/api/tasks/:id/cancel` | Annuler une tâche (Pending/Queued/Running) ; émet task_cancelled. |
| GET | `/api/tasks/:id` | Statut d’une tâche (progress, status, tokens_used, cost_usd). |
| GET | `/api/tasks/:id/events` | Liste des événements (délégation, progression, complétion). |
| GET | `/api/tasks/:id/human-input` | Question en attente (human-in-the-loop). |
| POST | `/api/tasks/:id/human-reply` | Répondre à une question en attente (body: response). |
| GET | `/api/metrics/summary` | Métriques LLM par provider/model (P50/P95/P99 latence, coût). |
| GET | `/api/router/metrics` | Métriques détaillées (option period=day|week|month|year). |
| GET | `/api/update/status` | Dernière version connue (pour bannière de mise à jour). |

Les réponses sont en JSON. Le daemon écoute par défaut sur le port 3876 (`AKASHA_PORT`).

---

## 3. Modèle d’événements

Les événements sont exposés via **GET /api/tasks/:id/events** et (en interne) via le bus d’événements. Chaque événement a un `event_type` et un `payload` optionnel. Types principaux : `user_request_received`, `task_created`, `progress_update`, `sub_agent_spawned`, `task_completed`, `task_failed`, `task_waiting_user_input`, `tool_invoked`, `task_escalated_to_human`, `task_cancelled`. Explicabilité : `tool_invoked` peut inclure `explanation` (null si non fourni) ; `sub_agent_spawned` peut inclure `delegation_reason` (null si non fourni).

Pour un export ou une intégration (dashboard, audit), on peut s’appuyer sur les champs structurés des logs (tracing) et sur les métriques exposées par l’API.

---

## 4. Intégration typique

- **Autre UI** : appeler `POST /api/message`, puis poller `GET /api/tasks/:id` et `/api/tasks/:id/events` pour afficher la progression et les questions en attente ; soumettre les réponses avec `POST /api/tasks/:id/human-reply`.
- **Canal personnalisé** (bot, script) : même flux ; le « client » peut être un script (curl, Python, etc.) qui envoie le message et attend la complétion (ou écoute les événements si un mécanisme temps réel est ajouté).
- **Observabilité** : consommer `GET /api/metrics/summary` et les logs (tracing) pour tableaux de bord latence / coût / erreurs.

---

## 5. Authentification et sécurité CSRF

L’API du daemon écoute uniquement sur `127.0.0.1` (localhost). Pour renforcer la sécurité :

- **Protection CSRF** : le daemon valide l’en-tête `Origin` sur toutes les requêtes mutantes (POST/PUT/DELETE/PATCH). Toute requête dont l’Origin n’est pas une origine locale (`localhost`, `127.0.0.1`, `tauri://`, `https://tauri.localhost`) est rejetée avec `403 Forbidden`. Cela bloque les attaques CSRF depuis des pages web malveillantes ouvertes dans le navigateur de l’utilisateur. Les clients non-navigateur (curl, scripts) n’envoient pas d’en-tête `Origin` et ne sont pas affectés.

- **Exposition sur le réseau** : si le daemon est exposé hors de localhost, il est recommandé de placer un reverse proxy avec authentification (nginx + basic auth ou OAuth) devant le daemon. Une couche d’authentification forte (jeton aléatoire dans un header personnalisé) peut également être ajoutée dans une évolution future.
