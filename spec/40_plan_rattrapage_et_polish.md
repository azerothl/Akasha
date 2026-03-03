# Plan de rattrapage (phases partielles) + Phase Polish UI

## Ordre de traitement

1. **Phase 6** — Providers manquants + métriques visualisables  
2. **Phase 4** — Canaux manquants (Telegram, optionnel Teams)  
3. **Phase 7** — mTLS NATS + réplication state/log  
4. **Phase 8** — Suite d’évals + onboarding  
5. **Phase 9 — Polish UI** (nouvelle phase en dernier)

---

## Phase 6 — Rattrapage

### 6.1 Providers cloud (akasha-llm)

| Provider      | Statut   | Action |
|---------------|----------|--------|
| OpenAI        | Absent   | Implémenter `OpenAIProvider` (API chat/completions), config `api_key_ref` / vault, modèle dans routing |
| Anthropic     | Absent   | Implémenter `AnthropicProvider` (messages API), idem config |
| OpenRouter    | Absent   | Implémenter `OpenRouterProvider` (API unifiée), multi-modèle |
| Azure OpenAI  | Absent   | Implémenter `AzureOpenAIProvider` (endpoint + key) |
| Google AI     | Absent   | Implémenter `GoogleAIProvider` (Gemini/Vertex style) |

- Config : clés dans vault (`vault://openai_api_key` etc.), référence dans `llm_router.yaml` ou env.
- Enregistrement conditionnel : n’enregistrer que les providers dont la config (URL/key) est présente.

### 6.2 Métriques visualisables

- Existant : `GET /api/router/metrics` (JSON).
- À faire : page ou section dans l’UI Tauri qui affiche métriques (requêtes, latence, fallbacks, coût si dispo). Peut être inclus dans la phase Polish UI.

---

## Phase 4 — Rattrapage

### 4.1 Canaux additionnels

| Canal    | Priorité | Contraintes              | Action |
|----------|----------|---------------------------|--------|
| Telegram | Haute    | Bot API simple            | Adapter type Slack/Discord : webhook ou long polling, `POST /channels/telegram` ou équivalent, token dans vault |
| Teams    | Moyenne  | Bot Framework / webhook   | Adapter similaire, auth OAuth ou token |
| WhatsApp | Basse    | API business / partenaire | Stub ou doc “hors scope v1” si trop contraint |

- Critère spec : même `task_id` visible et pilotable depuis plusieurs canaux → déjà vrai si on ajoute Telegram/Teams comme nouveaux adapters qui appellent la même API daemon.

---

## Phase 7 — Rattrapage

### 7.1 mTLS pour NATS

- Config optionnelle dans `llm_router.yaml` ou fichier dédié `cluster.yaml` : `nats.tls.client_cert`, `client_key`, `ca`.
- Connexion NATS avec `async_nats` en TLS + client certs quand config présente.

### 7.2 Réplication state / log

- Leader : à chaque append log et à chaque événement tâche (création, mise à jour), publier sur NATS (`akasha.cluster.replicate.log`, `akasha.cluster.replicate.task`).
- Followers : souscrire et appliquer (réécrire log local, appliquer événements au TaskStore local).
- Au passage en leader : état déjà cohérent si réplication à jour ; sinon chargement depuis un snapshot ou replay (MVP : replay des événements depuis le dernier connu).

### 7.3 Perte d’un node sans perte de tâches

- Garantie par réplication : toutes les mutations (log + tâches) répliquées avant ack. Nouveau leader reprend avec état répliqué.

---

## Phase 8 — Rattrapage

### 8.1 Suite d’évals

- **Security** : tests automatisés (script ou crate) — prompt injection (doit refuser), pas d’exfiltration de secrets dans la sortie.
- **Runbooks** : vérifier que les réponses doctor/advice citent les runbooks (ou au moins ne contredisent pas).
- **Hallucinations** : checks basiques (réponses vides, format attendu). Optionnel : jeu de Q/R sur la spec.

### 8.2 Onboarding guidé

- Doc ou flow dans l’UI : “Premier lancement” (vault, Ollama, `llm_router.yaml`, doctor). Peut être intégré au Polish UI.

---

## Phase 9 — Polish UI (nouvelle)

- **Chat** : mise en forme des réponses, indicateur de progression tâche, erreurs claires.
- **État** : affichage “daemon connecté / déconnecté”, statut des checks (optionnel).
- **Métriques routeur** : page ou panneau “Router” (requêtes, modèle utilisé, fallbacks, latence).
- **Paramètres** : rappel des variables d’environnement / lien vers doc, ou formulaire minimal (port, chemin data_dir).
- **Cohérence visuelle** : thème, typo, feedback (loading, succès, erreur).
- **Accessibilité** : contrastes, focus, labels (base).

---

## Critères de complétion (résumé)

- **Phase 6** : au moins 2 providers cloud opérationnels (ex. OpenAI + OpenRouter), métriques exposées et affichables (API + UI ou doc).
- **Phase 4** : au moins 1 canal de plus (Telegram recommandé).
- **Phase 7** : mTLS optionnel NATS, réplication log + tâches branchée, test “kill leader → nouveau leader reprend sans perte”.
- **Phase 8** : suite d’évals (security + runbooks) exécutable, onboarding documenté ou intégré à l’UI.
- **Phase 9** : UI polish livrée (chat, métriques, paramètres, cohérence).
