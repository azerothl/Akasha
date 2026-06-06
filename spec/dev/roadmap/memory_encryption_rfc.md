# RFC — Chiffrement au repos de la mémoire long terme (A4)

**Statut :** Veille / draft (wave 6)  
**Owner :** Akasha core team  
**Référence matrice :** [memory_landscape_roadmap_matrix.md](./memory_landscape_roadmap_matrix.md) — ligne **A4**  
**Exigence spec :** [spec/06_memory_model.md](../../06_memory_model.md) (long terme « persistant chiffré »)

## 1. Contexte

Aujourd’hui :

- **Vault** (`akasha-store` / secrets) : chiffrement AES-256-GCM pour clés API et tokens.
- **`memory.db`** (long terme, faits, archive platform) : SQLite **en clair** sur disque dans `data_dir`.
- **Embeddings** : blobs f32 dans `memory_entries.embedding` — lisibles si accès fichier.

KinBot et le rapport « mémoire agents 2026 » positionnent le chiffrement mémoire comme **attendu** en self-hosted. Akasha couvre la souveraineté (A5) mais pas encore A4.

## 2. Objectifs (wave 6)

1. Protéger **confidentialité au repos** des contenus mémoire (texte + métadonnées sensibles).
2. Conserver **performance recall** acceptable (pas de x10 latence sur top-k).
3. Rester **100 % local** — pas de KMS cloud obligatoire.
4. Migration **non destructive** depuis installs existantes.

## Non-objectifs v1

- Chiffrement homomorphique ou recherche sur ciphertext sans déchiffrement.
- Chiffrement du court terme volatile (session RAM) — hors scope.
- Certification FIPS / HSM enterprise.

## 3. Options techniques (veille)

| Option | Avantages | Risques |
|--------|-----------|---------|
| **SQLCipher** (SQLite encrypt extension) | Transparent pour rusqlite ; fichier unique | Build/feature flags ; embeddings en blob chiffré = pas de recherche vectorielle SQL native sans extension |
| **Chiffrement page-level libsodium** sur fichier DB | Contrôle fin | Maintenance custom, migrations fragiles |
| **Dossier vault mémoire** (entries chiffrées JSON/msgpack) | Aligné vault existant | Refactor `LongTermStore`, recall plus complexe |
| **Chiffrement contenu seul** (embedding + text AES-GCM par entrée) | Recall déchiffre top-N en app | CPU sur hot path ; clé dérivée master |

**Recommandation veille :** prototyper **SQLCipher** OU **chiffrement champ `content` + embedding** avec clé dérivée du vault master (PBKDF2/Argon2 + `AKASHA_DATA_KEY` ou unlock au `akasha start`).

## 4. Modèle de clés (proposition)

```
Master secret (vault ou AKASHA_MEMORY_KEY env)
    └── Data key (AES-256) — rotated optional
            └── memory.db (SQLCipher) OR per-row content/embedding blobs
```

- **Unlock** : au démarrage daemon (`akasha start`) — prompt passphrase optionnel TUI.
- **Rotation** : export → re-encrypt → import (réutiliser bundle export v1).
- **Backup** : rappeler opérateur — perte clé = perte mémoire (doc + `doctor` warning).

## 5. Impact produit

| Composant | Changement |
|-----------|------------|
| `akasha-store/long_term_memory.rs` | Open DB chiffrée ; migrations |
| `akasha-embeddings` | Embeddings stockés chiffrés ou recalculés à l’index (trade-off) |
| FTS5 / keyword search | Index sur plaintext en mémoire temporaire OU index tokenisé séparé chiffré |
| Export/import | Bundle inclut `encryption_version` |
| `akasha doctor` | Vérifier état chiffrement, clé présente |
| Site / compare | Aligner wording « encrypted at rest » quand livré |

## 6. Plan d’implémentation suggéré

### Phase A — Spike (2 sem.)

- Bench latence recall top-20 avec SQLCipher vs champ chiffré (10k entrées synthétiques).
- Décision build : feature flag `memory-encryption`.

### Phase B — MVP (4–6 sem.)

- Migration `akasha doctor --fix --encrypt-memory` (opt-in).
- Clé via env ou vault unlock.
- Tests : export/import round-trip, recall metrics inchangés ±10 %.

### Phase C — Durcissement

- Doc opérateur, runbook perte clé, compare KinBot parity (ligne mémoire chiffrée).

## 7. Critères d’acceptation

- [x] Operator guidance when `AKASHA_MEMORY_ENCRYPT=1` (`akasha doctor --fix` sets env key).
- [ ] `memory.db` illisible sans clé (audit `strings` / sqlite3) — requires SQLCipher MVP.
- [ ] Recall e2e tests passent avec chiffrement activé.
- [ ] Export v1 reste compatible (migration documentée).
- [ ] Pas de régression perf >15 % p95 recall sur bench interne.

## 8. Références

- [spec/06_memory_model.md](../../06_memory_model.md)
- [spec/12_threat_model.md](../../12_threat_model.md)
- [kinbot-akasha-parity-matrix.md](./kinbot-akasha-parity-matrix.md)
- KinBot README — Vault AES-256-GCM

**Dernière mise à jour :** 2026-06-06 (wave 7 foundation — `AKASHA_MEMORY_ENCRYPT` + doctor guidance ; SQLCipher spike deferred pending bench).
