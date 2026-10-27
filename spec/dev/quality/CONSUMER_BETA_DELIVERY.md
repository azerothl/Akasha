# Livraison de la bêta Windows pour particuliers

Le périmètre est celui validé le 26 octobre 2026 : documents personnels avec
OCR local, organisation/rappels, veille, consentement distant, mémoire et
sauvegardes chiffrées. Les trois parcours réutilisent les tâches du daemon.

## Suivi des lots

| Lot | Livraison | Acceptation restante |
|---|---|---|
| 1 — Résultat attendu | Corpus versionné de 50 scénarios ; classification des preuves et gate à trois runs réels | Exécution complète sur les composants des lots suivants et les paquets distribués |
| 2 — Consentement et protection | Socle : accords par tâche, transports protégés, SQLCipher, clé Windows, API de sauvegarde/restauration | Voir consumer_beta_privacy.md : validation du paquet, périmètre documentaire, reprise HTTP et second compte Windows |
| 3 — Modèles et démarrage | Démarrage en trois étapes, première tâche vérifiée et préqualification synthétique réelle | Classement sur corpus complet, calibration et recours distant ; voir consumer_beta_models.md |
| 4 — Documents et OCR | À livrer | PDF mixtes, photos, références et paquet OCR |
| 5 — Organisation, veille, sauvegarde | À livrer | Persistance, restauration sur autre compte |
| 6 — Distribution et pilote | À livrer | Paquets CPU/NVIDIA ; dix personnes pendant quatre semaines |

Les PR #273 et #274 sont des prérequis. Tant qu'elles ne sont pas intégrées,
les PR de la bêta sont empilées sur leur branche ; cela ne vaut pas validation
ni publication d'une version.

## Preuves et qualification

`consumer_beta_corpus.json` définit 10 conversations, 15 documents, 10 cas
d'organisation, 5 veilles et 10 contrôles de protection/restauration. Chaque
preuve décrit un résultat observé (`result` ou `refusal`) et les contrôles
indépendants requis. Un scénario absent est `not_run` et bloque le gate.

`consumer_beta_eval.py --evidence chemin.json --output rapport.json` distingue
résultat correct, refus attendu, échec explicite et faux succès. Le rapport
conserve l'identité du binaire, des sources, du moteur, du modèle et de sa
configuration. `--qualify run1.json run2.json run3.json` refuse les empreintes
différentes et les passages incomplets. Un fournisseur contrôlé ne qualifie
jamais un modèle réel.

Le pilote exige un export volontaire de compteurs locaux sans contenus,
chemins, noms de documents ou secrets. Aucun participant n'est contacté
automatiquement. La présence du code ou d'un plan de pilote ne prouve ni une
campagne exécutée ni ses résultats.
