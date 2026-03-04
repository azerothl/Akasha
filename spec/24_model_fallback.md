# Model Strategy, Router & Fallback

## LLM Router Intelligent

Akasha intègre un **LLM Router** sophistiqué qui permet de:
- Router les requêtes vers le modèle optimal selon le type de tâche
- Supporter plusieurs providers simultanément
- Gérer les fallbacks automatiques
- Optimiser coûts et performance

### Architecture du Router

```
User Request → Task Classifier → LLM Router → Provider Selector → Model
                                      ↓
                                 Fallback Chain
                                      ↓
                              (en cas d'échec)
```

## Types de Tâches Supportés

Le router supporte plusieurs types de tâches, chacun pouvant avoir un modèle dédié:

| Type | Description | Use Case |
|------|-------------|----------|
| **code_generation** | Génération et analyse de code | Développement, debugging, review |
| **creative_writing** | Rédaction créative | Articles, emails, contenu |
| **scientific_analysis** | Analyse scientifique | Recherche, papiers, données |
| **data_analysis** | Analyse de données | Statistiques, visualisation |
| **conversation** | Conversation générale | Chat, Q&A basique |
| **system_diagnostic** | Diagnostic système | Support interne Akasha |

### Configuration Utilisateur

L'utilisateur peut configurer pour chaque type de tâche:
- Le modèle principal à utiliser
- Le provider préféré
- Les fallbacks autorisés
- Les contraintes de coût/latence

**Exemple de configuration:**
```yaml
code_generation:
  primary:
    provider: anthropic
    model: claude-3.5-sonnet
  fallback:
    - provider: openai
      model: gpt-4
    - provider: ollama
      model: deepseek-coder
```

## Providers Supportés

Akasha supporte de multiples providers LLM:

### Providers Cloud

- **OpenAI** - GPT-4, GPT-3.5, etc.
- **Anthropic** - Claude 3 Opus/Sonnet/Haiku
- **OpenRouter** - Accès unifié à 100+ modèles
- **Azure OpenAI** - Version entreprise d'OpenAI
- **Google AI** - Gemini Pro/Ultra

### Providers Locaux

- **Ollama** - Modèles locaux (Llama, Mistral, etc.)
- **Local** - Modèle custom de l'utilisateur
- **akasha_embedded** - Modèle intégré à l’application (Qwen3 0.6B ou Baguettotron 321M) ; utilisé par défaut pour tous les types de tâche. Voir [34_embedded_small_model.md](34_embedded_small_model.md).
- **Akasha Core** - Modèle interne natif (utilise aussi l’embarqué en priorité si disponible)

### Architecture Multi-Provider

```
┌─────────────────────────────────────────┐
│         LLM Router Manager              │
├─────────────────────────────────────────┤
│  ┌──────────┐  ┌──────────┐  ┌────────┐│
│  │ OpenAI   │  │Anthropic │  │OpenRtr ││
│  │ Provider │  │ Provider │  │Provider││
│  └──────────┘  └──────────┘  └────────┘│
│  ┌──────────┐  ┌──────────┐  ┌────────┐│
│  │  Ollama  │  │  Local   │  │ Akasha ││
│  │ Provider │  │ Provider │  │  Core  ││
│  └──────────┘  └──────────┘  └────────┘│
└─────────────────────────────────────────┘
```

## Stratégie de Fallback

En cas de problème avec le modèle principal, le router applique une **chaîne de fallback**:

### 1️⃣ Primary Model
Modèle configuré pour le type de tâche

### 2️⃣ Provider Alternative
Autre modèle du même provider (ex: GPT-4 → GPT-3.5)

### 3️⃣ Cross-Provider Fallback
Modèle équivalent d'un autre provider (ex: Claude → GPT-4)

### 4️⃣ Local Model
Modèle local (Ollama, etc.)

### 5️⃣ Akasha Core
Modèle de secours minimal garanti disponible

### Conditions de Basculement

Le fallback s'active automatiquement en cas de:
- **API Error** - Erreur HTTP 5xx, service down
- **Timeout** - Pas de réponse sous 10 secondes
- **Rate Limit** - Quota atteint
- **Model Unavailable** - Modèle temporairement indisponible
- **Cost Threshold** - Budget utilisateur dépassé

### Configuration Fallback

```yaml
fallback_strategy:
  timeout_seconds: 10        # Timeout avant fallback
  max_retries: 2             # Tentatives avant fallback définitif
  retry_cooldown: 60s        # Cooldown avant retry du primary
  log_switches: true         # Logger les changements de modèle
  notify_user: true          # Notifier l'utilisateur du fallback
```

## Mode Offline Total

En mode offline (pas d'internet):
- ✅ Tous les providers cloud sont désactivés
- ✅ Routing automatique vers modèles locaux uniquement
- ✅ Akasha Core Model reste disponible
- ✅ Notification claire à l'utilisateur

## Monitoring & Métriques

Le router collecte des métriques pour optimiser les décisions:

### Métriques par Modèle
- **Latence** - Temps de réponse moyen
- **Coût** - Coût par requête (tokens)
- **Qualité** - Score utilisateur/feedback
- **Erreurs** - Taux d'erreur/indisponibilité
- **Switches** - Nombre de fallbacks déclenchés

### Dashboard Utilisateur

L'utilisateur peut visualiser:
- Répartition des requêtes par modèle
- Coûts par provider
- Taux de succès/échec
- Latence moyenne
- Recommandations d'optimisation

## Auto-Optimisation (Future)

Le router pourra à terme:
- Apprendre des préférences utilisateur
- Optimiser automatiquement le routing
- Suggérer des modèles alternatifs moins chers/plus rapides
- Détecter les patterns de tâches

## Sécurité & Privacy

- ✅ Les clés API sont stockées dans le vault chiffré
- ✅ Chaque provider a des permissions ABAC dédiées
- ✅ Logs redactés (pas de secrets)
- ✅ L'utilisateur contrôle quels providers ont accès à quelles données