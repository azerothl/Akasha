# Réduction de la charge CI et des builds de release

Suivi : #147 (cache), #148 (sélection CI), #149 (clients Windows),
#150 (compatibilité CPU/CUDA). État documenté le 18 octobre 2026.

## Changements fusionnés dans la PR #165

La CI calcule les fichiers modifiés avec Git et le graphe inverse des
dépendances locales Cargo. Le graphe inclut les dépendances optionnelles,
de développement, de build et propres à une cible. Un changement d'une crate
sélectionne aussi les crates qui en dépendent. Un impact sur le core ou le
daemon déclenche les vérifications complètes, dont l'UI. Les manifestes,
lockfiles, scripts, workflows, entrées runtime et chemins inconnus déclenchent
également le parcours complet. Une base Git absente ou inexploitable ne permet
jamais d'ignorer les vérifications.

Les changements de README et de documentation de conception au format Markdown
évitent les jobs lourds. Les pages de docs/user et user_guide_final.md restent
couvertes car elles sont servies par le produit. Les déplacements sont traités
comme suppression + ajout afin de contrôler les deux chemins. Les valeurs
envoyées aux outputs GitHub sont sérialisées en JSON, y compris les noms de
fichiers contenant des retours à la ligne.

Le check requis existant test reste présent sur chaque PR, y compris les PR
documentaires. Il échoue si la sélection ou un job requis échoue, est annulé
ou est ignoré à tort. Le job llama.cpp conserve son caractère consultatif.
Les tests UI reçoivent le daemon déjà construit par le job Rust ; un changement
UI seul construit son propre daemon. Le déclenchement manuel de CI force
tous les contrôles.

## Cache et reproductibilité

Les deux applications ont désormais leurs Cargo.lock suivis :
racine du workspace et apps/akasha-ui/src-tauri. Avant ce changement, la règle
Cargo.lock de .gitignore les excluait, et les caches étaient restaurés avant
leur génération. Les clés basées uniquement sur ces fichiers ne pouvaient
donc pas identifier les dépendances d'un checkout neuf.

Les lockfiles locaux existants ont servi de point de départ. La résolution
verrouillée a été vérifiée hors ligne pour le workspace et Tauri ; seul le
numéro du package local Tauri a dû passer de 0.10.0 à 0.11.0. Ce constat
n'établit pas que les dépendances d'une ancienne release étaient identiques.
La synchronisation de version met désormais aussi à jour les packages locaux
des deux lockfiles, sans faire évoluer les dépendances externes.

L'action locale cargo-cache utilise Swatinem/rust-cache v2. Elle conserve
les dépendances et le cache CMake llcb de CUDA, avec une partition explicite
par image OS, architecture, cible, profil et variante native. L'action amont
ajoute l'identité du compilateur, les manifestes/locks et les variables de
compilation à ses clés. Le digest des sources vendor et du script de build
CUDA complète l'identité native.

Les crates du workspace ne sont pas conservées dans les snapshots. Cela évite
de considérer leurs exécutables comme des couches distribuables indépendantes.
Le cache se concentre sur les dépendances ; la compilation et l'édition des
liens du code produit restent à effectuer. Les objets inutiles et incrémentaux
sont nettoyés par l'action amont avant sauvegarde.

Les clés ne changent pas pour chaque commit source. Les PR du même dépôt peuvent
sauvegarder un cache afin de mesurer un second run ; celles des forks restaurent
seulement les caches accessibles. Le nettoyage CUDA inconditionnel en CI est
supprimé. L'option explicite -CleanLlamaCache reste disponible pour réparer
un cache ; le préfixe akasha-deps-v2 empêche de restaurer les anciennes entrées
/MT avec la nouvelle configuration /MD.

Important : les règles de portée GitHub restent applicables. Un cache créé
sur un tag n'est pas accessible depuis un autre tag. Un tag peut restaurer
un cache compatible de la branche par défaut. Le mode manuel Release avec
publish=false permet des répétitions sans publication et peut amorcer des
caches depuis cette branche. Son coût doit être inclus dans la comparaison :
lancer un build uniquement pour amorcer le cache n'est pas en soi une économie.

Au relevé initial, le dépôt occupait 5 056 324 727 octets dans 6 caches.
Les nouvelles entrées restent soumises au quota et à l'éviction GitHub.
Vérifier le volume après les premiers runs avant d'ajouter un deuxième niveau
de cache de compilateur. sccache a été considéré mais n'est pas activé :
il ajouterait un autre stockage avant de disposer des mesures du premier.

### Mesure après fusion de la PR #165 — 20 octobre 2026

Avant nettoyage, `gh cache list` recensait 18 entrées pour 11 383 834 209 octets.
L'API d'usage indiquait simultanément 10 253 547 114 octets et 18 entrées ;
elle n'avait pas encore reflété la dernière entrée de 1 130 287 095 octets.
Le dépôt a un plafond configuré de 10 Go. Les entrées des PR déjà fusionnées
#138, #165 et #200 ont été supprimées (10 caches), sans toucher à `main` ni aux
PR ouvertes. L'inventaire après suppression compte 9 entrées, 4 769 518 092
octets : 7 caches de `main`, un de #221 et un de #223. L'API d'usage renvoyait
encore l'ancien relevé juste après le nettoyage, puis a convergé à 4 769 518 092
octets et 9 entrées au relevé suivant.

Cette réduction remet l'inventaire visible sous le plafond, mais le ticket #147
reste ouvert : il manque encore deux builds de release comparables, des mesures
de hits/misses et de durée après changement source, puis la validation des
archives produites.

Sources :
- https://github.com/Swatinem/rust-cache
- https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching

## Une compilation des clients Windows

Le job windows-clients compile akasha-cli et akasha-tui une seule fois,
avec les features par défaut, sur windows-2022/VS 2022. Il publie un artefact
interne commun, un manifeste SHA-256 et le graphe Cargo avec ses features.
Les deux jobs daemon Windows récupèrent ces fichiers et vérifient leurs
empreintes après staging. Les artefacts intermédiaires et rapports ne sont
pas sélectionnés pour publication dans les Releases.

Les builds Linux/macOS et Tauri continuent en parallèle avec ce producteur.
Les jobs Windows attendent les clients partagés avant leur build/packaging.
Les transferts, éventuelles recompilations de dépendances entre ces jobs
et temps d'attente doivent donc être comptés dans le bilan mesuré.

Les anciens clients v0.11.0 ont des empreintes différentes entre les archives
CPU et CUDA, qui étaient compilées sur deux images Windows différentes.
Leurs imports DLL ne nécessitent pas CUDA. La nouvelle procédure garantit
l'identité entre les deux archives produites dans un même run, sans prétendre
que les anciens binaires étaient identiques octet pour octet.

## Décision CPU/CUDA : NO-GO pour l'artefact actuel

L'analyse du daemon publié dans l'archive CUDA v0.11.0 confirme un import PE
direct de nvcuda.dll. Le chargeur Windows demande cette DLL du pilote NVIDIA
avant que Rust puisse choisir Candle ou examiner un modèle GGUF. Le fallback
dans le code ne permet donc pas à cet artefact de devenir la distribution
universelle pour les machines sans ce pilote.

Le rapport windows_cpu_cuda_v0.11.0.json provient du vrai fichier publié,
identifié par son SHA-256. La machine locale ayant un pilote NVIDIA,
aucun démarrage sans pilote ni aucune inférence CPU/GPU ne sont revendiqués
par ce rapport. Les cas GGUF présent et absent restent non validés ; pour
cet import direct, l'obstacle intervient avant leur sélection.

Le workflow CPU-CUDA compatibility audit télécharge une release existante
sur un runner Windows standard et enregistre le démarrage sans pilote lorsqu'il
peut être essayé. Le même diagnostic est ajouté au staging des futurs builds
CUDA. Un run vert du diagnostic signifie que le rapport a été produit :
son verdict NO_GO ou INCONCLUSIVE n'est jamais une preuve d'inférence réussie.
La preuve GPU réelle reste suivie dans #142.

Un éventuel futur artefact commun devra charger CUDA facultativement et passer
les vrais parcours CPU sans pilote (avec/sans GGUF) et GPU, avant changement
du format distribué.

## Mesures observées et validations restantes

Baseline publique : release v0.11.0, run du 28 septembre 2026 :
https://github.com/azerothl/Akasha/actions/runs/36480940401

| Mesure | CPU Windows | CUDA Windows |
| --- | ---: | ---: |
| Archive | 32 288 871 octets | 610 456 251 octets |
| Contenu décompressé de l'archive | 94 267 578 octets | 908 260 026 octets |
| Job complet observé | 25 min 59 s | 46 min 32 s |

Les jobs étaient parallèles. Leur durée inclut installation, compilation,
staging et transferts ; ce ne sont ni des temps de compilation purs ni un coût
en euros. Aucun gain après modification n'a encore été mesuré.

Les fichiers JSON voisins conservent les tailles, empreintes, imports et temps
observés. Les futurs jobs exportent ci-build-report.json et les rapports
Cargo --timings. Le volume target mesuré précède le nettoyage du cache :
ce n'est pas la taille de l'entrée sauvegardée.

| Ticket | Preuve locale | Validation GitHub ou matérielle encore requise |
| --- | --- | --- |
| #147 | Lockfiles résolus avec --locked ; nettoyage ciblé des 10 caches attachés aux PR fusionnées ; inventaire visible redescendu à 4 769 518 092 octets | Deux runs de release comparables, hits/misses, durées après changement source, convergence du relevé d'usage et validation des archives |
| #148 | Sélection sur les vrais manifestes et tests de dépendances, déplacements, chemins mixtes et fallback | Runs docs seuls, crate isolée, UI seule et changement transverse ; statut requis |
| #149 | Imports des anciens clients inspectés ; construction commune et vérification SHA câblées | Deux archives nouvelles, smoke et mesure des transferts/runner |
| #150 | Import direct nvcuda.dll confirmé dans le daemon publié ; décision NO-GO documentée | Diagnostic natif sur runner sans pilote ; aucune inférence supplémentaire revendiquée |

Les tickets restent ouverts tant que leurs preuves d'acceptation requises
ne sont pas disponibles.

## Vérification et exploitation

Vérifications locales des changements de pipeline :

    python -m unittest discover -s scripts/ci/tests -v
    cargo tree --locked --offline --target x86_64-pc-windows-msvc -p akasha-cli -p akasha-tui
    cargo tree --locked --offline --manifest-path apps/akasha-ui/src-tauri/Cargo.toml --target x86_64-pc-windows-msvc --depth 0
    actionlint -shellcheck= -pyflakes= .github/workflows/ci.yml .github/workflows/release.yml .github/workflows/build-release.yml .github/workflows/cpu-cuda-compatibility.yml .github/workflows/sync-release-version.yml

Le jeu de tests couvre aussi la synchronisation des versions des lockfiles,
en vérifiant que les dépendances externes ne changent pas.

Pour mesurer après publication de la branche, relancer deux fois le même CI
ou le mode Release publish=false sur le même ref. Conserver les identités de
run, rapports et timestamps GitHub. Comparer ensuite une modification source
restreinte avec les mêmes locks, compilateur et image de runner. Pour le cache
entre tags, partir d'un cache compatible réellement accessible depuis la
branche par défaut et signaler toute absence de ce cache.

Contrôle du volume et nettoyage manuel ciblé des anciennes entrées :

    gh api repos/azerothl/Akasha/actions/cache/usage
    gh cache list --repo azerothl/Akasha --limit 100

Examiner les clés et portées avant suppression par identifiant. Ne pas effacer
tous les caches pour mesurer un run chaud.

## Mesures comparatives des releases — 20 octobre 2026

Ces mesures actualisent le relevé précédent. Les runs ont été déclenchés
successivement sur main avec publish=false :

- Run 39420405894, commit 33fcf0e : https://github.com/azerothl/Akasha/actions/runs/39420405894
- Run 39428283604, commit 58b6273 : https://github.com/azerothl/Akasha/actions/runs/39428283604

Les rapports JSON CI et les rapports Cargo --timings ont été téléchargés depuis
les artefacts de ces runs. Le tableau reprend la durée complète rapportée par
chaque job, le résultat de restauration du cache Cargo et la taille du dossier
target avant nettoyage.

| Cible | Run froid : cache / durée / target | Run chaud : cache / durée / target |
| --- | --- | --- |
| Producteur CLI/TUI Windows | miss / 330,884 s / 1 397 041 454 o | hit / 145,582 s / 1 396 388 801 o |
| Windows CPU | miss / 1 345,919 s / 3 027 819 327 o | hit / 361,376 s / 3 027 356 629 o |
| Windows CUDA | miss / 2 801,693 s / 3 548 517 065 o | hit / 1 011,442 s / 3 547 782 970 o |
| Linux x86_64 | miss / 903,165 s / 3 354 321 993 o | hit / 180,143 s / 3 353 556 367 o |
| macOS x86_64 | miss / 1 198,807 s / 2 798 805 912 o | hit / 405,672 s / 2 798 407 408 o |
| macOS ARM64 | miss / 1 287,696 s / 2 741 056 507 o | hit / 429,561 s / 2 740 346 102 o |

Pour ces six jobs, la somme des durées mesurées passe de 7 868,164 s
(131 min 08 s) à 2 533,776 s (42 min 14 s), soit 5 334,388 s de jobs runner
en moins sur le second run. Ce sont des sommes de temps runner, pas une
réduction identique de la durée murale d'une release, car les cibles s'exécutent
en parallèle. Les commits et quelques images runner diffèrent : cette
comparaison décrit les runs observés et ne constitue pas un essai causal
contrôlé.

Entre les deux commits, le second build a reçu des changements Rust dans
akasha-daemon et akasha-tools (PR #238), en plus de l'alignement Tauri suivi
par #240. Le cache CI conserve les dépendances compatibles et retire les
artefacts des crates du workspace ; un hit n'importe donc pas les exécutables
construits d'un ancien commit. Le code courant a été recompilé depuis le
checkout, tandis que les dépendances compatibles ont été restaurées. Les
builds des six cibles ont réussi au second run. Le premier run a échoué
uniquement dans ses trois builds Tauri sur l'écart entre tauri 2.11.5 et
@tauri-apps/api 2.10.1 ; la correction a été intégrée dans #240 et les trois
builds Tauri du second run ont réussi.

### Gain de compilation CLI/TUI Windows

Dans le run de release v0.11.0 (36480940401), l'étape de compilation CLI/TUI
a duré 8 min 19 s sur le job CPU et 6 min 52 s sur le job CUDA, soit 15 min
11 s de temps runner additionné. Le producteur partagé a duré 330,884 s sur
son premier run froid et 145,582 s sur le run chaud. Les deux jobs de packaging
récupèrent ensuite le même artefact d'environ 5,55 Mo ; les téléchargements
observés ont ajouté environ 4 s au total. L'économie indicative en temps runner
est donc d'environ 9 min 36 s au premier run et 12 min 42 s au run chaud.
Les runs et images Windows sont différents ; cette estimation compare les
durées des étapes enregistrées, sans chiffrer un coût monétaire ni un gain de
durée murale.

### Relevé de base après la PR #241

Après la fin de la CI de #241 et sa fusion, ses deux caches de PR, inutiles
après les jobs, ont été supprimés par identifiant. Le relevé convergé est de
18 entrées pour 9 017 259 036 octets, sous le quota configuré de 10 Go :
16 caches main totalisent 8 898 390 122 octets ; les deux caches conservés
pour les PR ouvertes #221 et #223 totalisent environ 59,4 Mo chacun. L'API
d'usage, initialement en retard sur la suppression, a ensuite convergé vers
le même total.

### Nettoyage après le run 39431939285

Le run de validation a ajouté les partitions de cache associées aux nouveaux
checksums du workflow ; l'usage API a momentanément atteint 10 887 600 625
octets. Une fois les jobs terminés, les quatre anciennes entrées main ont été
supprimées par identifiant (`9218859147`, `9218679955`, `9216930364`,
`9218548631`). L'inventaire courant compte 15 entrées main pour un total de
8 841 693 347 octets. L'API d'usage a d'abord rapporté 9 951 111 942 octets,
puis a convergé vers la somme de l'inventaire, 8 841 693 347 octets. Aucune
entrée de cache n'est actuellement listée pour les refs de PR #221, #223 ou
#242.

### Validation de l'assemblage des archives

La PR #241 ajoute l'assemblage des archives dans un job dédié, même lorsque
publish=false, puis vérifie les empreintes et l'identité des exécutables
Windows CPU/CUDA dans les archives autonomes et complètes. Le run 39431939285,
déclenché sur le commit fusionné 705d698, s'est terminé avec succès. Les 9 jobs
de build mesurés ont tous réussi. Leurs cache hits sont faux : le changement du
workflow de build a créé de nouvelles clés à checksum distinct, donc ce run a
réchauffé ces partitions après invalidation ; cela ne contredit pas les hits
observés entre les runs 39420405894 et 39428283604.

| Cible | Cache | Durée job | Taille target |
| --- | --- | ---: | ---: |
| Producteur CLI/TUI Windows | miss | 513,955 s | 1 397 207 242 o |
| Windows CPU | miss | 1 326,375 s | 3 028 800 455 o |
| Windows CUDA | miss | 2 777,461 s | 3 549 340 573 o |
| Linux x86_64 | miss | 833,545 s | 3 354 811 029 o |
| macOS x86_64 | miss | 1 230,386 s | 2 799 619 599 o |
| macOS ARM64 | miss | 1 280,873 s | 2 741 553 313 o |
| Tauri Linux | miss | 377,525 s | 2 112 897 631 o |
| Tauri macOS | miss | 279,774 s | 1 168 579 815 o |
| Tauri Windows | miss | 445,609 s | 1 792 980 279 o |

La somme des 9 durées est 9 065,503 runner-seconde (151 min 06 s), sans inclure
le job de packaging. Le job de packaging a duré 240 s, a assemblé et téléversé
13 archives, puis a réussi le contrôle des archives.

Le producteur commun Windows a construit une fois akasha.exe et akasha-tui.exe.
Les jobs CPU et CUDA ont sauté leur compilation CLI/TUI, téléchargé l'artefact
partagé et vérifié son manifeste. Le job de packaging confirme les deux mêmes
SHA-256 dans les quatre archives Windows (CPU/CUDA, autonomes/complètes).
Chacun des deux jobs Windows a téléchargé l'artefact partagé en environ une
seconde. Le producteur partagé a pris 513,955 s sur ce run froid ; comparé aux
15 min 11 s de compilation doublée de la release v0.11.0, le gain mesuré est
d'environ 6 min 37 s de temps runner avant les transferts. Cette mesure ne
prédit pas la durée murale, car les jobs sont parallèles.

Tailles des archives Windows livrables : CPU autonome 34 036 657 o, CUDA
autonome 612 143 000 o, CPU complète 45 413 685 o, CUDA complète 623 520 023 o.
L'artefact Actions release-archives totalise 2 016 258 590 o. Les archives ne
sont pas publiées par ce run.

Le diagnostic de démarrage Windows sans pilote NVIDIA a également été exécuté
sur l'artefact CUDA de ce run. Le rapport dédié montre l'import direct de
nvcuda.dll et l'échec 0xC0000135 avant le choix backend/GGUF ; la décision
documentée reste NO-GO pour un artefact Windows CPU+CUDA universel. Voir
[`release_first_boot_candidate_run394319_windows_cuda.json`](release_first_boot_candidate_run394319_windows_cuda.json).
