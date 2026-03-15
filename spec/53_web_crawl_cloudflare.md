# Spécification — Crawl web optionnel (Cloudflare Browser Rendering)

Ce document décrit l’intégration **optionnelle** du crawl de sites via l’API [Cloudflare Browser Rendering](https://developers.cloudflare.com/browser-rendering/) (endpoint `/crawl`). L’utilisateur peut activer cette fonctionnalité pour permettre à l’agent de crawler un site entier en une requête (découverte automatique des URLs, rendu headless, sortie HTML, Markdown ou JSON), au-delà du simple `web_fetch` (une page, pas de rendu JavaScript).

---

## 1. Contexte et objectifs

### 1.1 Besoin

- **Limite actuelle** : `web_fetch` récupère une seule URL en GET, sans exécution JavaScript. Pour ingérer tout un site (documentation, blog, base de connaissances), l’agent devrait enchaîner de nombreux appels et gérer lui-même la découverte des liens.
- **Objectif** : offrir une option pour lancer un **crawl de site** en une requête : soumission d’une URL de départ, découverte automatique des pages (sitemap, liens), rendu headless, et récupération du contenu en plusieurs formats (HTML, Markdown, JSON structuré). Cas d’usage : RAG, veille, recherche ou monitoring sur un domaine.

### 1.2 Référence externe

- [Changelog Cloudflare — Crawl entire websites with a single API call](https://developers.cloudflare.com/changelog/post/2026-03-10-br-crawl-endpoint/)
- [Documentation de l’endpoint /crawl](https://developers.cloudflare.com/browser-rendering/rest-api/crawl-endpoint/)

L’API Cloudflare est un **signed-agent** qui respecte `robots.txt` et [AI Crawl Control](https://www.cloudflare.com/ai-crawl-control/) par défaut, ce qui facilite la conformité avec les règles des sites crawlés.

### 1.3 Optionnel pour l’utilisateur

La fonctionnalité n’est **active** que si l’utilisateur :

1. Configure un compte Cloudflare (`account_id`) et une clé API (token).
2. Active le crawl dans `tools_policy.yaml` (ex. `web_crawl_enabled: true`).

Sans configuration, les outils `web_crawl` et `web_crawl_status` ne sont pas exposés ou renvoient un message explicite invitant à configurer Cloudflare et à activer l’option.

---

## 2. Périmètre fonctionnel

### 2.1 Outils prévus

| Outil | Usage | Description |
|-------|--------|-------------|
| `web_crawl` | `web_crawl <url> [options]` | Lancer un job de crawl sur l’URL de départ. Appel à l’API Cloudflare (POST). Retourne un `job_id`. L’URL doit appartenir à un domaine autorisé (`allowed_web_domains` / `blocked_web_domains`). Options (à définir en implémentation) : profondeur, limite de pages, format de sortie, etc. |
| `web_crawl_status` | `web_crawl_status <job_id>` | Consulter le statut d’un job et récupérer les résultats (GET sur l’API Cloudflare). Permet à l’agent de poller jusqu’à complétion et d’obtenir le contenu crawlée (HTML, Markdown ou JSON). |

Le format exact des arguments (séparateurs, options) est à fixer lors de l’implémentation (alignement avec le parsing des autres outils dans le daemon).

### 2.2 Comportement

- **Jobs asynchrones** : le crawl s’exécute côté Cloudflare. L’agent reçoit un `job_id` après `web_crawl`, puis interroge `web_crawl_status` pour suivre la progression et récupérer les résultats.
- **Résultats** : selon la configuration de l’appel Cloudflare, le contenu peut être retourné en HTML, Markdown ou JSON structuré. L’agent (ou un module en aval) peut utiliser ce contenu pour la mémoire long terme, un index RAG, ou une synthèse pour l’utilisateur.
- **Politique de domaines** : l’URL de départ du crawl est soumise aux mêmes règles que `web_fetch` : `allowed_web_domains` et `blocked_web_domains` dans `tools_policy.yaml`. Les pages découvertes pendant le crawl restent sous le contrôle de Cloudflare (respect robots.txt, etc.).

---

## 3. Configuration

Les clés suivantes sont ajoutées à `tools_policy.yaml`. Référence : [35_configuration_reference.md](35_configuration_reference.md), [tools_policy.example.yaml](tools_policy.example.yaml).

| Clé | Type | Défaut | Description |
|-----|------|--------|-------------|
| `web_crawl_enabled` | booléen | `false` | Activer les outils `web_crawl` et `web_crawl_status`. Nécessite `cloudflare_account_id` et une clé API. |
| `cloudflare_account_id` | string | — | Identifiant du compte Cloudflare (pour l’URL d’appel à l’API Browser Rendering). |
| `cloudflare_api_key_ref` | string | — | Référence de la clé API : `vault://cloudflare_api_token` ou nom de variable d’environnement (ex. `CLOUDFLARE_API_TOKEN`). |

Sans `web_crawl_enabled: true` et sans compte/clé valides, les outils ne sont pas proposés ou renvoient une erreur explicite.

---

## 4. Sécurité et éthique

- **Respect des sites** : l’endpoint Cloudflare est un signed-agent qui respecte `robots.txt` et AI Crawl Control. La spec rappelle à l’utilisateur de limiter les domaines autorisés via `allowed_web_domains` et `blocked_web_domains` pour éviter les usages non souhaités.
- **Pas de contournement** : l’API ne permet pas de contourner les protections anti-bot ou les captchas ; le crawler s’identifie comme bot.
- **Données sensibles** : la clé API Cloudflare doit être stockée dans le vault ou en variable d’environnement, jamais en clair dans le fichier de configuration.

---

## 5. Implémentation (hors scope de la présente spec)

La présente spécification décrit le **comportement attendu** et la **configuration** pour une implémentation future. Aucun code (crate, daemon, UI) n’est modifié dans le cadre de cette entrée de spec ; l’implémentation pourra s’appuyer sur les outils et clés de configuration décrits ci-dessus.
