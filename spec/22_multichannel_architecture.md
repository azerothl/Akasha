# Architecture Multi-Canal

## Canaux Supportés par Défaut

- Interface locale textuelle
- Interface vocale locale
- Slack
- Microsoft Teams
- Discord
- WhatsApp
- Telegram

## Propriétés

- Chaque canal est un adaptateur isolé
- Aucun canal n’a accès direct aux secrets
- Messages entrants convertis en format standard interne

## Extensibilité

De nouveaux canaux peuvent être ajoutés via plugins conformes à l’interface:

```
interface ChannelPlugin {
  connect()
  receive()
  send()
  authenticate()
  disconnect()
}
```