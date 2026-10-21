# Recette de premier démarrage — candidat v0.12.0

Campagne du 21 octobre 2026, construite depuis le commit `ae439027bf17636248827c4ea4b2ca99a6f816eb`. Le workflow Release a été lancé avec `publish=false` : aucun artefact public ni tag `v0.12.0` n’a été créé. Les résultats valident les archives candidates conservées temporairement dans Actions, pas une release publiée.

## Résultats

Chaque archive a été extraite dans un environnement neuf avec un répertoire de données temporaire. La recette vérifie `init`, la disponibilité du daemon, `doctor`, la route embarquée Candle, la réponse finale exacte `AKASHA_OK` et le lancement de l’UI quand elle est fournie.

| Archive candidate | Runner / OS et architecture | `init` | Daemon | `doctor` | Première réponse | UI | Job |
| --- | --- | ---: | ---: | --- | --- | --- | --- |
| `akasha-full-linux-x86_64.zip` | `ubuntu-latest`, Linux x86_64 | 1 s | 1 s | Réussi | Candle CPU, 41 s, `AKASHA_OK` | Fenêtre visible `Akasha` | [119394523869](https://github.com/azerothl/Akasha/actions/runs/39645516338/job/119394523869) |
| `akasha-full-windows-x86_64.zip` | `windows-latest`, Windows x64 | Réussi | 1,1 s | Réussi | Candle CPU, 69,24 s, `AKASHA_OK` | Fenêtre `Akasha`, 0,61 s | [119394523796](https://github.com/azerothl/Akasha/actions/runs/39645516338/job/119394523796) |
| `akasha-full-macos-x86_64.zip` | `macos-15-intel`, macOS x86_64 | 1 s | 1 s | Réussi | Candle CPU, 153 s, `AKASHA_OK` | Lancement confirmé, binaire x86_64 | [119394523942](https://github.com/azerothl/Akasha/actions/runs/39645516338/job/119394523942) |
| `akasha-full-macos-aarch64.zip` | `macos-latest`, macOS ARM64 | 1 s | 1 s | Réussi | Candle CPU, 34 s, `AKASHA_OK` | Lancement confirmé, binaire arm64 | [119394524017](https://github.com/azerothl/Akasha/actions/runs/39645516338/job/119394524017) |

Les quatre jobs de premier démarrage, les builds de release et l’assemblage des archives ont réussi dans le [run 39645516338](https://github.com/azerothl/Akasha/actions/runs/39645516338). Le smoke Linux démarre Openbox sous Xvfb, puis confirme une fenêtre réellement visible nommée `Akasha`.

## Limite du compteur de tokens

Les quatre réponses progressives conservées dans les états de tâche se terminent par `AKASHA_OK.` et la route observée est `technical_writer` / `default`, avec Candle chargé. Le champ `last_turn_tokens_out` vaut toutefois `0` sur cette version ; Candle ne fournit pas ce compteur dans cet endpoint. La recette prouve donc la réponse par son texte final exact et son backend observé, mais ne présente pas ce compteur comme une mesure de tokens.

L’artefact de diagnostic Windows de ce run conserve un bloc `<think>` dans `task.json`. Le smoke Windows a validé la réponse après retrait de ce bloc, mais son export d’évidence n’était pas expurgé. La PR corrige l’écriture du fichier pour expurger le JSON avant sauvegarde et refuser tout bloc restant ; cette correction n’a pas encore produit un nouvel artefact de diagnostic.

## Conclusion pour #141

La matrice complète est validée sur le candidat v0.12.0. L’issue reste ouverte jusqu’à la publication de la release, au téléchargement des artefacts publics correspondants et à la répétition de la recette sur ceux-ci. Cette campagne ne valide pas les archives CUDA, qui sont suivies séparément.
