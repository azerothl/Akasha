# Recette premier démarrage — release v0.11.0

Relevé du 20 octobre 2026. Cette matrice porte sur les archives publiques
de la release, depuis une extraction et un répertoire de données temporaires.
Un run marqué « smoke historique » a utilisé le script antérieur qui masquait
l'échec de `doctor` ; son statut vert ne certifie donc pas le contrôle complet.

## Résultats

| Artefact publié | Plateforme | Résultat observé | État |
| --- | --- | --- | --- |
| `akasha-linux-x86_64.zip` | `ubuntu-latest`, Linux x86_64 | `init`, daemon et première tâche terminés. Le daemon signale un modèle embarqué prêt ; première inférence en 55 s, 88 tokens déclarés. `doctor` échoue sur `09_event_model.yaml` et `10_data_model.yaml`. La réponse finale visible est `Réponds.` au lieu de la réponse demandée. | Échec ; non validé |
| `akasha-full-linux-x86_64.zip` | Linux x86_64 | Non testé | Non validé |
| `akasha-windows-x86_64.zip` | Windows x86_64 | Non testé dans cette campagne | Non validé |
| `akasha-full-windows-x86_64.zip` | Windows x86_64 | Non testé | Non validé |
| `akasha-macos-x86_64.zip` | macOS x86_64 | Non testé | Non validé |
| `akasha-macos-aarch64.zip` | macOS ARM64 | Non testé | Non validé |
| `akasha-full-macos-x86_64.zip` | macOS x86_64 | Non testé | Non validé |
| `akasha-full-macos-aarch64.zip` | macOS ARM64 | Non testé | Non validé |
| `akasha-ui-linux.zip`, `akasha-ui-windows.zip`, `akasha-ui-macos.zip` | UI desktop | Aucun lancement sur poste neuf dans cette campagne | Non validé |
| Archives Windows CUDA | Windows avec GPU NVIDIA | Hors de cette matrice CPU ; suivi dans #142 et #150 | Non validé ici |

## Preuve Linux

- Release : [`v0.11.0`](https://github.com/azerothl/Akasha/releases/tag/v0.11.0)
- Archive testée : `akasha-linux-x86_64.zip` (40 562 417 octets)
- Run CI : [39411352153](https://github.com/azerothl/Akasha/actions/runs/39411352153), SHA `221cbe1770f64fb57fe12efeeb6703ce322f443a`, job `release-artifact-first-boot-linux`
- Log et état de tâche conservés par l'artefact `release-first-boot-39411352153-1`.
- `doctor` rapporte : « Spec YAML files missing » et nomme les fichiers
  `09_event_model.yaml` et `10_data_model.yaml`.
- La tâche termine et retourne `last_turn_model_used=embedded`, mais les
  dernières étapes de progression contiennent une réponse hors sujet. Le
  smoke historique vérifiait seulement le statut de la tâche et un compte de
  tokens positif ; il affichait `smoke OK` malgré ces deux échecs fonctionnels.

Le script [`smoke-release-artifact-linux.sh`](../../../scripts/smoke-release-artifact-linux.sh)
est maintenant strict : il conserve la sortie de `doctor`, n'ignore plus son
code d'échec et valide la réponse finale au prompt déterministe. Une nouvelle
exécution sur l'archive publiée v0.11.0 doit échouer, ce qui est le résultat
attendu pour signaler les défauts connus de cet ancien artefact. Le workflow
permet de relancer cette recette manuellement après publication d'une nouvelle
release ; elle n'empêche plus les PR ordinaires de passer en retestant à chaque
fois une archive déjà connue comme défectueuse. Les blocs `<think>` sont retirés
des journaux conservés afin de ne pas publier le raisonnement interne du modèle.

## Conclusion

La seule archive CPU testée n'est pas validée. Les autres archives et les
clients UI restent non testés. Le ticket #141 reste ouvert jusqu'à la correction
des défauts Linux et à l'exécution de la matrice sur les artefacts concernés.
