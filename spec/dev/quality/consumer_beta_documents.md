# Comprendre mes documents — lecture locale et références

Le parcours accepte PDF, JPG/PNG, TXT et Markdown depuis la section Données.
Il conserve l'original et lance une tâche d'import annulable. Les questions
passent par `/api/message` avec `document_ids`, la file de conversation et le
routeur existants. Cette branche ne charge ni mémoire personnelle, ni outils,
ni autres documents. Un accord distant éventuel est celui de cette tâche.

## Extraction et distribution

`scripts/prepare-ocr-runtime.py --output chemin/ocr-runtime` construit sous
MSVC une bibliothèque Tesseract 5.5.3 avec Leptonica 1.87.0 statique. Le décodage
PNG/JPEG se fait dans Rust ; les codecs et outils d'entraînement inutiles,
libcurl et libarchive ne sont pas intégrés. PDFium 158.0.8111.0 et les modèles
français, anglais et orientation proviennent des versions/empreintes de
`spec/ocr_runtime.json`. Les licences et notices sont copiées dans le paquet.

Le pont `native/ocr/akasha_utf8.cpp` utilise l'API publique FileReader de
Tesseract pour lire les modèles via des chemins Windows Unicode. L'essai avec
un chemin accentué a détecté puis confirmé la correction d'un échec du lecteur
de fichiers standard. Son empreinte est aussi verrouillée.

Le constructeur produit `runtime-lock.json`, comprenant les empreintes de
chaque bibliothèque, modèle et notice. Sa propre empreinte doit être fournie
par `AKASHA_OCR_LOCK_SHA256` **pendant la compilation du daemon**. L'édition
consumer-beta refuse un moteur sans cette empreinte compilée ou avec des
fichiers altérés. L'édition de développement peut recevoir cette empreinte à
l'exécution. Les DLL sont chargées par chemin absolu, sans chercher dans le
répertoire courant ni dans PATH. Le compilateur peut changer les octets du
paquet : on qualifie et conserve le verrou du paquet réellement distribué.

Chaque page est extraite dans un processus court du même daemon, avant tout
chargement des réglages, modèles ou canaux de l'application. Son environnement
exclut les clés de services et le profil personnel. Le parent arrête le
processus lors d'une annulation ou après 60 secondes. Les limites contrôlées
sur le serveur sont 25 Mio/fichier, 100 pages, 20 fichiers/lot et deux pages
traitées simultanément. Les images sont aussi bornées en dimensions et pixels ; une expansion au-delà de 32 Mio de texte extrait par document est arrêtée et ses pages non lues sont signalées.
Cette séparation limite les pannes et leur durée ; ce n'est pas un bac à sable
de sécurité Windows pour du code natif hostile.

PDFium lit le texte page par page. Les pages sans texte et les grandes images
avec peu de texte passent en OCR. Une confiance faible, une page vide ou une
erreur restent visibles comme avertissements. Les PDF mixtes conservent les
deux méthodes. La détection reste heuristique : les mises en page atypiques
doivent être vérifiées dans les originaux.

## Sources, résultat et reprise

Les index v2 conservent document, page, positions Unicode, méthode, confiance,
empreinte de l'original et avertissement. `/api/user-rag/retrieve` conserve le
champ `chunks` et ajoute `passages`. Une réindexation remplace atomiquement
l'index, conserve la source et les identifiants. Un ancien index est identifiable
comme non localisé ; le bouton Relire le document crée un nouvel index.

La réponse doit référencer des numéros réellement fournis. La présence d'une
référence ne prouve pas à elle seule l'exactitude de l'interprétation : l'interface
invite à ouvrir la page originale. Pages non lues, informations déclarées
absentes et passages non utilisés restent signalés. Les sources ne deviennent
jamais des instructions ou des appels d'outils. Le Markdown produit neutralise
les images, liens et HTML fournis par le modèle avant l'export, pour empêcher
un chargement distant déclenché par leur affichage.

Le Markdown est un fichier effectivement écrit et vérifié par la tâche ; la
sauvegarde du profil inclut ce dossier. Le bouton PDF réutilise l'impression
existante : l'utilisateur choisit Enregistrer en PDF. L'ouverture de cette
fenêtre ne prouve pas qu'un PDF a été enregistré. Les résultats et références
restent consultables après redémarrage.

## Validation observée le 26 octobre 2026

- 735 tests natifs du daemon consumer-beta passent (un ignoré), puis huit contrôles ciblés après l’ajout de la limite d’expansion ; ils couvrent les limites, la cohérence des
  index, positions Unicode, manifeste corrompu et références inventées.
- 78 tests UI et compilation de production passent. Les deux parcours Chromium
  aux largeurs 1440 et 900 ouvrent la source du résultat.
- Onze essais du processus natif vérifient texte PDF, scan, PDF mixte, JPG/PNG,
  rotation, page blanche, texte accentué, refus de 101 pages et rendu de page.
- Neuf contrôles HTTP passent avec un vrai modèle local `qwen3.5:9b` : sept
  imports avec empreintes et positions, réponse/export malgré une injection
  documentaire, puis persistance après redémarrage. Le fournisseur contrôlé
  reproduit séparément ces contrôles pour la CI.

Les reçus datés sont dans `spec/dev/quality/evidence/`. Ce sont des essais de
développement, pas la qualification du paquet Windows, des matériels CPU/NVIDIA
ou du corpus complet de 50 cas. Les arrêts forcés, documents difficiles et PDF
effectivement enregistrés complètent le gate de distribution du lot 6.
