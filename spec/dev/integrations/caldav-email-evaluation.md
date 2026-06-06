# Évaluation — CalDAV et Email (inspiration Odysseus)

**Statut:** Évaluation produit — **non implémenté** (2026-06-03)  
**Contexte:** Odysseus intègre IMAP/SMTP + CalDAV ; Akasha reste focalisé daemon/opérateur.

---

## CalDAV

### Ce qu'Odysseus apporte

- Sync bidirectionnelle avec Radicale, Nextcloud, Apple, Fastmail
- Couleurs par calendrier, import/export `.ics`
- Agent « calendrier-aware »

### État Akasha

- Calendrier **interne** (récurrences, occurrences, scheduler) — voir `spec/36_ui_architecture.md`
- **Aucun connecteur CalDAV** dans le monorepo

### Effort estimé

| Composant | Effort |
|-----------|--------|
| Client CalDAV Rust (sync pull/push) | 3–5 semaines |
| Résolution conflits + mapping événements | 2 semaines |
| UI comptes + statut sync | 1 semaine |
| Tests multi-fournisseurs | 1–2 semaines |

### Recommandation

**Reporter (P3)** sauf demande utilisateur explicite. Alternative intermédiaire : export/import `.ics` manuel via API (`GET/POST /api/calendar/ics`) — effort ~3 jours.

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
| CalDAV | Non retenu v0.x | Spec `.ics` import/export si besoin calendrier externe |
| Email | Non retenu v0.x | Documenter pattern webhook/gateway pour alertes |

Réviser cette évaluation si le positionnement produit évolue vers « workspace grand public » type Odysseus.
