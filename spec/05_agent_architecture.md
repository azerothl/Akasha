# Architecture Agentique

## 0️⃣ Superviseur et modes d’exécution

Toute requête passe par un point d’entrée unique (Main Agent) qui applique un **classifieur de complexité** et choisit le mode :

- **Direct** : une intention, une sortie, pas de coordination (météo, traduction, etc.). Tâche envoyée directement au worker conversation, sans orchestrateur.
- **Guidé** : sous-parties modérées, structuration ou vérification utile (plan, diagnostic). Passage par l’orchestrateur, décomposition légère.
- **Orchestré** : projet multi-étapes, plusieurs livrables, dépendances, QA. Pipeline à états (Cadrage → Planification → Production → Vérification → Consolidation → Validation → Livraison), persistance d’état et backlog.

---

## 1️⃣ Main Agent (User Interface Agent)

Rôle:
Point d’entrée unique.

Responsabilités:
- Réception messages
- Accusé réception
- Classification complexité (superviseur) et choix du mode (Direct / Guidé / Orchestré)
- Création tâche et routage (conversation directe ou orchestrateur)
- Reporting global
- Interaction mémoire court terme

Accès:
- Mémoire court terme
- Mémoire long terme (lecture)
- Pas d’accès secrets en clair

---

## 2️⃣ Orchestrator Agent (Chef de projet)

Rôle:
Comprendre la demande, transformer en plan exécutable, découper en sous-tâches, affecter les agents, vérifier l’avancement, relancer tant que les critères de fin ne sont pas atteints.

Responsabilités:
- Analyse et décomposition (LLM)
- Affectation des sous-tâches aux agents spécialisés
- Agrégation et synthèse des réponses
- Vérification de satisfaction ; si besoin, un tour de raffinement (avec limite d’escalade)
- En mode Orchestré : pilotage du pipeline à états (persistance `pipeline_context`), transition vers Livraison à la fin

Accès:
- Lecture mémoire long terme
- Pipeline store (état, attempt_count)
- Pas accès secrets directs

---

## 3️⃣ Agents spécialisés

Roster (décomposeur, prompts rôle, router LLM) :

- **Conversation** : répond à l’utilisateur, utilise les outils (web_search, write_file, run_command, etc.).
- **Analyste / Product** : formalise le besoin (périmètre, critères d’acceptation, backlog initial).
- **Architecte** : conçoit le squelette (architecture, tâches, dépendances, définition of done).
- **Production** : frontend, backend, database, integration, image_generation — livrable précis par agent.
- **QA** : contrôle qualité ; vérifie cohérence, couverture, fichiers manquants, TODO ; ne valide pas si critères incomplets.
- **Documentaliste** : transforme un « tas de fichiers » en donnée exploitable (RAG, mémoire).
- **Créatif** : texte et image, sens créatif.
- **Système** : connaissance complète de l’application Akasha, dépannage et informations.
- **Code, search, financial, project_manager, technical_writer, research, security_audit** : rôles existants.

Contrat de sortie (mode guidé/orchestré) : les agents peuvent produire un bloc JSON en fin de réponse (`status`, `summary`, `files_created`, `issues_found`, `blocked` avec cause/impact/workaround). L’orchestrateur exploite ce contrat (détection de blocage, synthèse).

Prompt de tâche (couche 3) : pour analyste, architecte, frontend, backend, database, integration, qa, image_generation, le message envoyé est enveloppé dans une instruction structurée (objectif, critères de réussite, format de sortie).

Accès:
- Limité et scoped
- Secrets uniquement si explicitement autorisé (ex. VAULT: dans run_command)