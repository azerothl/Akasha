# Vérification réelle de Recherche — ticket #186

Date : 19 octobre 2026. Base : `17c302ed9bdddf08031bd940f16ff30cd15e3edb` (PR #224 fusionnée).

## Protocole

- Interface existante construite en mode `e2e`, servie sur `127.0.0.1:4189`, avec son proxy vers le vrai daemon sur le port 3876. Aucun remplacement des réponses ni fixture de recherche.
- Daemon Windows compilé avec `cargo build -p akasha-daemon --no-default-features --features embedded -j 2`.
- Données isolées ; modèle Ollama déjà installé `qwen3.5:9b`, service dédié sur `127.0.0.1:11435`, contexte 16384, réflexion désactivée, délai fournisseur 240 s, aucun nouvel essai automatique. Le GPU est utilisé : `/api/ps` indique 6 047 945 194 octets en VRAM. Aucun modèle téléchargé.
- Recherche lancée par le formulaire, un tour maximum. Capture des phases, du rapport affiché et de sa restitution après rechargement depuis l’historique.

## Premier essai sur le code fusionné

Run `dr_077e051752f149698e9a370d64d9c196` : 138 s, 2735 mots, six pages retenues, dix résultats examinés, fournisseur DuckDuckGo. Les six liens ont été relus indépendamment : HTTP 200, titres et empreintes conservés dans [les contrôles des sources](validation/research-186-2026-10-19/before-source-checks.json).

L’interface affiche correctement **Rapport partiel**, six sources et l’avertissement de vérification ; ces informations et le contenu restent accessibles après rechargement. Cependant, le contenu comporte des erreurs : noms `push_back`/`pop_back` attribués à `Vec`, coûts aux deux extrémités généralisés, confusion entre longueur et capacité, garanties de croissance non documentées, citation d’une ancienne version pour une méthode ultérieure et lien absent du manifeste. Le diagramme imposé ajoute des coordonnées inventées et s’affiche comme du code.

Comparaison avec les sources primaires : [Vec](https://doc.rust-lang.org/std/vec/struct.Vec.html), [VecDeque](https://doc.rust-lang.org/std/collections/struct.VecDeque.html), [tableaux de complexité](https://doc.rust-lang.org/std/collections/index.html). Ces pages distinguent les méthodes, les coûts amortis et les garanties documentées. Un lien valide ne prouve pas que l’affirmation associée est étayée.

Preuves : [run JSON](validation/research-186-2026-10-19/before-research-run.json), [rapport original à ne pas utiliser comme référence](validation/research-186-2026-10-19/before-report.md), [rapport affiché](validation/research-186-2026-10-19/report-before-fix.png), [historique après rechargement](validation/research-186-2026-10-19/history-before-fix.png).

## Problèmes corrigés

1. La rédaction finale reçoit désormais les extraits détaillés, rattachés à chaque URL, et doit vérifier le brouillon contre ces éléments. Les tableaux d’extraits JSON sont conservés au lieu d’être ignorés au profit du résumé.
2. Les consignes excluent les détails non étayés, les transferts de propriétés entre options/versions, les coordonnées inventées et les diagrammes Mermaid non rendus. La longueur doit suivre les preuves disponibles.
3. Les liens Markdown vers des pages absentes du manifeste déclenchent une révision. S’ils subsistent, ils sont signalés et le rapport reste insuffisamment sourcé. Les ancres de section d’une page consultée sont acceptées ; les images ne comptent pas comme citations.
4. Les URL HTTP(S) explicites du sujet sont lues via le même outil `web_fetch`, avec sa politique de domaines, ses limites et ses contrôles de réponse. Leur nombre est borné et les doublons éliminés. Cela fonctionne même lorsque les moteurs ne fournissent aucun résultat.
5. La troncature des requêtes compte les caractères Unicode : un sujet français provoquait auparavant une panique en coupant un `é` entre ses octets.

## Limites observées pendant les essais

Après le premier essai, DuckDuckGo retourne HTTP 202 et une page de vérification anti-robot sans résultat ; le fournisseur de secours ne fournit pas de résultat exploitable. Aucun contournement n’a été effectué. Deux recherches terminent donc avec l’erreur d’absence de preuve, correctement affichée et conservée dans l’historique ([capture](validation/research-186-2026-10-19/no-evidence-after-fix.png)). La collecte suivante utilise les quatre URL officielles explicitement renseignées dans le formulaire, sans simuler leur contenu.

Les premiers essais locaux de compilation ont aussi rencontré le verrou Windows du binaire en cours d’exécution : le daemon de validation a été arrêté avant reconstruction. Cela ne constitue pas un échec de test fonctionnel.

## Contrôles automatisés

- `cargo test -p akasha-daemon --lib --no-default-features --features embedded deep_research -j 2` : **19 tests réussis**. Les nouveaux contrôles couvrent les extraits en chaîne/tableau, leur rattachement à l’URL, les références non consultées, les sources explicites bornées/dédupliquées et les frontières Unicode.
- `npm run test:e2e -- --grep 'research outcome'` : **5 parcours réussis** sur le correctif final (succès, partiel, vide, absence de preuve, annulation), après retrait de l’expérience de relecture. Ces tests utilisent des fixtures, distinctes des recherches réelles ci-dessous.
- [Sortie Rust](validation/research-186-2026-10-19/tests-deep-research.txt) · [Sortie UI](validation/research-186-2026-10-19/tests-ui-outcomes.txt).

## Résultat intermédiaire : limites du modèle 9B

L’essai après ajout de la relecture, `dr_be3bfa2e9dc24e70bf7a556bb6b93b7d`, dure 186 s et produit 1993 mots à partir des quatre pages lues directement. La relecture ne suffit pas à le valider : il généralise encore les coûts aux extrémités de `Vec`, décrit à tort plusieurs allocations pour le tampon circulaire et utilise des références abrégées sans définition. Ce résultat n’est **pas** accepté comme preuve positive exploitable. Le modèle ne respecte pas non plus entièrement les consignes de longueur et de citation. Les garde-fous déterministes sur les URL ne prouvent pas la véracité des phrases.

Un essai distinct utilise ensuite le modèle local déjà installé `qwen3:14b` sur le même service isolé, contexte 16384, réflexion désactivée. `/api/ps` indique 11 682 688 531 octets en VRAM. Le sujet demande cette fois une réponse ciblée de 400 mots maximum, les mêmes critères et les quatre mêmes documents. Le changement de modèle et de formulation est explicite ; ce contrôle ne permet pas de déclarer tous les modèles équivalents ni de garantir leurs futures réponses.

## Dernier essai réel : modèle 14B

Run `dr_c0f8ad2d4bfc4e209d8f15167d55724c` : **446 s, 683 mots, quatre pages lues, aucun résultat supplémentaire de moteur**. Les quatre URL sont accessibles (HTTP 200), leurs titres et empreintes sont dans [source-checks.json](validation/research-186-2026-10-19/source-checks.json). Le rapport est signalé partiel ; le nombre de sources et les limites restent visibles après rechargement, puis sélection de l’entrée dans l’historique, y compris une fois le daemon de validation arrêté.

Preuves : [run complet](validation/research-186-2026-10-19/run.json), [rapport non validé](validation/research-186-2026-10-19/report.md), [modèle effectivement chargé](validation/research-186-2026-10-19/model-14b.json), [rapport affiché](validation/research-186-2026-10-19/report-14b-not-validated.png), [après rechargement](validation/research-186-2026-10-19/history-14b-not-validated.png), [entrée de l’historique sélectionnée](validation/research-186-2026-10-19/history-selected-14b.png).

**Vérification défavorable du contenu :** le tableau nomme encore `push_back`, `push_front`, `pop_back`, `pop_front` dans une colonne `Vec` ; ce ne sont pas ses méthodes. Les opérations en tête doivent être distinguées des méthodes `push` et `pop` de fin. Le tableau omet aussi le caractère amorti des insertions à indice arbitraire. La citation de la page du module `vec` pour les opérations de tête n’étaye pas à elle seule cette ligne. Le rapport dépasse les 400 mots demandés. Les passages corrects, liens valides et avertissements visibles ne suffisent pas à accepter tout le résultat.

## Expérience retirée et décision

Une relecture supplémentaire par modèle a été essayée et apparaît dans les traces des essais 9B/14B ci-dessus. Elle **n’est pas livrée dans le correctif final** : les sorties réelles montrent qu’elle ne corrige pas fiablement les faits ni les noms d’API. Elle ajoutait un appel et un état sans bénéfice suffisamment établi. Le message UI et le sixième parcours de test propres à cette expérience ont également été retirés ; les cinq parcours existants restent contrôlés. Les traces expérimentales sont conservées pour expliquer ce choix, sans les présenter comme le comportement du correctif final.

Le correctif final conserve les réparations déterministes (Unicode, URL explicites via la politique de collecte, conservation des extraits, références hors manifeste signalées) et des consignes de rédaction plus strictes. Il ne prétend **pas** résoudre la fiabilité factuelle générale d’un LLM. La CI distante et la livraison de ces changements restent à effectuer.

**Ticket #186 maintenu ouvert.** La restitution réelle, les références accessibles et l’historique sont observés, mais aucun des rapports examinés n’a été entièrement validé sur le fond. Le dernier critère ne doit pas être coché. Il reste à obtenir un rapport exploitable sans ces erreurs, à vérifier ses affirmations contre les sources et à livrer les correctifs validés. Aucun rapport n’a été réécrit à la main pour fabriquer une réussite.

## Suite à la revue de la PR #225

Trois retours corrigés : les sources explicites sont désormais collectées même si la recherche web est désactivée ; les URL avec parenthèses équilibrées, notamment les destinations Markdown et les chemins imbriqués, sont conservées lors de la collecte et de la vérification des citations. Une URL contenue dans le paramètre d’une autre URL n’est pas interprétée comme une source supplémentaire.

Le contrôle de collecte utilise un serveur HTTP local de test, sans fournisseur LLM : il constate la requête quand le domaine est autorisé et son absence quand ce même domaine est bloqué, avec `web_search_enabled: false` dans les deux cas. Les notes sans source ne sont pas ajoutées lorsqu’une preuve web a effectivement été obtenue. Les scénarios historiques sans moteur de recherche et sans URL restent possibles.

`cargo test -p akasha-daemon --lib --no-default-features --features embedded deep_research -j 2` : **22 tests réussis** sur cette correction. [Sortie du contrôle](validation/research-186-2026-10-19/ci-review-tests.txt). Il s’agit de régressions déterministes, sans nouvelle prétention de validation factuelle des rapports réels.

### Deuxième passe de revue : contexte et pages réellement lues

- Les extraits sont limités à 480 octets UTF-8 par source, le manifeste à 2000 octets. Les URL sont conservées entières ; les autres sources omises sont signalées. Ces limites concernent le prompt, pas les données complètes du run en mémoire.
- Le sujet, le brouillon, le profil et les consignes de révision ont également des budgets. Le prompt complet est contrôlé à 5632 octets UTF-8 maximum, avec 2048 tokens de sortie et une marge de 512 pour les instructions système et le format des messages : enveloppe conservatrice pour le contexte Ollama usuel de 8192 tokens. Le but de longueur est adapté au budget de sortie (au plus 1024 mots), sans imposer du remplissage. Un contexte configuré plus petit peut encore refuser la requête ; il ne provoque plus de faux rapport final.
- Si la génération initiale ou la révision échoue, ou si le résultat est vide, le run passe en erreur avec un message FR/EN. Il ne retourne plus silencieusement le brouillon ni un faux rapport de remplacement. Les étapes de rédaction passent en erreur ; les éléments déjà collectés sont conservés dans le run.
- La suffisance se calcule sur le nombre de pages effectivement retenues. Une collecte de pages explicites ne reste plus marquée « connaissances du modèle sans Web » parce que le moteur de découverte est désactivé. L’absence de preuve et le faible nombre de pages restent signalés séparément. Une omission liée au contexte a son propre avertissement.
- Régressions : 100 sources volumineuses et Unicode, bornes du manifeste et des deux prompts, URL non tronquées, erreurs/vides non exportables, et seuil de 12 pages indépendant de la découverte. Contrôle ciblé final : **25 tests Rust réussis**, **6 parcours UI réussis**, dont le nouvel échec de rédaction. Ces tests restent des fixtures, pas une validation factuelle des rapports réels.

### Troisième passe : provenance de la collecte directe

Le drapeau `search_degraded` conserve l’indisponibilité de la découverte, même quand des pages explicites ont été lues. Il ne détermine plus à lui seul que le rapport manque de preuves : la suffisance des pages est indépendante. Le client et les exports distinguent les pages consultées des connaissances sans source. Aucun fournisseur Brave n’est inventé quand la liste est vide ; l’export indique la lecture directe, et conserve un avertissement de couverture si les pages restent insuffisantes. Le statut de découverte en cours ne prétend plus que le modèle est la seule source.

Contrôles locaux réussis : **26 tests Rust Recherche**, **6 tests unitaires UI ciblés** (qualité et export FR/EN) et **7 parcours UI**, dont le résultat sourcé avec découverte indisponible. Les messages d’absence de preuve et les rapports réellement insuffisants gardent leurs états distincts. Ces contrôles sont des fixtures et ne changent pas la décision de maintien ouvert de #186 sur la fiabilité factuelle des rapports réels.

### Quatrième passe : budget partagé et citations réalisables

Le manifeste réserve les URL complètes et un extrait minimum avant de partager le budget restant entre les pages sélectionnées. Une collecte usuelle de 12 sources conserve ainsi ses 12 représentations malgré des extraits longs. Les URL exceptionnellement longues peuvent encore imposer une sélection, en ordre de collecte et sans biais alphabétique ; les sources omises restent signalées. Les contrôles de citation et de longueur utilisent le nombre de sources réellement transmises au rédacteur, tandis que les métadonnées conservent la collecte complète et son seuil de suffisance. La révision reçoit le même manifeste et une cible de citations compatible.

**27 tests Rust Recherche réussis**, dont les cas à 12 sources et à URL volumineuses : [sortie du contrôle](validation/research-186-2026-10-19/ci-manifest-tests.txt). Aucune nouvelle recherche réelle ni validation factuelle générale.

### Cinquième passe : références Markdown résolues

Le contrôle des citations utilise désormais le parseur CommonMark `pulldown-cmark` 0.9, déjà employé dans le workspace. Il résout les liens par références explicites, raccourcies ou implicites et les autoliens ; les images, définitions inutilisées et exemples de code ne comptent pas comme citations. Une destination non consultée reste détectée avec ce format. **28 tests Rust Recherche réussis** : [sortie du contrôle](validation/research-186-2026-10-19/ci-reference-tests.txt).

### Sixième passe : conserver une rédaction valide si la révision facultative échoue

Précision du comportement de la deuxième passe : la première rédaction reste obligatoire et son échec demeure une erreur. Une révision motivée uniquement par la longueur ou le nombre de citations peut en revanche échouer sans effacer ce rapport final ; ses avertissements de qualité restent calculés. Si la révision est nécessaire à cause de citations non consultées, son échec ou sa réponse vide demeure bloquant. Aucun brouillon de synthèse intermédiaire ne remplace une rédaction finale manquante. **29 tests Rust Recherche réussis**, avec succès, vide et absence de révision dans les deux modes : [sortie du contrôle](validation/research-186-2026-10-19/ci-revision-tests.txt).

### Septième passe : liens HTML et URL nues du rendu

Le contrôle extrait aussi les ancres du HTML rendu (scraper 0.27) et les URL nues GFM dans le texte (linkify 0.11). Les entités HTML, attributs non cités, variantes de casse et liens sans schéma `//` sont traités ; les préfixes `www.` correspondent aux liens HTTP du rendu. Le code et les images ne sont pas des citations. Les URL comparées sont canonisées avant retrait du fragment. **30 tests Rust Recherche réussis**, dont une régression combinant ces formes, les sources connues et les exemples exclus : [sortie du contrôle](validation/research-186-2026-10-19/ci-renderer-tests.txt). Les API ont été vérifiées dans leurs documentations officielles : https://docs.rs/scraper/latest/scraper/ et https://docs.rs/linkify/latest/linkify/.

### Huitième passe : édition du parseur HTML

La dépendance finale est fixée à `scraper = 0.18.1`, sans fonctionnalités par défaut. Son manifeste téléchargé déclare l’édition 2021 ; la version 0.27 de la passe précédente exigeait l’édition 2024 (Rust 1.85). Le correctif retire cette exigence supplémentaire et ne relève pas le minimum annoncé du projet. Les fonctionnalités CLI inutiles du parseur sont désactivées. **30 tests Rust Recherche réussis** avec cette version sous le toolchain stable installé : [sortie du contrôle](validation/research-186-2026-10-19/ci-parser-compat-tests.txt). La compilation de tout le workspace sous Rust 1.70 n’a pas été effectuée ; cette correction ne constitue pas une certification de toutes ses dépendances sur cet ancien compilateur.
