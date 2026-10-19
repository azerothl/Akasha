# #186 — vérification factuelle réelle, 19–20 octobre 2026

Suite de la [PR #233](https://github.com/azerothl/Akasha/pull/233) fusionnée dans `main` (`a77a3fb`). Code applicatif contrôlé : `8ecb9ec` ; la régression UI supplémentaire est dans `8836673` et ne modifie pas l’application.

## Causes et corrections

- La cible de rédaction ignorait le maximum de mots demandé. Les maxima explicites français/anglais sont reconnus ; la cible est plafonnée à 75 % du maximum. Un dépassement déclenche une reprise même avec peu de sources, puis reste signalé s’il persiste. Aucun passage n’est coupé arbitrairement.
- Les formats imposaient des verdicts et sections redondantes, favorisant le remplissage. Le sujet et sa longueur passent avant le format de catégorie ; les consignes interdisent les mécanismes et garanties non documentés. Température de rédaction/reprise : zéro, sans prétendre que cela prouve la fidélité.
- Un en-tête de tableau pouvait être sélectionné sans ses données. Le programme récupère les lignes nommées dans le sujet et la notation voisine directement dans la page originale. Sans cible littérale, toutes les lignes du tableau sont conservées. Les qualifications et cellules restent groupées dans le budget ; les paragraphes généraux restent des unités séparées. Aucun coût ni nom d’API n’est inventé ou codé en dur pour Rust.
- Avant publication, des lots bornés de paragraphes sont examinés contre les seuls passages originaux. Le modèle retourne des avis et identifiants, jamais du texte de correction à publier. Un avis manquant, dupliqué ou hors plage échoue explicitement. Les paragraphes rejetés sont retirés ou remplacés par des passages originaux ; les assertions de complexité et d’amortissement utilisent ces citations exactes même si le modèle les approuve. Cela couvre le faux positif observé sur `push`/`pop` sans faire confiance à une nouvelle reformulation.
- Le rapport supprime les titres devenus vides, conserve les tableaux lisibles avec leurs colonnes, déduplique les extraits de remplacement et respecte le plafond par blocs complets. La phase de contrôle est visible dans le suivi. Les remplacements/omissions sont signalés dans les avertissements.

**Limite :** l’avis sémantique du modèle peut encore avoir des faux positifs sur la prose conservée ; ce n’est pas un certificat universel de vérité. Les citations peuvent rester dans leur langue originale. La validation ci-dessous associe contrôle des citations et relecture indépendante de toute la prose restante.

## Vérifications automatisées

- [47 tests Rust Recherche réussis](validation/research-186-factual-2026-10-20/tests-final.txt) : budget UTF-8, maxima/minima, tableaux/qualifications, en-tête seul, lignes récupérées dans la source, faux avis positif sur les coûts, contrôle incomplet, absence de résultat vide, limite par passages complets et annulation.
- [Compilation du daemon Windows réussie](validation/research-186-factual-2026-10-20/build-final.txt). Les avertissements préexistants ne sont pas présentés comme nouveaux échecs.
- [12 parcours UI Recherche réussis](validation/research-186-factual-2026-10-20/tests-ui-research.txt), avec TypeScript/Vite, puis [1 nouvelle régression du tableau cité réussie](validation/research-186-factual-2026-10-20/tests-ui-table.txt). Cette dernière vérifie en-têtes, trois lignes, cellule amortie distincte de la cellule non amortie, légende et lien. **13 parcours au total sur deux exécutions**, avec fixtures identifiées ; ils ne constituent pas une validation factuelle d’un modèle.

## Protocole réel et essais conservés

Interface normale sur `127.0.0.1:4190`, daemon reconstruit sur `127.0.0.1:3876`, données isolées dans `E:\tmp\akasha-186-factual-20261019`, Ollama propre sur `127.0.0.1:11435`. Modèle déjà installé `qwen3.5:9b`, contexte 8192, réflexion désactivée, plafond fournisseur 1536 tokens, délai 600 s. Aucun modèle téléchargé. [Routeur du dernier contrôle](validation/research-186-factual-2026-10-20/llm_router.yaml) · [Politique](validation/research-186-factual-2026-10-20/tools_policy.yaml).

Sujet identique aux rapports refusés : comparaison Vec/VecDeque, mémoire, accès indexé, opérations aux deux extrémités, complexités amorties et usages, en réponse courte (400 mots maximum). Les quatre URL officielles sont fournies explicitement. La découverte est désactivée et seul `doc.rust-lang.org` est autorisé ; les pages sont réellement lues par `web_fetch`, sans fixture ni remplacement de réponse. Ce contrôle ne mesure pas la disponibilité des moteurs de découverte.

| Code / run | Résultat observé | Décision |
| --- | --- | --- |
| `2c4db6a` / `dr_59351ae446dc437ca3d8e2fc3260a0fa` | CPU, 484 s, 370 mots ; contiguïté attribuée aux deux types et qualifications `push`/`pop` encore fusionnées | Refusé : [run](validation/research-186-factual-2026-10-20/run-cpu.json), [rapport brut](validation/research-186-factual-2026-10-20/report-cpu.md) |
| `a13f7d8` / `dr_1e7ec1b4ae4b4add9ffa756478027727` | GPU, 37 s, 187 mots ; source-table trop volumineuse, titre vide | Incomplet : [run](validation/research-186-factual-2026-10-20/run-a13f7d8-incomplete.json), [rapport brut](validation/research-186-factual-2026-10-20/report-a13f7d8-incomplete.md) |
| `f2474a2` / `dr_f4d3110d265a48df972a91c4e461e984` | GPU, 35 s, 381 mots ; en-tête extrait sans données | Incomplet : [run](validation/research-186-factual-2026-10-20/run-f2474a2-incomplete.json), [rapport brut](validation/research-186-factual-2026-10-20/report-f2474a2-incomplete.md) |
| `8ecb9ec` / `dr_728e0b2688cb43e7b1b01990d33fc24a` | GPU configuré, 203 s, 368 mots, quatre pages, tableau avec les deux lignes et légende | Fidélité factuelle contrôlée : [run brut](validation/research-186-factual-2026-10-20/run-final.json), [rapport brut](validation/research-186-factual-2026-10-20/report-final.md) |

Une première tentative GPU a aussi été annulée pendant la planification ([trace](validation/research-186-factual-2026-10-20/run-gpu-cancelled.json)). Les contrôles GPU et CPU ont été alternés selon la charge. Un autre service Ollama a repris une requête GPU pendant le dernier essai ; ses slots étaient actifs, il n’a pas été déchargé. L’essai avait déjà atteint quatre lectures et a finalement terminé. Aucun modèle externe n’a été arrêté ; ces durées ne sont pas un benchmark comparatif CPU/GPU.

## Relecture indépendante du dernier rapport

Les quatre pages officielles répondent HTTP 200, documentation Rust 1.99.1 ; les [empreintes et horodatages](validation/research-186-factual-2026-10-20/source-checks.json) identifient les instantanés relus. Une cinquième page sur `Iter` sert uniquement à la relecture indépendante, pas au compteur du run.

Le [contrôle reproductible des citations](validation/research-186-factual-2026-10-20/audit-source-quotes.py) vérifie les empreintes des HTML indépendants puis compare les citations/cellules au texte des pages, hors séparateurs de mise en page Markdown. [Résultat : 37 passages ou cellules, aucun écart](validation/research-186-factual-2026-10-20/quote-audit.json). Les HTML restent dans le dossier temporaire nommé ci-dessus (`source-0.html` à `source-4.html`) ; ils ne sont pas ajoutés comme cinq pages complètes au dépôt. Pour rejouer, employer ces instantanés et les fichiers JSON de preuve ; une nouvelle récupération peut différer après mise à jour de la documentation.

| Point vérifié | Constat et référence primaire |
| --- | --- |
| Organisation mémoire et accès | Seul Vec est décrit comme contigu ; VecDeque est un tampon circulaire. Aucun masquage d’éléments supprimés ni optimum de performance n’est ajouté. [Vec](https://doc.rust-lang.org/std/vec/index.html) · [VecDeque](https://doc.rust-lang.org/std/collections/vec_deque/index.html) |
| Qualifications de fin de Vec | La citation garde séparément `push` amorti O(1) et `pop` O(1). [Module Vec](https://doc.rust-lang.org/std/vec/index.html) |
| Tableau et notation | Les deux lignes, colonnes, n = taille, i = indice, astérisque amorti et qualification des suppressions sont présents. Aucun n = capacité ni coût limité aux seules réallocations n’est affirmé. [Collections](https://doc.rust-lang.org/std/collections/index.html) |
| Première affirmation en prose | `as_slices()` retourne deux tranches ordonnées, dont l’une peut être vide ; la prose ne promet ni découpage fixe ni optimisation mesurée. [Méthode](https://doc.rust-lang.org/std/collections/struct.VecDeque.html#method.as_slices) |
| Seconde affirmation en prose | `range` crée un itérateur sur l’intervalle demandé. L’absence de copie des T est déduite des signatures `Iter` et `Item = &T`, pas d’un benchmark ni d’une promesse générale d’absence d’allocation. [range](https://doc.rust-lang.org/std/collections/struct.VecDeque.html#method.range) · [Iter](https://doc.rust-lang.org/std/collections/vec_deque/struct.Iter.html) |
| Longueur et limites | 368 mots au compteur du produit et au contrôle indépendant, inférieur à 400 ; limite de portée explicitement affichée. |

Les citations originales priment sur une traduction incertaine ; le rapport contient donc des passages anglais avec titre/prose/limites en français. Il reste volontairement **partiel** (quatre documents sur le seuil générique de dix-huit pages, découverte désactivée). Cette validation établit la fidélité du cas observé, pas une garantie générale sur tout sujet ou fournisseur.

## Restitution réelle et décision de livraison

Le rapport a été contrôlé dans l’interface, après passage par Notes, rechargement complet, sélection d’un ancien rapport puis retour à l’entrée de 368 mots. Les quatre sources, limites, phases et tableau de trois lignes sont conservés. [Historique après rechargement](validation/research-186-factual-2026-10-20/history-final.png) · [Tableau affiché](validation/research-186-factual-2026-10-20/table-final-wide.png).

**Validation locale factuelle et restitution : réussies sur ce cas.** Les anciens rapports refusés restent intacts. Le dernier critère UI disposera de sa preuve réelle de résultat sourcé après livraison des corrections. **#186 reste ouvert tant que cette branche n’est pas livrée et sa CI vérifiée.** Aucun push, fusion ou succès de CI n’est revendiqué par ce bilan local.


## Suivi de revue de la PR #234

La réserve fixe de 35 mots et la note bilingue ajoutée systématiquement ont été supprimées. Les avertissements de vérification restent dans les métadonnées de qualité, hors du budget du rapport. Un rapport court conserve ainsi son contenu et aucune langue supplémentaire ne lui est imposée. Test de régression sur trois formulations (français, espagnol et anglais avec San Francisco) ; 48 tests Rust Recherche réussis. Les captures et la recherche réelle ci-dessus décrivent le commit initial, antérieur à cet ajustement de publication.


### Second passage de revue CI

Les titres susceptibles de contenir une affirmation sont maintenant inclus dans les blocs contrôlés ; seule une liste fermée de titres génériques est exemptée. La détection de la notation Big-O conserve les frontières des mots, afin de ne pas confondre « Go (the language) » avec une garantie de complexité. Deux tests de régression supplémentaires : 50 tests Rust Recherche réussis. Cette évolution a été vérifiée par tests ciblés, sans nouvelle recherche réelle.


### Citations propres à chaque bloc

Un lien vers une page consultée ne suffit plus : si le bloc cite une URL que son audit ne retient pas comme preuve, sa prose est remplacée par les passages originaux sélectionnés. Test des liens Markdown, HTML, automatiques et par référence, ainsi que des ancres autorisées : 51 tests Rust Recherche réussis.


### Formulations de limites de longueur

Le parseur reconnaît aussi « 400 mots au maximum », « do not exceed », « limit the answer/report/response to », « ne pas dépasser » et « limiter la réponse/le rapport à ». Les bornes strictes « under 400 words » et « moins de 400 mots » imposent au plus 399 mots. Les tests excluent toujours les minimums et les nombres de sources ; 51 tests Rust Recherche réussis.


### Budget et blocs longs

Le plafond final est vérifié aussi pour les rapports dégradés sans page consultée : un dépassement résiduel après révision produit une erreur explicite. Les titres génériques utilisent uniquement le budget restant après sélection du contenu. Les longs paragraphes sont répartis entre phrases complètes pour les audits bornés. Les tableaux et les assertions indivisibles restent entiers ; si un tel bloc dépasse encore la capacité de contrôle, la recherche échoue explicitement plutôt que publier une assertion non contrôlée. 54 tests Rust Recherche réussis.


### Références Markdown et preuves normalisées

Les références Markdown sont résolues avec le parseur sur le document complet avant la séparation des blocs : chaque affirmation conserve ainsi sa destination pour le contrôle de ses preuves. L’expansion des tableaux conserve également les autres citations vérifiées dont les espaces avaient été normalisés sur plusieurs lignes sources. Deux régressions supplémentaires : 56 tests Rust Recherche réussis.


### Blocs Markdown et légendes après tableau

Le découpage utilise les limites des blocs du parseur Markdown et conserve les exemples de code, listes et tableaux entiers. Les citations sont ajoutées dans un paragraphe séparé pour préserver les clôtures de code. La collecte recherche aussi les qualifications dans les 14 lignes suivant le tableau. Trois régressions supplémentaires : 59 tests Rust Recherche réussis.


### Débits et limites par unité

Le parseur écarte les nombres de mots suivis de « per », « par » ou « / » : un débit de génération ou une limite par élément ne devient pas une contrainte sur la longueur totale du rapport. Les tests couvrent aussi la coexistence d’un débit et d’une véritable limite de réponse ; 59 tests Rust Recherche réussis.
