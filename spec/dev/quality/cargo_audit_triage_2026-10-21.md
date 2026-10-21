# Cargo audit triage — 21 octobre 2026

## Résumé

Audit exécuté avec `cargo-audit 0.22.2` et la base RustSec mise à jour le 21 octobre 2026, sur la branche qui inclut la montée de Wasmtime 36 et les changements de cette PR.

| Résultat | Nombre | Traitement |
|---|---:|---|
| Occurrences vulnérables | 3 | Toutes dans `zip`, déjà suivies par #205 |
| Avis non maintenus | 12 | Classés informatifs selon #219, sans remplacement direct demandé |
| Avis `unsound` | 0 | Les deux avis `lru` ont disparu avec `lru` 0.18 |
| Dépendances `yanked` | 0 | Les deux versions précédemment signalées ont disparu après mise à jour du lockfile |

Les changements de cette PR mettent à niveau `reqwest` 0.11 vers 0.12, `async-nats` 0.36 vers 0.50 et `lru` 0.12 vers 0.18. La mise à niveau de `ratatui` 0.28 vers 0.30 retire aussi l'ancienne copie de `lru` retenue par le TUI. Les chaînes `h2` 0.3 et `rustls` 0.21 des anciens clients HTTP ont disparu. Serenity utilise maintenant son backend TLS natif pris en charge, sans la branche `rustls` 0.22 / `rustls-webpki` 0.102.

## Avis restant dans le périmètre de #219

La pile WebSocket de Serenity 0.12.5 conservait `rustls` 0.22.4 et `rustls-webpki` 0.102.8, sans mise à niveau simple du lockfile. [La documentation Serenity 0.12.5](https://docs.rs/crate/serenity/0.12.5) décrit aussi le backend `native_tls_backend` : SChannel sous Windows, Secure Transport sous macOS et OpenSSL ailleurs. Ce backend a été choisi pour éliminer la branche vulnérable sans dépendance Git non publiée ni remplacement du client Discord.

Le build et les tests multiplateformes de la PR valident la compilation de ce backend. Une connexion à un vrai serveur Discord ne fait pas partie des tests automatisés ; le bot n'est pas activé pendant les tests.

## Avis suivis ailleurs

Les 3 avis `zip` concernent `zip` 7.4.0 dans le chemin Candle et restent suivis par #205. Le rapport `cargo audit` les affiche toujours ; la fermeture de #205 ne prétend pas supprimer l’avis, elle documente l'absence du chemin d'extraction vulnérable dans l’usage courant d’Akasha.

Les 12 avis `unmaintained` concernent `anymap2`, `derivative`, `fxhash`, `instant`, `number_prefix`, `paste`, `serial`, `ttf-parser`, `unsafe-libyaml`, `zbus_names`, `zvariant` et `zvariant_derive`. `fxhash` est une dépendance transitive de Wasmtime ; `ttf-parser` est suivi avec la chaîne PDF. Selon la classification demandée dans #219, ces notices n’impliquent pas une faille connue et ne reçoivent pas de remplacement spéculatif dans cette PR.

## Vérification

- `cargo check -p akasha-cluster -p akasha-daemon -p akasha-tui` : réussi sur Windows MSVC.
- `cargo test -p akasha-daemon -p akasha-cluster -p akasha-tui --lib --quiet` : code de sortie 0, 579 tests réussis, 1 ignoré, 0 échec, après le changement de backend TLS.
- `cargo audit --file Cargo.lock --json` : 3 occurrences vulnérables liées à `zip`, 12 notices non maintenues, aucune notice `unsound` ni dépendance `yanked`.
- `git diff --check` : réussi.

La suite GitHub Actions reste la validation finale du SHA de la PR.
