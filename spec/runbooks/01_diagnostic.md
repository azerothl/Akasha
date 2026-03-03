# Runbook: Diagnostic système (akasha doctor)

## Objectif

Vérifier l'état du système Akasha et identifier les causes de dysfonctionnement.

## Vérifications standard

1. **Rust / Node** : prérequis pour build et UI.
2. **Binaire daemon** : `akasha-daemon` doit être trouvé (même répertoire que le CLI ou dans PATH).
3. **Fichiers de spec** : au moins `09_event_model.yaml` et `10_data_model.yaml` dans `spec/`.
4. **Santé du daemon** : endpoint `GET /` sur le port configuré (`AKASHA_PORT`, défaut 3876) doit répondre `{"status":"ok"}`.

## Actions recommandées (lecture seule)

- Si le daemon ne répond pas : vérifier qu'il est démarré (`akasha start` ou `start --foreground`).
- Si les specs manquent : exécuter depuis la racine du projet ou définir le répertoire de spec.
- Ne jamais proposer de commande destructive (suppression de données, formatage) sans confirmation explicite de l'utilisateur.

## Garde-fous

- Ne pas exposer de secrets (vault, tokens) dans les diagnostics.
- Privilégier les actions en lecture seule ; toute action d'écriture doit être clairement indiquée et confirmable.
