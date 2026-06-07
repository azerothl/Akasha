# Évaluation — CalDAV et Email (inspiration Odysseus)

**Statut:** CalDAV **implémenté** (phases 1–3, 2026-06-07) ; Email **non implémenté**  
**Contexte:** Odysseus intègre IMAP/SMTP + CalDAV ; Akasha reste focalisé daemon/opérateur avec calendrier externe en modèle parallèle.

---

## CalDAV

### Ce qu'Odysseus apporte

- Sync bidirectionnelle avec Radicale, Nextcloud, Apple, Fastmail
- Couleurs par calendrier, import/export `.ics`
- Agent « calendrier-aware »

### État Akasha (2026-06-07)

| Livrable | Statut |
|----------|--------|
| Modèle `external_calendar_events` + comptes CalDAV | Livré |
| `GET/POST /api/calendar/ics` import/export | Livré |
| `GET /api/calendar/events` merge runs + `type: external` | Livré |
| Sidecar `Akasha_plugins/caldav-channel` (PROPFIND, REPORT, outbox PUT/DELETE) | Livré |
| Outils agent `calendar_query`, `calendar_create`, `calendar_update`, `calendar_delete` | Livré |
| UI Tauri onglet CalDAV / ICS + styles événements externes | Livré |
| Spec [`caldav-integration.md`](caldav-integration.md) | Livré |

Calendrier **opérateur** (schedules, task runs) inchangé ; événements personnels CalDAV/ICS dans un cache séparé (pas fusionné dans `ScheduleStore`).

### Effort réalisé vs estimation initiale

| Composant | Estimation initiale | Réalisé |
|-----------|---------------------|---------|
| Fondation ICS + API | ~1 sem | Phase 1 |
| Sidecar pull + comptes | 3–4 sem | Phase 2 |
| Bidirectionnel + outils write | ~4 sem | Phase 3 |

### Recommandation (mise à jour)

**CalDAV en production partielle** : ICS + sync sidecar + écriture via outbox. OAuth Apple / tests CI Radicale restent backlog ; voir [`caldav-integration.md`](caldav-integration.md).

---

## Email (IMAP/SMTP)

### Ce qu'Odysseus apporte

- Boîtes multiples, triage IA, brouillons style-appris, spam/urgence
- Tâches email planifiées, rappels ntfy/email

### État Akasha

- Canaux **messagerie** (Slack, Discord, Telegram, Teams) — pas de client mail
- Notifications via gateways existants

### Effort estimé

| Composant | Effort |
|-----------|--------|
| IMAP/SMTP + vault credentials | 2–3 semaines |
| Pipeline triage/synthèse LLM | 1–2 semaines |
| UI inbox + composer | 3–4 semaines |
| Sécurité (PII, isolation) | continu |

### Recommandation

**Hors cœur court terme.** Piste préférée : **plugin communautaire** ou skill + connecteur externe plutôt qu'un client mail natif dans le daemon.

---

## Décision

| Intégration | Décision | Prochaine étape |
|-------------|----------|-----------------|
| CalDAV | **Retenu — livré v0.x (ICS + sidecar + outbox)** | CI Radicale, OAuth Apple si demande |
| Email | Non retenu v0.x | Documenter pattern webhook/gateway pour alertes |

Réviser cette évaluation si le positionnement produit évolue vers « workspace grand public » type Odysseus.
