# Plugin Architecture

Akasha adopte une architecture plugin-first.

## Types de Plugins

1. Channel Plugins
   - Slack
   - Teams
   - Discord
   - etc.

2. Tool Plugins
   - API connectors
   - Automations
   - System tools

3. Skill Plugins
   - Capacités métier spécialisées

4. MCP Plugins
   - Connecteurs protocole
   - Gouvernance externe

5. Memory Plugins
   - Alternative storage
   - Vector DB
   - Knowledge Graph

6. Model Plugins
   - Gestion multi-LLM
   - Routing modèle
   - Quantization

7. Security Plugins
   - Vault externe
   - HSM
   - Audit avancé

## Règles

- Les plugins ne peuvent pas modifier le core
- Permissions déclaratives obligatoires
- Sandbox d’exécution
- Signature numérique requise