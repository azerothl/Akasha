# Runbook: Redémarrage du daemon

## Quand redémarrer

- Après changement de configuration (port, vault, plugins).
- En cas de comportement anormal (timeouts, erreurs répétées).

## Redémarrage depuis l'application

- **Avec superviseur** (`akasha start` sans `--foreground`) : la commande `/restart` (Chat) ou `POST /api/restart` fait quitter le daemon avec le code 85 ; le superviseur détecte une sortie non nulle et relance automatiquement le daemon. Aucune action manuelle.
- **Sans superviseur** (`akasha start --foreground` ou exécution directe du binaire daemon) : `/restart` ou `POST /api/restart` arrête le processus sans le relancer ; il faut relancer manuellement avec `akasha start`.

## Procédure manuelle

1. Arrêter proprement : `akasha stop` (envoi SIGTERM au daemon).
2. Attendre quelques secondes que le processus se termine.
3. Démarrer : `akasha start` (avec superviseur) ou `akasha start --foreground` (logs visibles).

## Vérification

- `akasha doctor` doit afficher `[OK] Daemon health endpoint` après redémarrage.
- En mode cluster, un autre nœud peut prendre le leadership ; pas d'action requise.

## Garde-fous

- Ne pas forcer l'arrêt (kill -9) sauf en dernier recours.
- En cluster, éviter de redémarrer tous les nœuds en même temps.
