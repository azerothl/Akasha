# #186 — preuves originales et vérification factuelle

Complément au [premier bilan](research-186-validation-2026-10-19.md), après fusion de la [PR #225](https://github.com/azerothl/Akasha/pull/225). Les anciens rapports non validés sont conservés sans modification.

## Cause et corrections

- L'ancien nettoyage HTML supprimait la structure des tableaux, leurs en-têtes et leurs séparations. Le texte utile de `main`/`article` est désormais privilégié, les cellules et lignes restent distinctes, les retours de mise en forme HTML ne coupent plus les paragraphes avant une qualification, les entités HTML sont décodées et la navigation n'est pas une preuve.
- Les résumés et traductions du modèle pouvaient devenir les « preuves » de la rédaction. L'extracteur sélectionne maintenant des identifiants de lignes ; le programme copie le texte original. Les anciens formats ne sont acceptés que pour des passages retrouvés dans la page, après normalisation des espaces. Une sortie non structurée ne devient plus une preuve par défaut.
- Le manifeste conserve des passages complets, jamais une phrase coupée avant sa qualification. Son budget passe de 2000 à 2800 octets, sans dépasser le budget global de rédaction. Les sources omises sont signalées ; aucune rédaction web n'est publiée si aucun passage ne tient dans le contexte.
- Un brouillon généré n'est plus réinjecté dans la rédaction comme une preuve lorsque des pages ont été retenues. Au dernier tour configuré, les notes sont les passages des sources, sans appel de synthèse intermédiaire supplémentaire. Les consignes distinguent noms d'API, opérations abstraites et qualifications de coût ; aucune règle ne code en dur la réponse sur les collections Rust.
- Pendant les essais, une annulation pouvait être suivie d'une reprise d'analyse après la fin d'un appel modèle. Le travail en cours est désormais abandonné lors de l'annulation et aucun rapport final n'est publié pour ce run.

## Régressions déterministes

**36 tests Rust Recherche réussis**, avec compilation du daemon Windows :

- [Sortie des tests](validation/research-186-grounding-2026-10-19/tests-grounding-paragraphs.txt).
- [Compilation](validation/research-186-grounding-2026-10-19/build-paragraphs.txt).
- Cas couverts : lignes et légendes des tableaux, entités HTML, exclusion de navigation/code actif, identifiants hors plage ou dupliqués, ajout erroné d'un coût amorti, traduction présentée comme citation, passages trop longs, budgets Unicode et annulation d'un travail en attente.

**12 parcours UI Recherche réussis**, compilation TypeScript/Vite incluse : [sortie](validation/research-186-grounding-2026-10-19/tests-ui-research-final.txt). Les nouvelles régressions actionnent le bouton Annuler et vérifient la persistance après rechargement, ainsi que l'échec HTTP d'annulation sans faux statut terminal. Le suivi reste actif jusqu'à réception et sauvegarde de l'état terminal ; ses requêtes sont sérialisées. Un échec d'annulation a un message FR/EN visible pendant la recherche. Ces parcours utilisent des fixtures identifiées, distinctes de la recherche réelle.

Ces tests ne constituent pas une vérification sémantique générale des réponses d'un LLM.

## Protocole réel et périmètre

Le contrôle est lancé depuis le formulaire Recherche de l'interface existante, via son proxy HTTP vers le vrai daemon Windows ; aucune réponse n'est remplacée et aucune fixture n'est utilisée pour les recherches ci-dessous. Les données du daemon sont isolées dans `E:\tmp\akasha-186-grounding-20261019`. Les essais exploratoires utilisent l'origine navigateur `127.0.0.1:4189`, avec l'historique antérieur. Le contrôle final utilise l'origine `127.0.0.1:4190`, avec un historique neuf, sans effacer les essais précédents.

Sujet identique à l'essai 14B du premier bilan : comparaison de `Vec` et `VecDeque`, en français, 400 mots maximum, mémoire, accès par indice, opérations aux deux extrémités, coûts amortis et usages. Quatre URL officielles sont explicitement fournies : modules `vec`, `vec_deque`, `collections` et structure `VecDeque`.

Les pages ont été contrôlées indépendamment : HTTP 200, documentation affichée **Rust 1.99.1**, empreintes SHA-256 dans [source-checks.json](validation/research-186-grounding-2026-10-19/source-checks.json). La structure `Vec` est également consultée pour la relecture indépendante des noms de méthodes ; cela ne la fait pas compter comme une page lue par le run si elle n'y figure pas.

### Essais interrompus, non comptés comme rapports validés

- Un essai initial sur le code fusionné a perdu sa connexion lors de l'installation de l'observation du proxy. Il a été interrompu ; aucun résultat positif n'en est revendiqué.
- `dr_947ec770e0ab4106bcee0037d6f3f96c`, modèle local `qwen3:14b`, GPU, contexte 8192 : deux pages retenues, essai annulé pendant une lecture lente. [Trace](validation/research-186-grounding-2026-10-19/run-14b-cancelled.json).
- `dr_f7120e33262c46e9a2d9292f24866e88`, modèle local `qwen3.5:9b`, GPU : essai interrompu pour contention mémoire. La trace prise après l'annulation a exposé la reprise d'analyse corrigée ci-dessus. [Trace](validation/research-186-grounding-2026-10-19/run-9b-gpu-cancelled.json).
- `dr_e3be80b3f32b4e7a908b827be7e8592d`, même 9B sur CPU, découverte activée : cinq pages retenues, dont une ancienne documentation Rust 1.36.0. Cet essai exploratoire a été arrêté pour relancer un contrôle reproductible avec le dernier correctif et les quatre documents courants. [Trace](validation/research-186-grounding-2026-10-19/run-cpu-exploratory-incomplete.json).

Un autre service Ollama avait une requête active lors du contrôle de ses slots. L'accord de l'utilisateur permettait uniquement de décharger un modèle inactif : ce service a été laissé intact. CUDA est installé et fonctionnel ; l'exécution CPU du contrôle final est un choix d'isolation, pas un constat d'absence de CUDA.

### Découpe des paragraphes et annulation réellement contrôlée

Le run `dr_7d7f133ed4c64fa68b9a772fb343c05b` a exposé une coupure liée aux retours du fichier HTML : un extrait se terminait par « and » avant le coût de `pop`. Les lignes sont désormais des paragraphes/blocs logiques, et les sauts des blocs de code sont préservés. La nouvelle régression reproduit la phrase complète, les éléments inline et un bloc `pre`.

Ce run a ensuite servi à vérifier la garde d'annulation : état `cancelled`, date de fin présente et aucun rapport lors de trois lectures successives ; le slot du moteur est inactif. [Run](validation/research-186-grounding-2026-10-19/run-id-selection-cancelled.json) · [Contrôles](validation/research-186-grounding-2026-10-19/cancellation-check.json) · [Slot](validation/research-186-grounding-2026-10-19/cancelled-model-slots.json). Il n'est pas présenté comme un rapport factuellement validé.

### Contrôle final

Run `dr_8ec849da09fa4a96ba7429c1ad01dfa6`, code `891282d` (correctifs factuels `6872f1d` + `370c75f`), modèle local déjà installé `qwen3.5:9b`, réflexion désactivée, contexte 8192, `num_gpu: 0`, délai fournisseur 600 s et sortie plafonnée à 1536 tokens par la configuration de validation. Aucun modèle téléchargé.

La découverte est désactivée pour ce contrôle et seul `doc.rust-lang.org` est autorisé ; les quatre pages explicites sont réellement lues par `web_fetch`, avec la politique de domaines normale. Ce contrôle examine la fidélité du rapport issu de ces sources ; il ne mesure pas la disponibilité générale des moteurs de recherche. [Configuration du routeur](validation/research-186-grounding-2026-10-19/validation-router.json) · [Politique des outils](validation/research-186-grounding-2026-10-19/validation-tools-policy.yaml).

**Validation factuelle refusée.** Le run termine en 580 secondes avec quatre pages, 509 mots et `source_sufficient: false`. L’interface affiche correctement « Rapport partiel ». Le plafond demandé de 400 mots n’est pas respecté.

Preuves conservées sans réécriture : [run brut](validation/research-186-grounding-2026-10-19/run-complete-not-validated.json), [rapport brut](validation/research-186-grounding-2026-10-19/report-complete-not-validated.md), [capture](validation/research-186-grounding-2026-10-19/report-891282d-not-validated.png).

### Relecture contradictoire

| Affirmation du rapport | Contrôle sur les sources | Décision |
| --- | --- | --- |
| La contiguïté garantit une performance séquentielle optimale | La documentation décrit la contiguïté, sans garantir cet optimum | Supprimer la garantie |
| Le tampon masque les éléments supprimés et évite les allocations fréquentes | Mécanisme et fréquence non établis par les passages lus | Supprimer |
| `push` et `pop` ont ensemble un coût amorti O(1) | Le module `vec` distingue `push` amorti O(1) et `pop` O(1) | Conserver les qualifications séparées |
| n est la capacité | La convention du tableau désigne la taille de la collection | Corriger |
| Les coûts O(n)* s’appliquent uniquement au redimensionnement | L’insertion indexée comporte aussi le déplacement des éléments ; * indique une borne amortie | Corriger |
| Une performance constante aux deux extrémités | Les qualifications amorties ne doivent pas disparaître | Reformuler précisément |
| `as_slices()` optimise l’accès | L’API expose deux tranches ; aucun gain universel n’est établi | Supprimer le gain supposé |

Références : [module Vec](https://doc.rust-lang.org/std/vec/index.html), [module VecDeque](https://doc.rust-lang.org/std/collections/vec_deque/index.html), [tableaux et légendes](https://doc.rust-lang.org/std/collections/index.html), [API VecDeque](https://doc.rust-lang.org/std/collections/struct.VecDeque.html).

Un essai séparé de vérification locale de six affirmations rejette les mécanismes et garanties inventés, mais accepte encore la qualification commune de `push` et `pop`. [Entrée](validation/research-186-grounding-2026-10-19/claim-review-probe-input.json) · [Sortie brute](validation/research-186-grounding-2026-10-19/claim-review-probe-output.json). Ce vérificateur expérimental n’est pas intégré au produit et ne constitue pas une garantie sémantique.

Les corrections de collecte, de conservation des preuves et d’annulation sont vérifiées. La fidélité sémantique du rédacteur et le respect de la longueur restent non validés. **#186 reste ouvert.** Les commits sont locaux ; aucune livraison fusionnée ni réussite de CI n’est revendiquée.
