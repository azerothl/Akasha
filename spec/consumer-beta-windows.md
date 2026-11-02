# Akasha : bêta Windows pour particuliers

Retrouvez le montant d’une facture, les démarches indiquées dans un courrier,
ou les actions à préparer à partir d’une note. Chaque réponse documentaire
renvoie vers les passages du fichier original pour pouvoir la vérifier.

**Préversion technique Windows 0.13.0-beta.1 ; pilote avec données personnelles non ouvert.**
Les paquets CPU et NVIDIA et leurs preuves sont disponibles dans la
[release bêta](https://github.com/azerothl/Akasha/releases/tag/v0.13.0-beta.1).
Les autorisations, le chiffrement et la restauration doivent franchir le
contrôle complet de qualification avant le pilote.
Il n’existe pas encore de configuration matérielle recommandée pour cette bêta.

La [démonstration documentaire filmée](../docs/demos/consumer-beta-document.webm)
utilise des données fictives et un binaire de développement identifié dans son
reçu. Elle illustre le parcours ; chaque candidat doit conserver ses propres
preuves de qualification.

## Premier lancement

1. Récupérez le paquet complet `akasha-windows-0.13.0-beta.1-cpu.zip` ou
   `akasha-windows-0.13.0-beta.1-cuda.zip`, avec son rapport d'essai.
   La variante NVIDIA n’est proposée comme
   recommandée qu’après essai sur la configuration indiquée dans ce rapport.
2. Décompressez dans un nouveau dossier, puis double-cliquez sur
   **START-AKASHA.cmd**. Ne remplacez pas les fichiers d’une installation existante.
   Le pilote utilise son propre profil Windows et le port local 3877.
3. Suivez les trois étapes : préparer Akasha, choisir sa confidentialité,
   réussir une première tâche. Un modèle à télécharger reste un candidat ;
   « disponible » ne signifie pas « recommandé ».
4. Dans « Comprendre mes documents », choisissez **Essayer le dossier fictif**.
   Le dossier contient la même facture sous forme de PDF mixte texte/scan,
   photo et texte. Le montant attendu est **42 €**, l’échéance **10 novembre
   2026**. Ouvrez chaque référence pour comparer avec l’original.

Le téléchargement nécessite Internet et est mesuré séparément du temps de prise
en main. PDFium et Tesseract 5 français/anglais sont fournis avec leurs licences
et empreintes dans `ocr-runtime/`. Leur utilisation ne requiert pas de service
d’OCR distant ni d’installation manuelle. Les fichiers de démonstration sont
entièrement fictifs et leurs empreintes figurent dans `demo/manifest.json`.

## Usages et limites

- **Documents :** PDF texte, scanné ou mixte, photos JPG/PNG, TXT et Markdown.
  Jusqu’à 25 Mio et 100 pages par fichier, 20 fichiers par lot. L’OCR est limité
  à 60 secondes par page. Les pages non lues et les incertitudes sont signalées.
  L’écriture manuscrite, Word et Excel ne sont pas pris en charge. L’export PDF
  passe par la fenêtre d’impression Windows ; choisissez un fichier de sortie.
- **Notes en actions :** propositions à vérifier, dates et création à confirmer.
  La liste montre les actions et rappels réellement enregistrés. Marquer une
  action terminée annule son rappel en attente.
- **Veille personnelle :** sujet, sources et fréquence confirmés, accord propre
  à la veille, révocable et valable 30 jours. Rapports avec nouveautés, déjà-vus
  et sources indisponibles. Les extraits sont ceux des pages/moteurs de recherche ;
  ils ne constituent pas une vérification indépendante de leur contenu.

Akasha doit rester ouvert pour les rappels et la veille. Un ordinateur arrêté
ne travaille pas : une seule exécution reprend la période manquée au retour.

## Confidentialité et récupération

Les modèles locaux sont le choix par défaut. Une clé de service enregistrée
n’autorise aucun envoi. Un accord précise destinataire et données pour la tâche
et ses reprises ; un autre destinataire ou une autre portée exige un nouvel accord.
La veille a sa propre autorisation. Les intégrations externes non qualifiées,
la découverte réseau et les mises à jour automatiques sont désactivées dans
ce paquet. Les images distantes dans les réponses sont bloquées par sa politique
de contenu. Les composants publics de modèles sont téléchargés seulement lors
d’une préparation demandée dans l’interface.

La mémoire long terme est chiffrée avec SQLCipher et une clé aléatoire protégée
par le Gestionnaire d’identifiants Windows. Une clé indisponible verrouille la
mémoire. **Les conversations, notes et documents importés ne sont pas chiffrés
par Akasha pendant cette bêta.** Ils restent protégés par le compte Windows.

Créez une sauvegarde dans « Sauvegarder mon profil », avec une phrase personnelle
d’au moins 12 caractères, puis enregistrez une copie sur un autre support.
La sauvegarde utilise Argon2id et AES-256-GCM. Les modèles, caches, journaux,
clés et autorisations distantes sont exclus. La restauration est vérifiée dans
un dossier séparé ; elle n’écrase pas le profil actif. Confirmez son utilisation
au prochain démarrage, puis fermez et relancez Akasha. Reconnectez les services
et réactivez les rappels souhaités. Si le profil sélectionné devient inaccessible,
fermez la bêta puis lancez **RECOVER-PROFILE.cmd** : les deux profils sont conservés.

Une mise à jour du candidat consiste à sauvegarder le profil, fermer la bêta et
décompresser le nouveau paquet dans un nouveau dossier. Le profil reste séparé
des binaires. N’utilisez pas un ancien moteur sur la mémoire protégée.

## Qualification avant diffusion personnelle

Le rapport doit identifier paquet, binaire, moteur, modèle, configuration et
matériel. Il exige le corpus complet, aucun faux succès, trois passages réels
consécutifs à au moins 95 %, ainsi qu’une installation/mise à jour/restauration
depuis le paquet, une machine sans GPU, une NVIDIA et un autre compte Windows.
Les tests contrôlés et les compilations des autres systèmes restent distincts.
Voir [le suivi des six lots](dev/quality/CONSUMER_BETA_DELIVERY.md).

Après validation seulement, dix volontaires participent pendant quatre semaines.
Les mesures facultatives restent locales, sans contenu ni nom de fichier ;
chaque personne choisit d’exporter les compteurs déclarés. Objectifs : 8/10
installent et réussissent la démonstration sans assistance, moins de cinq minutes
d’intervention humaine hors téléchargement, 8/10 réussissent avec leurs propres
documents, 6/10 réutilisent spontanément un parcours en quatrième semaine.
Ces objectifs ne sont pas présentés comme des résultats déjà obtenus.
