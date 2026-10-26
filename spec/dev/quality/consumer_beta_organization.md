# Organisation, veille et récupération du profil

Le lot 5 réutilise `/api/message`, les tâches, le scheduler et les notifications
du daemon. Il ne lance pas un deuxième moteur d’agents.

## Comportements livrés

- Une note existante ou collée produit des propositions. Aucun rappel ni action
  n’est enregistré avant confirmation. Le contenu de la note est figé et vérifié
  à nouveau avant création. Les heures ambiguës ou inexistantes lors d’un
  changement d’heure sont refusées. Une transaction conserve les mêmes actions
  et rappels lors d’une confirmation répétée.
- Les rappels réellement persistés sont consultables, cochables et supprimables.
  Leur déclenchement produit une notification locale, sans appel de modèle. Une
  échéance passée pendant l’arrêt est signalée comme tardive. Les intervalles
  manqués reprennent au maximum une occurrence ; les anciennes règles RRULE
  utilisent un curseur de reprise et un décompte minimal explicitement indiqué.
- Une veille possède sujet, sources et fréquence immuables, plus ses propres
  accords révocables et expirants. Elle transmet uniquement le sujet limité au
  domaine et l’adresse des pages choisies. Le moteur existant fournit des extraits,
  sans synthèse LLM. Chaque rapport distingue nouveautés, déjà-vus et sources
  indisponibles. Si aucune source n’est utilisable, la tâche échoue explicitement
  et le rapport d’erreurs reste consultable. Modifier les destinataires demande de
  créer une nouvelle veille et de nouveaux accords.
- La correction d’un souvenir conserve identifiant et provenance, recalcule son
  embedding, retire les anciens faits et liens dérivés. Un extracteur commencé
  avant la correction ne peut pas réinsérer ses faits périmés. La suppression
  d’une entrée ne supprime pas les conversations d’origine.
- Les sauvegardes incluent les historiques, notes, sources documentaires,
  rapports, actions, rappels, mémoire et réglages de profil et préférences d’affichage. Les routes des
  modèles et réglages de recherche sont conservés après retrait des identifiants.
  Les adresses de services sont ramenées à leur origine pour exclure les secrets
  éventuellement placés dans un chemin ou une requête ; les intégrations
  personnalisées doivent être reconfigurées. Les comptes calendaires restaurés
  sont désactivés. Les clés du coffre, accords distants, poids, caches et journaux
  sont exclus.
- L’application Windows propose sélection et export de l’archive avec les boîtes
  de dialogue natives. Une restauration est vérifiée dans un dossier isolé,
  reçoit une nouvelle clé de mémoire et désactive tâches/rappels/récurrences.
  Une seconde confirmation sélectionne ce profil pour le prochain démarrage.
  L’ancien profil reste présent ; une destination absente ou endommagée bloque
  la sélection et ne crée aucune base de remplacement.

## Preuves observées le 26 octobre 2026

| Vérification | Résultat observé |
|---|---|
| Daemon particuliers, suite complète sans moteur embarqué | 743 réussites, 1 test PTY ignoré |
| Stockage SQLCipher, suite complète | 37 réussites |
| Outils, suite complète après recherche structurée et limite de lecture Web | 39 réussites |
| Interface React | 83 réussites ; compilation réussie |
| Pont natif d’export de sauvegarde | 2 réussites ; copie exacte et refus du texte clair sans remplacement |
| Daemon avec `consumer-beta,embeddings-tract` | Compilation réussie, y compris correction avec réembedding |
| Parcours HTTP avec fournisseur contrôlé | 9/9 : propositions, refus, unicité, archive, restauration et redémarrage |
| Même parcours HTTP avec `qwen3.5:9b` local | 9/9 |
| Chromium, organisation/veille/sauvegarde en 1440 et 900 px | 2 parcours réussis ; captures examinées |

Les reçus HTTP sont conservés sous `evidence/consumer-organization-*-2026-10-26.json`.
Chaque essai possède sa propre copie du daemon, son port et son profil temporaire.
Le test de veille utilise un vrai serveur HTTP synthétique et compte les requêtes
avant accord, après accord et après révocation. Le stockage vérifie séparément
la notification unique et les faits dérivés.

Les 9 cas HTTP ne constituent pas le corpus complet de 50 cas, ni trois passages
de qualification matérielle. Les dialogues natifs n’ont pas été pilotés depuis
un installateur distribué. La restauration sur un autre compte Windows reste à
éprouver physiquement ; le test de portabilité qui efface la clé d’origine ne
remplace pas cet essai. La campagne auprès des dix personnes n’a pas commencé.
