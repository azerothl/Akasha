# Cluster Architecture

## Modes

1. Standalone
2. Multi-node local network
3. Hybrid distributed

## Objectifs

- Répartition charge agents
- Haute disponibilité
- Résilience matérielle
- Continuité service

## Architecture

- Node Principal (Coordinator)
- Nodes Secondaires (Execution Nodes)
- Synchronisation append-only log
- Réplication mémoire long terme chiffrée

## Tolérance de panne

- Election automatique leader
- Reprise des tâches en cas de perte d’un node
- Isolation node compromis