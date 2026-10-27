# Livraison de la bêta Windows pour particuliers

Le périmètre est celui validé le 26 octobre 2026 : documents personnels avec
OCR local, organisation/rappels, veille, consentement distant, mémoire et
sauvegardes chiffrées. Les trois parcours réutilisent les tâches du daemon.

## Suivi des lots

| Lot | Livraison | Acceptation restante |
|---|---|---|
| 1 — Résultat attendu | Corpus de 50 scénarios exécuté par consumer_beta_run.py ; preuves HTTP/natives et CI ; séries de trois passages | Qualification des paquets de publication, distincte des preuves de développement |
| 2 — Consentement et protection | Accords par tâche/récurrence, transports protégés, SQLCipher, clé Windows, sauvegarde et migration récupérable | Validation du paquet et second compte Windows ; voir consumer_beta_privacy.md |
| 3 — Modèles et démarrage | Trois étapes, première tâche vérifiée, mesures synthétiques, recours distant explicite, classement des preuves complètes par usage | Modèles et matériels réellement qualifiés à publier ; voir consumer_beta_models.md |
| 4 — Documents et OCR | Lecture isolée PDFium/Tesseract, index v2, réponse sourcée, source et export ; essais natifs/HTTP/UI | Qualification du paquet, arrêts forcés et corpus complet ; voir consumer_beta_documents.md |
| 5 — Organisation, veille, sauvegarde | Propositions confirmées, rappels persistés, veille consentie, correction mémoire, restauration isolée et sélection du profil | Second compte Windows et paquets de publication ; voir consumer_beta_organization.md |
| 6 — Distribution et pilote | Accueil Simple, démonstration PDF/photo/TXT, vidéo reproductible, paquets candidats CPU/NVIDIA, gate de diffusion et compteurs volontaires | Acceptation physique des paquets ; dix personnes pendant quatre semaines ; voir consumer_beta_distribution.md |

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

Le rapport courant et les commandes de reproduction sont dans
[consumer_beta_distribution.md](consumer_beta_distribution.md). Les observations
de développement ne remplacent aucune case d'acceptation physique.
