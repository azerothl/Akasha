# Cargo audit triage — 21 octobre 2026

## Résumé

Audit exécuté avec `cargo-audit 0.22.2` et la base RustSec mise à jour le 21 octobre 2026, sur la branche qui inclut la montée de Wasmtime 36 et les changements de cette PR. Le workspace et l'application Tauri ont deux lockfiles distincts ; leurs résultats sont rapportés séparément.

| Résultat | Nombre | Traitement |
|---|---:|---|
| Workspace `Cargo.lock` : occurrences vulnérables | 3 | Toutes dans `zip`, déjà suivies par #205 |
| Workspace `Cargo.lock` : avis non maintenus | 12 | Classés informatifs selon #219, sans remplacement direct demandé |
| Workspace `Cargo.lock` : avis `unsound` | 0 | Les deux avis `lru` ont disparu avec `lru` 0.18 |
| Workspace `Cargo.lock` : dépendances `yanked` | 0 | Les deux versions précédemment signalées ont disparu après mise à jour du lockfile |

Les changements de cette PR mettent à niveau `reqwest` 0.11 vers 0.12, `async-nats` 0.36 vers 0.50 et `lru` 0.12 vers 0.18. La mise à niveau de `ratatui` 0.28 vers 0.30 retire aussi l'ancienne copie de `lru` retenue par le TUI. Les chaînes `h2` 0.3 et `rustls` 0.21 des anciens clients HTTP ont disparu. Serenity utilise maintenant son backend TLS natif pris en charge, sans la branche `rustls` 0.22 / `rustls-webpki` 0.102.

## Avis restant dans le périmètre de #219

La pile WebSocket de Serenity 0.12.5 conservait `rustls` 0.22.4 et `rustls-webpki` 0.102.8, sans mise à niveau simple du lockfile. [La documentation Serenity 0.12.5](https://docs.rs/crate/serenity/0.12.5) décrit aussi le backend `native_tls_backend` : SChannel sous Windows, Secure Transport sous macOS et OpenSSL ailleurs. Ce backend a été choisi pour éliminer la branche vulnérable sans dépendance Git non publiée ni remplacement du client Discord.

Le build et les tests multiplateformes de la PR valident la compilation de ce backend. Une connexion à un vrai serveur Discord ne fait pas partie des tests automatisés ; le bot n'est pas activé pendant les tests.

## Avis suivis ailleurs

Les 3 avis `zip` concernent `zip` 7.4.0 dans le chemin Candle et restent suivis par #205. Le rapport `cargo audit` les affiche toujours ; la fermeture de #205 ne prétend pas supprimer l’avis, elle documente l'absence du chemin d'extraction vulnérable dans l’usage courant d’Akasha.

Les 12 avis `unmaintained` concernent `anymap2`, `derivative`, `fxhash`, `instant`, `number_prefix`, `paste`, `serial`, `ttf-parser`, `unsafe-libyaml`, `zbus_names`, `zvariant` et `zvariant_derive`. `fxhash` est une dépendance transitive de Wasmtime ; `ttf-parser` est suivi avec la chaîne PDF. Selon la classification demandée dans #219, ces notices n’impliquent pas une faille connue et ne reçoivent pas de remplacement spéculatif dans cette PR.

## Lockfile Tauri distinct

L'audit de `apps/akasha-ui/src-tauri/Cargo.lock` (477 crates) ne trouve aucune vulnérabilité de type `vulnerable`, mais rapporte 6 notices `unmaintained` et l'avis `unsound` RUSTSEC-2024-0429 sur `glib` 0.18.5. Le graphe Linux passe par GTK 0.18.2, puis `muda`, Tauri et WebKitGTK. L'avis concerne les méthodes d'itération de `glib::VariantStrIter`; RustSec indique une version corrigée à partir de `glib` 0.20.0.

Recherche de portée : aucun fichier `crates/` ou `apps/` du dépôt ne référence `VariantStrIter` ou `array_iter_str`. Dans les sources de crates téléchargées pour ce graphe, ces symboles ne figurent que dans `glib` lui-même (implémentation, tests et documentation), sans appel observé par une dépendance. Tauri reste lié à GTK 0.18 ; cette notice est conservée comme résidu amont explicitement non atteint par un appel observé dans Akasha, sans prétendre que l'avis a disparu.

## Vérification

- `cargo check -p akasha-cluster -p akasha-daemon -p akasha-tui` : réussi sur Windows MSVC.
- `cargo test -p akasha-daemon -p akasha-cluster -p akasha-tui --lib --quiet` : code de sortie 0, 579 tests réussis, 1 ignoré, 0 échec, après le changement de backend TLS.
- `cargo audit --file Cargo.lock --json` : 3 occurrences vulnérables liées à `zip`, 12 notices non maintenues, aucune notice `unsound` ni dépendance `yanked`.
- `cargo audit --file apps/akasha-ui/src-tauri/Cargo.lock` : aucune vulnérabilité, 6 notices non maintenues et 1 avis `unsound` (`glib` 0.18.5) documenté ci-dessus.
- `git diff --check` : réussi.

La suite GitHub Actions reste la validation finale du SHA de la PR.
