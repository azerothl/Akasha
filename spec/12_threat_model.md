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

---

TM-004 — Accès local aux souvenirs persistants
Risque:
Une personne ou un processus ayant accès au compte utilisateur peut lire `memory.db`, les copies de sauvegarde ou les exports de mémoire. La base SQLite n’est pas chiffrée par Akasha ; le texte des souvenirs et les embeddings sont lisibles directement. `AKASHA_MEMORY_ENCRYPT` est sans effet.

Mesures actuelles:
- Protéger le `data_dir` et les exports par les permissions du compte utilisateur.
- Activer le chiffrement complet du disque (BitLocker, FileVault ou LUKS) et protéger de la même façon les sauvegardes et copies externes.
- Limite : le chiffrement du disque protège l’appareil éteint/verrouillé ; il ne protège pas contre un processus disposant des droits de l’utilisateur dans une session déverrouillée.

Risque résiduel:
Pas de chiffrement applicatif au repos ni de clé mémoire. La livraison éventuelle d’un chiffrement de `memory.db` nécessite une migration et un cycle de vie de clé dédiés (voir `spec/dev/roadmap/memory_encryption_rfc.md`).
