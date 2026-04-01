# Functional Requirements

> **Spécification détaillée** — une section par FR avec description et critères. Résumé liste seule : [01_functional_requirements.md](01_functional_requirements.md). *Nom de fichier : orthographe historique « fonctional ».*

FR-001 — Accusé de réception immédiat
Description:
> Toute requête utilisateur doit recevoir un accusé de réception en moins de 500ms.

Critères:
- Temps < 500ms
- Message clair confirmant prise en charge

---

FR-002 — Délégation intelligente
Description:
> Si une tâche dépasse les capacités conversationnelles du main agent, elle doit être déléguée à un agent spécialisé.

Critères:
- Classification automatique de tâche
- Création d’un task_id
- Notification utilisateur

---

FR-003 — Reporting temps réel
Description:
> L’utilisateur doit être informé de l’avancement d’une tâche.

Critères:
- Événement progress_update déclenché
- UI mise à jour en temps réel

---

FR-004 — Mémoire court terme contextuelle
Description:
> Maintenir le contexte conversationnel par session.

---

FR-005 — Mémoire long terme persistante
Description:
> Conserver les informations importantes entre sessions.

Contraintes:
- Chiffrement obligatoire
- Indexation sémantique

---

FR-006 — Protection des secrets
Description:
> Aucun secret ne doit être affiché en clair.

Critères:
- Secrets accessibles uniquement par agents autorisés
- Logs ne contiennent aucun secret

---

FR-007 — Auto-redémarrage sur crash
Description:
> Si un agent crashe, il doit redémarrer automatiquement sans intervention manuelle.

Critères:
- Détection crash < 5 secondes
- Redémarrage automatique avec limite configurable (max 5 tentatives)
- Isolation de l'agent défaillant si échecs répétés

---

FR-008 — Reprise d'état après redémarrage
Description:
> Les tâches en cours doivent être restaurées après un redémarrage du système.

Critères:
- État des tâches persisté en temps réel
- Restauration automatique au démarrage
- Notification utilisateur des tâches reprises

---

FR-009 — Multi-canal unifié
Description:
> L'utilisateur doit pouvoir interagir via plusieurs canaux avec le même contexte.

Critères:
- Un task_id reste valide sur tous les canaux
- Changement de canal sans perte de contexte
- Notifications synchronisées

---

FR-010 — Installation et gestion de plugins
Description:
> L'utilisateur peut installer, désactiver et désinstaller des plugins.

Critères:
- Installation sans redémarrage complet du système
- Vérification signature obligatoire
- Désactivation automatique si plugin défaillant

---

FR-011 — Mode dégradé automatique
Description:
> En cas de perte de ressource critique, le système passe en mode dégradé sécurisé.

Critères:
- Activation automatique selon conditions prédéfinies
- Désactivation plugins non essentiels
- Blocage appels API externes
- Notification claire utilisateur du mode actif

---

FR-012 — Fallback modèle automatique
Description:
> Si le modèle principal est indisponible, basculer automatiquement vers un modèle de secours.

Critères:
- Détection indisponibilité < 10 secondes
- Bascule transparente vers modèle local
- Notification utilisateur du changement
- Retour automatique quand modèle principal disponible

---

FR-013 — Logging immuable et auditable
Description:
> Toutes les actions critiques doivent être tracées dans un journal immuable.

Critères:
- Append-only log avec hash chain
- Impossibilité de supprimer ou modifier des entrées
- Vérification intégrité au démarrage
- Redaction automatique des secrets

---

FR-014 — Sandbox des plugins
Description:
> Les plugins doivent s'exécuter dans un environnement isolé et sécurisé.

Critères:
- Isolation WASM (Wasmtime)
- Permissions déclaratives requises
- Pas d'accès réseau par défaut
- Crash plugin n'affecte pas le core

---

FR-015 — Système de réputation plugins
Description:
> Les plugins doivent avoir un score de réputation basé sur leur comportement.

Critères:
- Scoring automatique (erreurs, ressources, sécurité)
- Restriction automatique si score faible
- Désactivation automatique si score critique
- Visibilité du score pour l'utilisateur

---

FR-016 — Diagnostic et self-healing
Description:
> Le système doit pouvoir diagnostiquer et corriger automatiquement les problèmes courants.

Critères:
- Détection anomalies (mémoire, CPU, erreurs répétées)
- Tentative auto-fix avant escalade
- Nettoyage mémoire corrompue
- Rechargement plugins défaillants

---

FR-017 — CLI de diagnostic
Description:
> Un outil CLI `akasha doctor` doit permettre de diagnostiquer l'état du système.

Critères:
- Vérification santé de tous les composants
- Détection problèmes configuration
- Recommandations de corrections
- Export rapport diagnostic

---

FR-018 — Onboarding guidé
Description:
> Le premier démarrage doit guider l'utilisateur dans la configuration initiale.

Critères:
- Assistant pas-à-pas piloté par Akasha Core Model
- Configuration vault et secrets
- Test connexion modèle externe (optionnel)
- Validation installation complète

---

FR-019 — Permissions granulaires
Description:
> Chaque agent et plugin doit avoir des permissions explicites et limitées.

Critères:
- RBAC pour rôles (main, orchestrator, plugin, etc.)
- ABAC pour scopes dynamiques
- Permissions immuables au runtime
- Audit trail des accès

---

FR-020 — Prompt injection protection
Description:
> Le système doit détecter et bloquer les tentatives d'injection malveillantes.

Critères:
- Filtrage instructions système dans input utilisateur
- Validation commandes critiques
- Séparation stricte user input / system prompts
- Logging tentatives détectées

---

FR-021 — LLM Router avec routing par type de tâche
Description:
> Le système doit router intelligemment les requêtes vers le modèle optimal selon le type de tâche.

Critères:
- Classification automatique du type de tâche (code, science, rédaction, etc.)
- Configuration utilisateur par type de tâche
- Support routing vers différents modèles selon contexte
- Métriques de performance par modèle/tâche

---

FR-022 — Support multi-provider LLM
Description:
> Le système doit supporter plusieurs providers LLM simultanément.

Critères:
- Support OpenAI, Anthropic, OpenRouter, Ollama, Azure, Google AI
- Configuration flexible par provider
- Gestion centralisée des clés API dans le vault
- Isolation des erreurs par provider

---

FR-023 — Fallback automatique inter-modèles
Description:
> En cas d'indisponibilité d'un modèle, basculer automatiquement vers un fallback.

Critères:
- Chaîne de fallback configurable par type de tâche
- Détection automatique des erreurs (timeout, API down, rate limit)
- Fallback cross-provider (ex: Claude → GPT-4 → Ollama)
- Notification utilisateur du changement
- Retour automatique au modèle primary quand disponible
- Maximum 2 tentatives avant fallback définitif

---

FR-024 — Monitoring et métriques des modèles
Description:
> Le système doit collecter des métriques sur l'utilisation et performance des modèles.

Critères:
- Tracking latence par modèle
- Tracking coûts par provider/modèle
- Taux de succès/erreur par modèle
- Nombre de fallbacks déclenchés
- Dashboard utilisateur avec visualisation des métriques

---

FR-025 — Chat non bloquant
Description:
> Une tâche longue ne doit jamais bloquer l'interface de discussion. L'utilisateur peut continuer à converser pendant l'exécution des sous-agents.

Critères d'acceptation:
- Le main agent envoie un ACK immédiat + task_id
- La conversation reste fluide pendant l'exécution
- Aucun "freeze" UI lors de tâches longues

---

FR-026 — Onglet « Tâches » (Task Center)
Description:
> L'utilisateur dispose d'un onglet dédié affichant en temps réel l'état, la progression, les logs et les résultats des tâches.

Critères d'acceptation:
- Liste des tâches (en cours, terminées, échouées)
- Détails par tâche: timeline d'événements + % + sous-tâches + erreurs
- Actions: pause / reprendre / annuler / relancer (selon permissions)

---

FR-027 — Suivi temps réel (streaming)
Description:
> Les mises à jour d'état/progression d'une tâche doivent être visibles en temps réel dans l'onglet Tâches.

Critères d'acceptation:
- progress_update reçu en streaming
- UI mise à jour sans refresh

---

FR-028 — Tâches récurrentes
Description:
> L'utilisateur peut planifier une tâche récurrente (cron-like) et suivre l'état en temps réel de chaque occurrence (run).

Critères d'acceptation:
- Création/édition/suppression d'une récurrence
- Chaque exécution génère un run_id et un task_id
- Historique des runs consultable

---

FR-029 — Onglet « Calendrier »
Description:
> Un onglet calendrier affiche toutes les récurrences et occurrences planifiées. L'utilisateur peut modifier ou supprimer les tâches planifiées.

Critères d'acceptation:
- Vue mois/semaine/jour
- Click -> détails récurrence
- Edit / delete / pause / exceptions (skip une occurrence)