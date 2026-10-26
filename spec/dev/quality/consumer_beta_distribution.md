# Distribution et pilote — preuves et limites

## Livraison logicielle

Les six lots sont proposés dans des PR empilées, sans fusion automatique. Les
PR #273 et #274 restent des prérequis à intégrer et valider. Cette livraison
prépare des candidats ; elle n'ouvre pas un pilote avec des données personnelles.

Le mode Simple donne accès aux documents, notes en actions, veille personnelle
et sauvegardes. Le dossier fictif embarqué contient un PDF mixte, une photo JPG
et un TXT, tous avec empreintes. Le résultat attendu est 42 euros à payer le
10 novembre 2026. Les étapes techniques restent dans les réglages avancés.

Le démarrage permet de mesurer un candidat local, de constater l'absence de
modèle recommandé et, au besoin, de connecter un service distant dans le
Gestionnaire d'identifiants Windows. Cette connexion n'accorde aucun envoi.
L'accord du destinataire choisi est exigé avant la première tâche et effacé
après son lancement, y compris si le service échoue.

`consumer-beta-candidate.yml`, déclenché manuellement, assemble séparément CPU
et NVIDIA sous Windows 2022. Il inclut PDFium/Tesseract, licences, modèles OCR
français/anglais et embeddings ONNX hors ligne avec versions/empreintes fixes.
Les quatre GGUF du catalogue utilisent des révisions immuables et une vérification
SHA-256 avant activation. Trois anciens liens de téléchargement invalides ont
été remplacés par des fichiers vérifiés. Les poids GGUF restent téléchargés à
la demande. Un fichier invalide ne remplace pas un modèle déjà présent.

Le paquet possède son identité Tauri, le port 3877 et un profil séparé sous
`%LOCALAPPDATA%\AkashaBeta\profile`. Son lanceur vérifie les composants avant
exécution, surveille le moteur et relance uniquement son processus sur le code
85. Il conserve le profil lors d'un changement de dossier de binaires. Le
raccourci de récupération annule uniquement le choix du profil restauré ; il
ne supprime aucun profil. Aucune installation existante n'est remplacée.

Les transports avancés non qualifiés sont refusés aussi au niveau HTTP dans
l'édition particuliers. Les écritures de l'ancien scheduler ne permettent pas
de modifier le périmètre immuable d'une veille autorisée. Les rappels et veilles
passent par les parcours confirmés ; l'édition développeur garde ses API.

## Vérifications de développement du 26 octobre 2026

Ces résultats sont distincts d'une qualification des paquets de publication :

- Corpus complet déterministe : **50/50**, soit 36 résultats corrects et
  14 refus attendus, aucun faux succès. Les reçus proviennent de processus de
  tests natifs, d'appels HTTP et du worker OCR, pas de déclarations du modèle.
- Magasin SQLCipher : 37 tests ; routeur/modèles : 127 tests et un ignoré ;
  daemon particuliers : 752 tests et un ignoré. Les échecs intermédiaires sont
  conservés dans les journaux, puis les contrôles concernés sont rejoués après
  correction.
- Interface : 83 tests unitaires, compilation et 11 parcours Chromium aux
  largeurs 900/1440, dont le refus d'un envoi distant sans accord et sa remise
  à zéro après échec. Inspection finale de l'accueil, sans débordement horizontal.
- Vidéo reproductible : interface réelle, daemon réel et Ollama `qwen3.5:9b` ;
  import PDF/photo/TXT, réponse, référence ouverte sur la page originale et
  export Markdown. Seul le transport natif IPC est remplacé dans cette capture
  Chromium ; tous les appels passent au vrai daemon isolé.
- Paquet CPU **debug** assemblé et lancé avec Windows PowerShell 5.1 : fenêtre
  Tauri observée, trois documents fictifs lus via le pont natif, mémoire indiquée
  chiffrée, redémarrage effectif du moteur avec un autre PID, arrêt de son moteur
  à la fermeture de la fenêtre. Le daemon personnel `C:\Akasha` est resté actif.
  Le test a corrigé la dépendance du lanceur à `Get-FileHash` et la conservation
  du code de sortie sous PowerShell 5.1.

Les préqualifications GGUF conservent réponses et identités complètes. La
première série a trouvé 24/27 pour Qwen2.5 1.5B et 0/27 pour les trois autres.
Elle a révélé que le moteur natif ignorait `thinking_level=off` pour Qwen3/3.5.
Le préfixe de génération explicite est désormais transmis au moteur, conformément
au [format officiel Qwen](https://huggingface.co/Qwen/Qwen3.5-0.8B/blob/main/chat_template.jinja).
Les mesures précédant cette correction restent historiques, sans recommandation.
Un test natif couvre la conservation du contenu source, le préfixe fermé, le
mode avec réflexion et le refus d'un format inconnu. Les résultats rejoués et
les trois passages du corpus réel doivent être joints à chaque candidat.

## Reproduction

Préparer les composants OCR avec `scripts/prepare-ocr-runtime.py`, les embeddings
avec `scripts/ci/prepare_consumer_embeddings.py`, puis compiler le daemon en
passant l'empreinte du `runtime-lock.json` dans `AKASHA_OCR_LOCK_SHA256`. Le
paquet refuse une empreinte OCR qui ne correspond pas au binaire compilé.

```powershell
python spec/dev/quality/consumer_beta_run.py --binary target/debug/akasha-daemon.exe --runtime C:/chemin/ocr-runtime --output C:/preuves/controlled
python spec/dev/quality/consumer_beta_run.py --binary target/debug/akasha-daemon.exe --runtime C:/chemin/ocr-runtime --live --model qwen3.5:9b --repeat 3 --output C:/preuves/live
python spec/dev/quality/consumer_model_probe.py --binary target/debug/akasha-daemon.exe --provider akasha_core --model qwen3.5-0.8b-q4 --model-dir C:/chemin/modeles/qwen3.5-0.8b-q4 --output C:/preuves/probe.json
python spec/dev/quality/consumer_video_audit.py --binary target/debug/akasha-daemon.exe --runtime C:/chemin/ocr-runtime --output C:/preuves/video
```

Les profils temporaires et ports sont propres à chaque essai. Le driver conserve
réponses, statuts parent/enfants, fichiers produits, durées mesurées, empreintes
des journaux et sources natives. L'injection de pannes reste contrôlée même dans
une série réelle ; les cas sémantiques utilisent le modèle local indiqué. Les
50 lignes ne signifient donc pas 50 appels au modèle. Une série réelle conserve
le même identifiant de série et une chaîne de trois passages sans réutilisation.

La CI Windows exécute le corpus déterministe et conserve les reçus. La CI UI
exécute les onze parcours de confidentialité et usages. Les compilations et
contrôles historiques des autres systèmes restent conservés.

## Condition d'ouverture du pilote

`consumer-package.json` indique toujours `candidate_only: true` et
`personal_data_pilot_authorized: false` à la création. Le workflow ne publie ni
tag ni release. Aucun label recommandé ne provient du seul catalogue ou des
27 précontrôles.

Pour qualifier les **deux paquets**, placer les rapports et reçus dans un dossier
de preuves. Copier `consumer_package_acceptance.example.json` en `acceptance.json`,
ajouter l'entrée NVIDIA et remplir chaque contrôle après exécution. Chaque
attestation indique opérateur, date, version Windows, matériel, empreinte du
manifeste et reçus non vides avec SHA-256. Une valeur absente bloque le contrôle.

```powershell
python scripts/ci/qualify_consumer_candidate.py --package C:/candidats/cpu --package C:/candidats/nvidia --evidence-root C:/preuves --output C:/preuves/decision.json
```

Ce contrôle recalcule les résultats du corpus, exige trois passages réels
consécutifs identiques et 95 % par usage, puis vérifie les composants et les
preuves physiques. Il ne modifie ni paquet, ni permissions et ne publie rien.

Restent exigés : une vraie machine Windows sans GPU, une NVIDIA avec moteur
CUDA, installation/mise à jour/restauration depuis les paquets de publication,
interruption des migrations et restauration sous un autre compte Windows. La
portabilité cryptographique déjà testée en supprimant la clé d'origine ne
remplace pas cet autre compte. Un binaire CPU exécuté sur la machine NVIDIA
de développement ne vaut pas essai d'une machine sans GPU.

## Pilote de quatre semaines

Après passage des contrôles, dix volontaires utilisent leurs propres profils.
Le panneau facultatif conserve uniquement huit compteurs déclarés, semaine
1 à 4 et durées bornées. Il ne collecte ni textes, noms de fichiers, chemins,
identifiants, secrets ou historique de navigation. L'export est déclenché par
l'utilisateur ; aucune télémétrie ni prise de contact n'est ajoutée.

Semaine 1 : installation, temps humain jusqu'à la démonstration, téléchargement
mesuré séparément et tâche avec documents personnels. Semaines 2 et 3 : noter
localement les blocages et corrections. Semaine 4 : mesurer la réutilisation
spontanée, sans relance de l'application présentée comme une habitude acquise.
Les objectifs proposés sont 8/10 pour installation/démonstration et documents
personnels, moins de cinq minutes d'intervention humaine, 6/10 pour le retour
spontané en quatrième semaine. Aucun participant ni résultat n'est inventé.
