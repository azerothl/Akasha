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

## Candidat de staging du run 39431939285 — non publié

Le 20 octobre 2026, le candidat Linux x86_64 construit depuis le commit
`705d698` a été testé après extraction avec un répertoire de données temporaire,
dans WSL NVIDIA-Workbench sous Ubuntu 22.04.5 (glibc 2.35). Le run complet
est en mode `publish=false` ; aucune release publique n'a été créée.

- Le binaire `akasha 0.11.0` démarre et `init --defaults` termine sans erreur.
- Le daemon devient joignable en 1 s. Les deux fichiers
  `spec/09_event_model.yaml` et `spec/10_data_model.yaml` sont présents et
  chargés.
- `doctor` échoue sur la détection de Node.js : le dossier `spec/` embarqué
  fait classifier l'archive comme un checkout source, et le diagnostic
  Playwright compte aussi l'absence facultative de Node comme erreur bloquante.
  Cette cause a été reproduite dans le code; la correction est suivie par la
  PR de ce relevé et devra être retestée sur un nouvel artefact.
- Le premier message se termine après 123,7 s avec `last_turn_tokens_out=0`;
  la tâche ne fournit pas de réponse valide au prompt `OK`. Ce test reste un
  échec d'inférence et le modèle embarqué n'est pas validé.
- Le runtime glibc 2.35 affiche un avertissement pour le symbole faible
  `GLIBC_2.39`, puis les binaires continuent à s'exécuter. Le même binaire
  affiche sa version dans WSL Ubuntu 24.04.1 (glibc 2.39) sans cet avertissement.

La synthèse expurgée des diagnostics, de l'état final de la tâche et du modèle
est conservée dans
[`release_first_boot_candidate_run394319_linux.json`](release_first_boot_candidate_run394319_linux.json).
Le journal complet et le diagnostic `doctor` sont également conservés dans
l'artefact local de la recette. Ces résultats ne valident ni la réponse
embarquée, ni les archives Windows/macOS, ni l'UI.

## Candidat de staging Windows CPU du run 39431939285 — non publié

Le 20 octobre 2026, l'archive `akasha-windows-x86_64` du commit `705d698` a
été extraite et testée sur Windows 11 Professionnel Insider Preview
10.0.26220, dans un répertoire de données temporaire. Le workflow est exécuté
avec `publish=false`.

- `init --defaults` réussit, le daemon devient joignable en 0,8 s et `doctor`
  termine avec le code 0. La configuration initiale détecte aussi un service
  Ollama local (`tinyllama`).
- Pour isoler le modèle embarqué, la configuration a ensuite été réglée sur
  `akasha_core` / `core`, avec Candle sur CPU. Le modèle est déclaré chargé et
  disponible ; après copie des autres fichiers de configuration créés par
  `init`, `doctor` termine encore avec le code 0.
- La tâche du `technical_writer` se termine après 30,252 s de latence modèle
  (40,1 s écoulées au total), avec zéro token de sortie et le texte visible
  final `Réponds.` pour une demande de réponse `OK`. La réponse est invalide ;
  le premier démarrage CPU n'est donc pas validé.
- Une première exécution de `doctor` sur la configuration embarquée incomplète
  avait échoué parce que trois fichiers n'avaient pas été copiés dans le
  nouveau répertoire de données. Le diagnostic a été rejoué après copie de
  `connectors.env`, `agent_profile.json` et `tools_policy.yaml` depuis
  l'initialisation réussie ; seul le résultat corrigé est retenu ci-dessus.

La synthèse expurgée est conservée dans
[`release_first_boot_candidate_run394319_windows_cpu.json`](release_first_boot_candidate_run394319_windows_cpu.json).
Le résultat confirme le chemin de démarrage Windows, mais pas une réponse
fonctionnelle du modèle embarqué. Il ne valide pas les archives CUDA ou UI.
