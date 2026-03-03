# Threat Model

TM-001 — Prompt Injection
Risque:
Sous-agent exécute instruction malveillante.

Mitigation:
- Validation stricte des instructions critiques
- Séparation instruction système / user

---

TM-002 — Secret Exfiltration
Risque:
Agent tente d’afficher token API.

Mitigation:
- Filtrage sortie texte
- Redaction automatique
- Secrets jamais injectés dans contexte conversationnel

---

TM-003 — Escalade inter-agent
Risque:
Sous-agent tente de s’auto-promouvoir.

Mitigation:
- RBAC strict
- Permissions immuables runtime