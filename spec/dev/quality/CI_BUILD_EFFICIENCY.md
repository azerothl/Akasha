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
