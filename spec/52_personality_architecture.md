# Architecture de personnalité (5 niveaux)

Ce document décrit l’architecture de personnalité de l’agent Akasha en 5 niveaux : Identité, Valeurs, Comportements, Modes, Mémoire de personnalité. La personnalité est définie par des fichiers YAML dans `spec/` et fusionnée avec le profil utilisateur (`data_dir/agent_profile.json`).

## Fichiers de configuration

| Fichier | Contenu |
|---------|--------|
| `spec/personality_core.yaml` | Noyau identitaire, valeurs, traits par défaut, règles comportementales, maniérismes |
| `spec/personality_modes.yaml` | Modes (assistant, operator, architect, onboarding) |
| `spec/initiative_policy.yaml` | Politique d’initiative (quand proposer, confirmer, créer tâche, se taire), politique sociale, visibilité du raisonnement |
| `data_dir/agent_profile.json` | Surcharge utilisateur : nom, genre, avatar, personnalité texte, rôle, rules, can_do, cannot_do, traits_override, preferred_mode |

Voir aussi [35_configuration_reference.md](35_configuration_reference.md) section 2b pour la structure de `agent_profile.json`.

## Les 5 niveaux

1. **Identité** — Noyau fixe (archetype, mission, posture, relation à l’utilisateur) + nom et genre depuis le profil utilisateur.
2. **Valeurs** — Hiérarchie des valeurs (clarté, sécurité, transparence, efficacité, continuité, contrôle utilisateur) pour guider les décisions.
3. **Comportements** — Règles comportementales (YAML) + rules / can_do / cannot_do du profil.
4. **Modes** — Mode actif (assistant, operator, architect, onboarding) : sélectionné par l’utilisateur (preferred_mode), dérivé du type de tâche (assigned_agent, Phase 3), ou défaut « assistant ».
5. **Mémoire de personnalité** — Ce que l’agent doit retenir (ton préféré, niveau technique, etc.) et ce qu’il ne doit pas inférer (état émotionnel, traits sensibles). S’appuie sur la mémoire long terme et le bloc [Contexte utilisateur] du memory orchestrator.

## Ordre des blocs dans le prompt

1. `APP_CONTEXT` (contexte Akasha, outils, règles métier)
2. `[Role]` (agent spécialisé : conversation, code, search, etc.)
3. **Bloc personnalité** : identité (name, mission, posture), valeurs, traits, behavior_rules, maniérismes, mode actif, initiative, social/reasoning
4. Mémoire court terme
5. Mémoire orchestrator (long terme, projet, facts, episodic, user_identity_block, personality_memory_block)
6. RAG documents utilisateur
7. Turns (historique conversation)

## Mémoire de personnalité (schéma)

**Clés structurées** (à mémoriser) : `preferred_tone`, `technical_depth_preference`, `confirmation_threshold`, `favorite_channels`, `recurring_projects`, `common_workflows`. D'autres clés sont acceptées.

**Ne jamais inférer sans preuve** : emotional state, sensitive identity traits, security permissions.

**Stockage** : événements épisodiques `event_type = "personality_memory"`, payload JSON `{"key", "value"}`. Le memory orchestrator injecte le bloc [Mémoire de personnalité]. **Écriture** : `POST /api/personality-memory` body `{ "key": "...", "value": "..." }`, réponse `{ "ok": true, "id": "<uuid>" }`. Une phrase rappelant ce principe est injectée dans le prompt système. L’enrichissement du schéma de mémoire (entités dédiées) et l’écriture/lecture structurée peuvent faire l’objet d’une évolution ultérieure.
