# Consentement et protection de la bêta particuliers

Ce lot ajoute un socle de protection. Il ne constitue pas une autorisation de
diffuser la bêta avec des données personnelles : les parcours, le paquet et les
essais de restauration sur un autre compte Windows restent à qualifier.

## Binaire et périmètre

Le chiffrement est activé par la fonctionnalité Cargo `consumer-beta` du daemon.
Les binaires développeur existants conservent leur compatibilité avec les bases
non chiffrées. Le premier démarrage du binaire particuliers initialise la protection
même lorsque les embeddings ne sont pas chargés ; un échec du Gestionnaire
d’identifiants est exposé comme mémoire verrouillée. Une base munie du marqueur `memory.db.protection.json` exige le
support SQLCipher et sa clé ; une ouverture ne crée jamais de base de remplacement.
Un ancien binaire SQLite échoue sur le fichier chiffré au lieu d'en lire les tables.

La clé aléatoire de 256 bits passe exclusivement par le Gestionnaire
d'identifiants. Le coffre à phrase par défaut n'est jamais utilisé pour cette
clé. L'API indique `locked`, `migration_required`, `not_initialized` ou `encrypted`.
Les tables, index et embeddings du fichier mémoire partagent la même ouverture
SQLCipher dans tous les lecteurs du magasin.

Le paquet particuliers utilise une liste positive d’outils et refuse l’écriture
arbitraire de fichiers par les agents, afin qu’un document ne puisse pas leur
faire réécrire les autorisations. Il désactive les transports opaques des outils avancés
(commandes système, navigateur piloté, MCP, extensions, images/voix distantes et
messagerie), les canaux de messagerie entrants, la réplication et les hooks
système. Ils ne peuvent pas bénéficier implicitement d'un accord de recherche
Web. Cette limite concerne la bêta particuliers ; la distribution développeur
reste distincte. Le lot de distribution doit vérifier cette sélection effective.

## Accords distants

Chaque appel de modèle enregistré dans le routeur est protégé avant le
transport. L'identité contient le fournisseur et son URL effective, sans
identifiants ni paramètres secrets. Une URL sur le réseau local n'est pas un
modèle exécuté sur cet ordinateur. Seules les adresses de boucle locale sont
traitées comme locales pour les moteurs qui annoncent une exécution locale.
Un adaptateur cloud ne devient pas local du seul fait de cette adresse. Ollama peut néanmoins relayer un modèle distant derrière
une adresse locale : ses métadonnées `remote_host` et `remote_model` sont
contrôlées avant d’envoyer le contexte, y compris pour un alias renommé.
Un destinataire cloud exige son propre accord ; des métadonnées absentes
bloquent l’envoi. Voir le [contrat API Ollama](https://github.com/ollama/ollama/blob/main/docs/openapi.yaml).
Les redirections HTTP sont désactivées.

Un accord porte sur une tâche racine (partagé avec ses enfants et tentatives)
ou sur une récurrence. Il précise le destinataire, les catégories de données,
une échéance et sa révocation. Un appel de modèle distant exige les catégories
message, historique, mémoire, documents et résultats d'outils, car le chat peut
assembler ces contextes. L'interface les annonce avant le choix. Les futurs
parcours documentaires devront encore limiter cette sélection aux sources
choisies. Une clé API existante ne produit aucun accord.

La recherche dispose d'accords séparés par moteur. Elle envoie son argument de
requête, sans joindre automatiquement les pièces jointes ou la mémoire au
transport du moteur de recherche. La minimisation sémantique de cet argument
doit encore être qualifiée avec les parcours réels. Lire une page exige un
accord sur l'origine de cette page. Un refus conserve un reçu de destinataire
et catégories, sans contenu. L'utilisateur peut autoriser ce destinataire dans
le centre de permissions, puis reprendre la même tâche depuis Tâches.

API additives : `GET /api/privacy`, `POST /api/permissions/remote` avec
`confirmed:true`, `scope`, `consents` et `hours`, puis
`POST /api/permissions/remote/revoke`. `/api/message` accepte `remote_consents`
pour un choix explicite effectué avant la création de sa tâche. Le choix du
composeur n'est pas conservé pour l'envoi suivant.

## Sauvegarde et migration

`POST /api/backup/create`, `/api/backup/restore` et `/api/memory/protection`
créent des tâches de maintenance suivies par les API de tâches existantes.
La phrase ne figure ni dans la tâche ni dans les événements. La restauration
et la conversion exigent `confirmed:true`. Le mot de passe comporte au moins
12 caractères. Les opérations sont sérialisées et contrôlent l'annulation avant
la bascule. Une bascule déjà effectuée n'est pas annulée rétroactivement.

Le format versionné `.akbk` utilise Argon2id (64 Mio, trois passes, un lane) et
AES-256-GCM avec sel et nonce aléatoires, en-tête authentifié, tailles bornées et
empreintes des fichiers. La limite actuelle est de 700 Mio de données de profil
et 10 000 fichiers. Les instantanés SQLite passent par l'API de sauvegarde.
L'archive complète est déchiffrée et contrôlée avant préparation de la
restauration dans un dossier isolé. Une copie de mémoire reçoit une nouvelle
clé de compte Windows. Le profil actif n'est pas remplacé ; les récurrences
restaurées sont désactivées et les tâches interrompues ne sont pas rejouées.

Sont inclus les bases d'historique et mémoire, notes, documents importés et les
réglages de profil explicitement listés dans `profile_backup.rs`. Les réglages
de services sont nettoyés des identifiants ; autorisations distantes, secrets,
modèles, caches et journaux sont exclus. Le lot 5 ajoute le choix visuel du
profil restauré, la séparation des identifiants Windows entre profils et la
neutralisation des rappels/connexions externes restaurés.

La conversion d'une ancienne mémoire effectue un export SQLCipher vers une
nouvelle base, préserve `user_version`, vérifie l'intégrité et ferme les
connexions avant renommage. Une sauvegarde chiffrée vérifiée précède cette
conversion. Le marqueur est écrit avant la bascule pour que toute interruption
verrouille la mémoire. Les fichiers de reprise ne sont pas supprimés en cas
d'erreur ; le magasin sait reprendre les deux étapes de renommage interrompues.
La reprise HTTP d'une conversion interrompue utilise l'identifiant de son
archive, sa phrase et l'empreinte de la source vérifiée. L'interface propose
cette reprise pour les archives conservées ; aucune base active n'est remplacée
par une archive dont la source ne correspond pas.

## Vérification reproductible

Sous PowerShell, dans le dépôt :

```powershell
. ./scripts/ci/prepare-sqlcipher-windows.ps1
cargo test --locked -p akasha-store --features memory-sqlcipher --lib
cargo test --locked -p akasha-vault --lib protected_archive
cargo test --locked -p akasha-daemon --no-default-features --features consumer-beta --lib
cd apps/akasha-ui
npm test
npm run build
npx playwright test --config playwright.privacy.config.ts
```

Le test de restauration efface la clé d'origine avant de restaurer le souvenir
depuis l'archive, puis contrôle son identifiant et son contenu. Cela éprouve la
portabilité de l'archive ; cela ne remplace pas un essai sur un second compte
Windows ni depuis l'installateur distribué. Les captures Chromium sont des
tests d'interface avec services contrôlés, distincts des preuves natives du
magasin et du daemon. Aucun résultat de modèle réel ni pilote n'est déduit de
ces tests.

La revue automatique de la PR a conduit à inclure `short_term/`, à neutraliser
les wakeups restaurés, à transmettre le périmètre au collecteur de sous-agents
et à séparer un refus d’autorisation des pannes de fournisseur. Les tests de
reprise vérifient l’utilisation immédiate après accord, sans délai de santé.
Les lectures personnelles de l’API du binaire particuliers refusent également
les origines de navigateur étrangères et opaques ; le pont natif reste accepté.
