# Architecture Agentique

## 1️⃣ Main Agent (User Interface Agent)

Rôle:
Point d’entrée unique.

Responsabilités:
- Réception messages
- Accusé réception
- Classification tâche
- Reporting global
- Interaction mémoire court terme

Accès:
- Mémoire court terme
- Mémoire long terme (lecture)
- Pas d’accès secrets en clair

---

## 2️⃣ Orchestrator Agent (Project Manager Agent)

Rôle:
Décomposition et orchestration.

Responsabilités:
- Analyse tâche
- Découpage sous-tâches
- Spawn sous-agents
- Agrégation résultats

Accès:
- Lecture mémoire long terme
- Pas accès secrets directs

---

## 3️⃣ Specialized Agents

Exemples:
- API Agent
- Code Agent
- Research Agent
- Automation Agent

Responsabilités:
- Exécution tâche spécifique
- Reporting progress

Accès:
- Limité et scoped
- Secrets uniquement si explicitement autorisé