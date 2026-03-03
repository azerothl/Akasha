# Security Model

## Stockage Secrets

- Vault local chiffré
- Accès par rôle uniquement
- Jamais exposé en sortie texte

## Isolation Agents

- Chaque agent exécuté en sandbox logique
- Accès mémoire limité par rôle
- Pas d’accès réseau par défaut

## Prompt Injection Protection

- Filtrage des instructions systèmes
- Validation des commandes critiques
- Séparation strict input utilisateur / instructions système

## Logs

- Chiffrés
- Redaction automatique des secrets