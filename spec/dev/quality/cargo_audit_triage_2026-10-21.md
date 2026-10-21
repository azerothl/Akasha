# Cargo audit triage — 21 octobre 2026

## Résumé

Audit exécuté avec `cargo-audit 0.22.2` et la base RustSec mise à jour le 21 octobre 2026, sur la branche qui inclut la montée de Wasmtime 36 et les changements de cette PR.

| Résultat | Nombre | Traitement |
|---|---:|---|
| Occurrences vulnérables | 10 | 7 dans la pile TLS Discord, 3 dans `zip` déjà suivis par #205 |
| Avis non maintenus | 12 | Classés informatifs selon #219, sans remplacement direct demandé |
| Avis `unsound` | 0 | Les deux avis `lru` ont disparu avec `lru` 0.18 |
| Dépendances `yanked` | 0 | Les deux versions précédemment signalées ont disparu après mise à jour du lockfile |

Les changements de cette PR mettent à niveau `reqwest` 0.11 vers 0.12, `async-nats` 0.36 vers 0.50 et `lru` 0.12 vers 0.18. La mise à niveau de `ratatui` 0.28 vers 0.30 retire aussi l'ancienne copie de `lru` retenue par le TUI. Les chaînes `h2` 0.3 et `rustls` 0.21 des anciens clients HTTP ont disparu.

## Avis restant dans le périmètre de #219

Les 7 occurrences restantes sont `rustls` 0.22.4 (2 avis) et `rustls-webpki` 0.102.8 (5 avis), uniquement via `serenity` 0.12.5 → `tokio-tungstenite` 0.21.0. Le graphe de dépendances de cette version Serenity fixe la famille WebSocket 0.21, incompatible avec le correctif rustls 0.23.46. Le registre ne publie pas encore Serenity 0.13 ; la mise à niveau ne peut pas être résolue par un simple update du lockfile.

Décision appliquée : ne pas remplacer le client Discord par une dépendance Git non publiée ni une autre bibliothèque dans ce lot. Conserver ces avis comme risque explicite et rouvrir l’upgrade lorsque Serenity publiera une version compatible, ou après décision spécifique sur le remplacement du client.

## Avis suivis ailleurs

Les 3 avis `zip` concernent `zip` 7.4.0 dans le chemin Candle et restent suivis par #205. Le rapport `cargo audit` les affiche toujours ; la fermeture de #205 ne prétend pas supprimer l’avis, elle documente l'absence du chemin d'extraction vulnérable dans l’usage courant d’Akasha.

Les 12 avis `unmaintained` concernent `anymap2`, `derivative`, `fxhash`, `instant`, `number_prefix`, `paste`, `serial`, `ttf-parser`, `unsafe-libyaml`, `zbus_names`, `zvariant` et `zvariant_derive`. `fxhash` est une dépendance transitive de Wasmtime ; `ttf-parser` est suivi avec la chaîne PDF. Selon la classification demandée dans #219, ces notices n’impliquent pas une faille connue et ne reçoivent pas de remplacement spéculatif dans cette PR.

## Vérification

- `cargo check -p akasha-cluster -p akasha-daemon -p akasha-tui` : réussi sur Windows MSVC.
- `cargo test -p akasha-daemon -p akasha-cluster -p akasha-tui --lib --quiet` : code de sortie 0, 579 tests réussis, 1 ignoré, 0 échec.
- `cargo audit --file Cargo.lock --json` : 10 occurrences vulnérables, 12 notices non maintenues, aucune notice `unsound` ni dépendance `yanked`.
- `git diff --check` : réussi.

La suite GitHub Actions reste la validation finale du SHA de la PR.
