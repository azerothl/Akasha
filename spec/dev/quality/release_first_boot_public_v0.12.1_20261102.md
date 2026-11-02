# Premier démarrage des archives publiques v0.12.1 — 2 novembre 2026

Les quatre archives CPU complètes de la [release publique v0.12.1](https://github.com/azerothl/Akasha/releases/tag/v0.12.1) ont été téléchargées à nouveau et testées dans des profils neufs. Ce sont les octets publiés, sans reconstruction locale ni remplacement d’asset.

Preuves : [campagne 41484317475](https://github.com/azerothl/Akasha/actions/runs/41484317475), quatre jobs réussis et artefacts `published-first-boot-*`. Les reçus durables sont dans [le manifeste JSON](release_first_boot_public_v0.12.1_20261102.json).

| Archive | Environnement observé | Init, daemon, doctor | Réponse CPU exacte `AKASHA_OK` | Latence de la dernière génération | Lancement UI |
|---|---|---|---|---|---|
| `akasha-full-linux-x86_64.zip` | Ubuntu 24.04, Linux x86_64, glibc 2.39 | Réussis | Réussie, Candle | 56,483 s | Fenêtre Akasha et rendu contrôlés |
| `akasha-full-windows-x86_64.zip` | Windows Server 2022, AMD64 | Réussis | Réussie, Candle | 63,040 s | Fenêtre Akasha, 0,59 s |
| `akasha-full-macos-x86_64.zip` | macOS 15.7.9, Intel | Réussis | Réussie, Candle | 158,470 s | Binaire x86_64 lancé |
| `akasha-full-macos-aarch64.zip` | macOS 26.6.2, ARM64 | Réussis | Réussie, Candle | 33,925 s | Binaire ARM64 lancé |

Chaque tâche termine avec le statut `completed` et le modèle observé `default`. Le contrôle compare la réponse à un marqueur exact ; une réponse simplement non vide ne suffit pas. Le compteur `tokens_out` de cette version reste à zéro sur ce chemin : il n’est pas présenté comme une mesure du débit. Les durées ci-dessus viennent de `last_turn_latency_ms`, sans téléchargement, installation ni démarrage de l’interface.

Les empreintes des archives, architectures, environnements, identifiants de campagne et commit du harness sont conservés dans le manifeste. Les journaux complets et le contrôle de rendu Linux sont téléchargeables dans les artefacts de campagne, conservés trente jours. Le workflow `Published release first boot` permet de reproduire le contrôle d’un autre tag existant sans publier de release.

Le chemin Linux qui échouait avec `$1: unbound variable` dans #263 est effectivement exécuté jusqu’à la vérification de la fenêtre. Le correctif #264 est donc confirmé. Cette campagne répond à #141 pour la release corrigée v0.12.1 ; elle ne modifie pas les anciennes archives v0.11.0 défectueuses.

Limites : cette recette porte sur le premier démarrage CPU et l’ouverture de l’interface. Elle ne qualifie ni les interactions complètes de l’UI, ni CUDA, ni les trois parcours de la bêta Windows v0.13.0-beta.1, ni les modifications ultérieures de main.
