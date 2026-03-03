# Personas

## Persona 1 — Alex, le Développeur Autonome

### Objectifs
- Automatiser des tâches répétitives (CI/CD, monitoring, alertes)
- Garder le contrôle total sur ses données et secrets API
- Intégrer facilement des outils via plugins (GitHub, Slack, Jira)
- Avoir un assistant qui fonctionne même sans internet

### Frustrations
- Les assistants cloud exposent potentiellement ses secrets
- Dépendance aux services externes qui peuvent tomber
- Manque de contrôle sur le comportement de l'assistant
- Configuration complexe et maintenance chronophage

### Niveau technique
- Avancé : confortable avec CLI, Docker, APIs
- Comprend les concepts de sécurité (chiffrement, sandboxing)
- Capable de débugger et lire des logs

### Attentes vis-à-vis de l'agent
- **Privacy first:** Jamais de fuite de données sensibles
- **Résilience:** Continue de fonctionner même si internet coupe
- **Extensibilité:** Peut ajouter ses propres plugins/skills
- **Transparence:** Logs clairs, diagnostic accessible
- **Performance:** Réponses rapides, pas de latence inutile

### Use Cases Typiques
- "Surveille mes repositories GitHub et notifie-moi des PRs urgentes"
- "Exécute mes tests et déploie si OK"
- "Récupère les logs de prod et analyse les erreurs"
- "Résume mes meetings Slack du jour"

---

## Persona 2 — Sarah, la Chef de Projet Tech

### Objectifs
- Centraliser les informations de plusieurs canaux (Slack, Teams, emails)
- Suivre l'avancement des tâches et projets
- Générer des rapports et synthèses automatiquement
- Déléguer des tâches administratives chronophages

### Frustrations
- Information dispersée entre trop d'outils
- Perte de temps à faire des synthèses manuelles
- Oubli de follow-ups importants
- Exposition des données projets sur des clouds tiers

### Niveau technique
- Intermédiaire : utilise des outils SaaS, pas de développement
- Confortable avec interfaces graphiques
- Veut de la simplicité, pas de CLI complexe

### Attentes vis-à-vis de l'agent
- **Interface simple:** UI claire, pas besoin de commandes obscures
- **Fiabilité:** Doit fonctionner 24/7 sans supervision
- **Multi-canal:** Accessible depuis Slack, Teams, UI locale
- **Sécurité:** Données projets sensibles bien protégées
- **Autonomie:** L'agent gère les tâches de bout en bout

### Use Cases Typiques
- "Résume les discussions Slack du channel engineering de cette semaine"
- "Rappelle-moi tous les vendredis de faire le reporting hebdo"
- "Collecte les updates de chaque membre d'équipe et génère un dashboard"
- "Alerte-moi si un projet prend du retard selon les deadlines"

---

## Persona 3 — Marc, le Power User Local-First

### Objectifs
- Maximiser la privacy : tout doit rester local
- Avoir un système qui fonctionne 100% offline
- Contrôler et auditer absolument tout
- Expérimenter avec des modèles locaux customisés

### Frustrations
- Dépendance forcée aux APIs cloud (OpenAI, etc.)
- Manque de transparence sur ce que font vraiment les assistants
- Impossibilité d'auditer les décisions de l'IA
- Vendor lock-in avec les gros providers

### Niveau technique
- Expert : développeur expérimenté ou SysAdmin
- Maîtrise Rust, Docker, Kubernetes
- Connaît bien les concepts de cryptographie et sécurité

### Attentes vis-à-vis de l'agent
- **100% local-first:** Aucune donnée ne sort de sa machine
- **Open & auditable:** Code open source, logs complets
- **Modèles locaux:** Peut utiliser ses propres modèles (Ollama, etc.)
- **Cluster mode:** Peut déployer sur plusieurs machines
- **Zero-trust:** Sandbox strict, permissions granulaires

### Use Cases Typiques
- "Configure un cluster Akasha sur 3 machines locales"
- "Utilise uniquement mon modèle Llama local, pas d'API externe"
- "Audite tous les accès aux secrets depuis 30 jours"
- "Crée un plugin custom pour interfacer avec mon système maison"

---

## Persona 4 — Julie, l'Early Adopter Curieuse

### Objectifs
- Découvrir de nouvelles façons de booster sa productivité
- Expérimenter avec l'IA sans risquer sa privacy
- Trouver un assistant "éthique" et transparent
- Apprendre les bases de l'IA agentique

### Frustrations
- Méfiance envers les gros assistants cloud (ChatGPT, Copilot)
- Confusion sur "qui voit quoi" dans ses conversations
- Complexité technique des solutions alternatives
- Manque de guidance pour bien démarrer

### Niveau technique
- Débutant-Intermédiaire : utilise des apps, pas de code
- Veut comprendre sans être submergée de détails techniques
- Capable d'installer un logiciel et suivre un tutoriel

### Attentes vis-à-vis de l'agent
- **Simplicité:** Onboarding guidé, UI intuitive
- **Pédagogie:** L'agent explique ce qu'il fait
- **Sécurité rassurante:** Visuellement clair que c'est local
- **Progressive disclosure:** Fonctionnalités avancées cachées au début
- **Support intégré:** Akasha Core Model pour l'aider en cas de blocage

### Use Cases Typiques
- "Comment installer Akasha sur mon Mac ?"
- "Aide-moi à configurer Slack pour recevoir des notifications"
- "C'est quoi un plugin et comment j'en installe un ?"
- "Pourquoi Akasha est plus sécurisé que ChatGPT ?"