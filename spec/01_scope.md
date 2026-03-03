# Scope

## Inclus dans la v1

### Core Runtime
- [x] Agent architecture (Main Agent, Orchestrator, Specialized Agents)
- [x] Event bus interne (messages entre agents)
- [x] Watchdog et self-healing
- [x] Redémarrage automatique sur crash
- [x] Interface locale Tauri (chat texte)

### Sécurité
- [x] Vault local pour secrets (OS Keychain + fallback chiffré)
- [x] RBAC/ABAC enforcement
- [x] Prompt injection protection
- [x] Output redaction (anti secret leakage)
- [x] Logs chiffrés avec redaction automatique

### Mémoire
- [x] Mémoire court terme (session volatile)
- [x] Mémoire long terme (persistante chiffrée AES-256)
- [x] Indexation vectorielle (embeddings)
- [x] SQLite local standalone

### Task Management
- [x] Création et suivi de tâches
- [x] Délégation intelligente
- [x] Progress reporting temps réel
- [x] Reprise après redémarrage

### Plugins (Framework de base)
- [x] Plugin API stable
- [x] Sandbox WASM (Wasmtime)
- [x] Signature et vérification plugins
- [x] Système de permissions déclaratives
- [x] Réputation plugins (scoring + auto-restriction)

### Modèles
- [x] LLM Router intelligent (routing par type de tâche)
- [x] Support multi-provider (OpenAI, Anthropic, OpenRouter, Ollama, Azure, Google AI)
- [x] Routing configurable par type de tâche (code, science, rédaction, etc.)
- [x] Fallback automatique inter-modèles et cross-provider
- [x] Support modèles locaux (Ollama, custom)
- [x] Akasha Core Model (onboarding, diagnostic, validation)
- [x] Métriques et monitoring par modèle (latence, coût, erreurs)

### Canaux
- [x] Interface locale UI (chat)
- [x] 1-2 adaptateurs externes (Slack ou Discord au choix)

### Logging & Audit
- [x] Append-only log immuable (hash chain)
- [x] Persistance événements système

### CLI
- [x] `akasha start/stop`
- [x] `akasha doctor` (diagnostic de base)

---

## Exclu de la v1

### Cluster Mode
- [ ] Multi-node distribué
- [ ] Leader election
- [ ] Réplication state cross-nodes
- [ ] NATS cluster (sera en v2)

### Canaux Avancés
- [ ] WhatsApp (API contraintes)
- [ ] Telegram
- [ ] Microsoft Teams
- [ ] Interface vocale complète (voice input/output)

### Plugins Pré-Installés
- [ ] Marketplace plugins
- [ ] Catalogue officiel
- [ ] Auto-update plugins

### Model Training
- [ ] Fine-tuning Akasha Core Model
- [ ] RAG avancé multi-sources
- [ ] Evaluation framework complet

### Fonctionnalités Avancées
- [ ] Multi-utilisateurs
- [ ] Partage de context inter-agents complexe
- [ ] Workflow automation builder UI

---

## Hypothèses

### Utilisateur
- Utilisateur a des compétences techniques de base (installation logiciel)
- Utilisateur accepte de stocker des données localement
- Utilisateur dispose d'une clé API pour modèle externe OU accepte d'utiliser uniquement le modèle local

### Matériel
- Machine moderne capable de faire tourner un runtime 24/7
- Espace disque disponible pour logs et mémoire (minimum 5 GB)
- RAM suffisante pour le daemon + modèle local si nécessaire (minimum 8 GB recommandé)

### Réseau
- Connexion internet disponible pour appels API externes (optionnel)
- Pas de proxy corporate bloquant pour v1 (sera géré en v2)

### Sécurité
- OS dispose d'un keychain/credential manager fonctionnel
- Utilisateur a les droits d'installer et exécuter des services locaux
- Firewall ne bloque pas les ports nécessaires (localhost principalement)

---

## Contraintes

### OS cible
- **Priorité 1:** Windows 10/11 (64-bit)
- **Priorité 2:** macOS 12+ (Intel + Apple Silicon)
- **Priorité 3:** Linux (Ubuntu 22.04+, distributions majeures)

### Ressources matérielles
- **CPU:** 4 cores minimum (recommandé 6+)
- **RAM:** 8 GB minimum (recommandé 16 GB)
- **Disque:** 5 GB minimum (10 GB recommandé pour logs et mémoire)
- **GPU:** Optionnel (pour modèle local haute performance)

### Connexion internet
- **Requis pour:** Appels API modèles externes, mise à jour système
- **Optionnel:** Système fonctionne 100% offline avec modèle local uniquement
- **Bande passante:** Minimum 1 Mbps (recommandé 5+ Mbps pour confort)

### Performance
- Accusé de réception < 500ms (FR-001)
- Démarrage daemon < 10 secondes
- Healthcheck interval: 5 secondes max
- Redémarrage agent crashé: < 30 secondes

### Sécurité
- Aucun secret jamais stocké en clair
- Chiffrement minimum: AES-256
- Sandbox plugins obligatoire (WASM)
- Logs sensibles obligatoirement redactés

### Compatibilité
- API plugins stable (pas de breaking changes v1.x)
- Migration données garantie entre versions mineures
- Backward compatibility événements système