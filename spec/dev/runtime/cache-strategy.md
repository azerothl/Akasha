# Cache requêtes idempotentes (stratégie)

## Objectif

Réduire la charge sur providers et services externes pour **GET idempotents** à forte répétition (ex. métadonnées routeur, listes statiques), avec **TTL** et **invalidation** prévisibles.

## Périmètre v1 (recommandé)

- **Pas** de cache des réponses LLM utilisateur (risque de fuite contextuelle).
- Cibles typiques : `GET /api/router/models`, `GET /api/plugins`, health agrégé — **derrière** un feature flag `AKASHA_HTTP_CACHE_TTL_SECS` (optionnel, défaut 0 = désactivé).

## Clés

- Normaliser : méthode + path + query **triée** (éviter doublons `?b=1&a=2` vs `?a=2&b=1` si activé pour query).

## Invalidation

- **TTL court** (ex. 30–120 s) pour listes ; **invalidation à chaud** sur `POST` mutation (reload plugins, update router) en vidant le namespace `router:` ou `plugins:`.

## Garde-fous

- Plafond entrées (LRU, ex. 256) et taille max réponse en octets (ex. 256 Ko) — rejeter le cache au-delà.
- Jamais cacher si `Authorization` personnalisé ou cookies session (non applicable au loopback Akasha par défaut).

## Implémentation

Code : module dédié + hook dans `handle_api` **après** auth locale ; livraison progressive derrière env. Ce document sert de **contrat** avant merge du code LRU.
