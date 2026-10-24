# Mises à jour incrémentales des distributions natives

**Statut :** V1 implémentée dans le code ; validation locale des composants,
de la confirmation, des hashes et du rollback sous Windows PowerShell 5.1.
La validation d’une mise à niveau entre deux releases publiées reste à effectuer.

## Problème

Les anciennes versions de `akasha update install` ouvraient seulement la page de
téléchargement. L’utilisateur devait récupérer l’archive complète, y compris les
bibliothèques CUDA inchangées. La V1 met à jour le moteur CLI/daemon/TUI sur place.

## Objectif V1

Mettre à jour sur place les distributions Windows x86_64 CPU et CUDA, sans
télécharger l’archive ZIP complète à chaque version. Le daemon vérifie les
versions au plus une fois par jour lorsqu’il est en ligne, puis télécharge et
vérifie en arrière-plan les seuls fichiers différents. L’utilisateur confirme
l’application et le redémarrage du daemon.

L’application desktop Tauri conserve son propre cycle de mise à jour. Linux et
macOS pourront reprendre le même contrat après validation du parcours Windows.

## Contrat de publication

Pour chaque variante Windows, `scripts/ci/package_updates.py` publie un manifeste
et des ZIP par composant : CLI, daemon, TUI, chaque DLL CUDA et les ressources.
Le manifeste décrit au minimum :

- la version, la plateforme et la variante CPU ou CUDA ;
- le chemin relatif à la racine d’installation ;
- l’URL de l’asset, sa taille et son SHA-256.

Les assets couvrent les exécutables, les DLL nécessaires à la variante et les
ressources livrées avec le paquet. `akasha-install.json` fixe la variante et le
flux public GitHub Releases du dépôt `Akasha_app`, indépendamment du dépôt source
qui peut être privé. Un serveur personnalisé via `--api-url` doit exposer une
table `update_manifests` dans `api/latest.json`. Les archives ZIP restent disponibles
pour les nouvelles installations. Les ressources constituent un seul composant ;
elles peuvent être retéléchargées ensemble lorsqu’un fichier de ce groupe change.

Le job `deploy_site` publie les composants dans un tag public `updates-vX.Y.Z`
de `Akasha_app`, puis actualise les manifestes du tag `updates-stable` avant
d’annoncer la release au site. Ces deux releases techniques sont marquées prerelease
et ne remplacent pas la dernière release utilisateur. Le site conserve son workflow
de création des releases `vX.Y.Z` avec les ZIP d’installation. Configurer le secret
`AKASHA_UPDATE_PUBLISH_TOKEN` avec `contents:write` sur ce dépôt ; le token de dispatch
existant peut aussi servir s’il possède ce droit. Sans token, le job signale que le
flux public n’a pas été publié et les clients gardent leur installation actuelle.

## Parcours utilisateur

1. Le daemon vérifie le manifeste de sa variante au plus une fois par jour et publie le
   statut dans le CLI et le statut de mise à jour déjà utilisé par l’UI.
2. Il identifie la variante CPU ou CUDA installée, récupère son manifeste et
   compare les hashes locaux. Seuls les fichiers absents ou différents sont
   téléchargés dans un répertoire temporaire.
3. Chaque téléchargement est vérifié avec SHA-256 avant de signaler que la
   mise à jour est prête.
4. `akasha update install` ou l’action de l’UI demande confirmation, puis un
   helper séparé attend la fermeture du CLI, arrête le daemon proprement,
   remplace les fichiers concernés et redémarre le daemon.
5. En cas d’échec de copie ou de vérification, les anciens fichiers sont
   restaurés et le daemon peut être relancé. Une absence de réseau ne bloque
   jamais le démarrage normal.

Le journal `.akasha-update/transaction.json` enregistre les sauvegardes avant
chaque remplacement. Une interruption pendant l’application est présentée comme
une mise à jour à récupérer ; la prochaine installation confirmée restaure d’abord
la transaction interrompue. Les exécutables d’autres installations ne sont pas arrêtés.

La configuration, les bases locales et les modèles téléchargés dans `data_dir`
ne font pas partie de la mise à jour. Une DLL CUDA déjà à jour n’est pas
téléchargée. Si un exécutable change, cet exécutable doit être téléchargé en
entier en V1 ; le patch binaire reste une optimisation ultérieure.

## Sécurité et compatibilité

- N’accepter que des URLs HTTPS issues du manifeste de release attendu.
- Vérifier le SHA-256 de chaque asset avant le remplacement.
- Ne jamais écraser un fichier actuellement utilisé : le helper applique la
  mise à jour après l’arrêt du processus concerné.
- Conserver une sauvegarde jusqu’au redémarrage réussi, puis la supprimer.
- Si le manifeste est absent, invalide ou incompatible, ne rien modifier et
  afficher une commande de téléchargement manuel.
- Les versions antérieures à la V1 ne possèdent pas le helper ; leur première
  mise à niveau vers une version compatible demandera une installation normale.

## Critères d’acceptation

- Une mise à niveau Windows CPU et CUDA fonctionne depuis la version précédente
  sans retélécharger le ZIP complet.
- Le journal de téléchargement démontre que les fichiers dont le hash est déjà
  conforme ne sont pas demandés au serveur.
- Une modification de la CLI seule ne télécharge pas le daemon ni les DLL CUDA ;
  une modification du daemon ne télécharge pas les DLL inchangées.
- Une interruption réseau, un SHA-256 erroné ou un échec de remplacement laisse
  l’installation précédente récupérable.
- Le daemon redémarre après une mise à jour réussie ; `data_dir`, le GGUF et les
  configurations locales restent inchangés.
- Les archives complètes et le parcours d’installation manuelle continuent de
  fonctionner sans le mécanisme d’update.

## Vérification reproductible

`python -m unittest discover -s scripts/ci/tests -p test_package_updates.py -v`
vérifie les inventaires, les hashes et les archives de composants. Sous Windows,
ce test lance `scripts/ci/test_update.ps1` avec Windows PowerShell 5.1 sur des
fichiers temporaires : téléchargement sélectif et réutilisation du staging,
refus de confirmation, asset corrompu, ZIP/path invalide, remplacement partiel,
redémarrage défaillant, reprise du journal et lancement réel du helper détaché.
Il ne lance pas les exécutables factices du fixture.

`cd apps/akasha-ui && npx playwright test --config playwright.update.config.ts`
vérifie le refus/l’acceptation de la confirmation, l’unicité de l’installation et
le passage du téléchargement à l’état prêt avec IPC et daemon simulés.

Les installations Windows peuvent suspendre les téléchargements automatiques en
définissant `AKASHA_AUTO_UPDATE=0` pour le daemon ; les commandes explicites
`akasha update check` et `akasha update install` restent disponibles.
