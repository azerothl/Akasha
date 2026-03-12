# WhatsApp — hors scope v1

## Décision

**WhatsApp** n’est pas dans le périmètre de la **v1** des canaux Akasha. Décision documentée (plan d’avancement, étape 9).

## Canaux actuels (v1)

- Slack
- Discord
- Telegram
- Microsoft Teams

## Pour une version ultérieure

Adapter WhatsApp impliquerait notamment :

- **API** : WhatsApp Business API (Meta) ou partenaire (Twilio, etc.).
- **Authentification** : clés / webhooks selon le fournisseur.
- **Réception / envoi** : webhook pour les messages entrants, API d’envoi (templates si requis).
- **Sécurité et conformité** : politique de confidentialité, conditions d’usage (ex. WhatsApp Business Policy).

Même pattern que les autres canaux : module dans `akasha-daemon/src/channels/`, config, enregistrement dans le routeur de canaux.

## Références

- [40_plan_rattrapage_et_polish.md](40_plan_rattrapage_et_polish.md)
- Canaux existants : `crates/akasha-daemon/src/channels/` (slack, discord, telegram, teams).
