# Comparaison du parcours court et du parcours agentique

Campagne du 27 novembre 2026 pour le ticket [#299](https://github.com/azerothl/Akasha/issues/299).

## Conditions

Les deux daemons ont été mesurés sur le même poste Windows 11 équipé d’un AMD
Ryzen 7 9800X3D et d’une NVIDIA GeForce RTX 4080 SUPER. Ollama 0.40.16 utilise
`qwen3.5:2b` avec un contexte de 8192 jetons, une sortie limitée à 256 jetons,
une température de 0 et le raisonnement désactivé. Le modèle était chargé sur le
GPU avant les mesures. Les poids Ollama correspondent à
`sha256:b709d81508a078a686961de6ca07a953b895d9b286c46e17f00fb267f4f2d297`.

| Variante | Sources | SHA-256 de l’exécutable |
| --- | --- | --- |
| Avant | `fc08c8f98cb99f1406c12318217541aa47a9e22a` | `0eb0fc49e0112651ad93779f039c28d1131cc8450eb9a4c7f5c47cee73e1debe` |
| Après | Sources Rust alignées sur `334f8decf2af8549588e2f6f9a00601032c7fbb9` (PR #300) | `62f88a9099635f525e8b9356d3e020ae155dcb6ef580def6673abbf5a62dde94` |

Les deux exécutables de développement sont en version `0.13.0-beta.3`, profil
`dev` sans fonctionnalités Cargo par défaut. Le benchmark alterne l’ordre des
versions et crée un profil jetable pour chaque passage du corpus. Chaque demande
ouvre une session distincte via `POST /api/message`, sans mode de composition
forcé. Le modèle est conservé en mémoire entre les passages. Le délai maximum
pour chaque essai est de 30 secondes.

La latence inclut la soumission et le suivi jusqu’au premier état terminal
observé. L’intervalle de sondage est de 100 ms. Le p50 et le p95 utilisent
le rang supérieur sur les réponses correctes de chaque scénario. Les appels
comptent les requêtes HTTP de génération, y compris celles de sélection,
de planification et de reprise.

## Reproduction

Compiler les deux versions avec `cargo build --locked -p akasha-daemon --no-default-features`,
puis copier chaque exécutable à un emplacement distinct. Avec une instance
Ollama locale configurée avec `OLLAMA_KEEP_ALIVE=-1`, lancer depuis la racine du dépôt.

```bash
python spec/dev/quality/short_answer_benchmark.py \
  --baseline /chemin/avant/akasha-daemon --candidate /chemin/apres/akasha-daemon \
  --ollama http://127.0.0.1:11475 --model qwen3.5:2b --device cuda \
  --context-size 8192 --timeout 30 --repetitions 30 --out short_answer_20261127.json
```

Adapter `--ollama` au port de l'instance locale. Sur Windows, un répertoire de
travail court peut être indiqué avec `--work-dir`. Le [rapport JSON](short_answer_20261127.json)
contient la commande exécutée avec le chemin de l'interpréteur ramené à `python`,
les réglages, les empreintes et les 300 essais individuels.

## Résultats

| Scénario | Réussites avant → après | p50 avant → après (ms) | p95 avant → après (ms) | Appels au modèle avant → après |
| --- | ---: | ---: | ---: | ---: |
| Définition du triangle en une phrase | 30/30 → 30/30 | 2 094 → 1 141 | 2 484 → 1 281 | 60 → 30 |
| Remise de 25 % sur 80 € puis 5 € de frais | 0/30 → 30/30 | — → 157 | — → 188 | 90 → 0 |
| Taux de remise manquant | 0/30 → 30/30 | — → 172 | — → 187 | 90 → 0 |
| Lecture d’un fichier fictif | 0/30 → 0/30 | — | — | 90 → 90 |
| Lecture, copie et relecture | 0/30 → 0/30 | — | — | 120 → 120 |
| **Total** | **30/150 → 90/150** | **2 094 → 187** | **2 484 → 1 250** | **450 → 240** |

Le tiret indique l'absence de réponse correcte. Les percentiles globaux
agrègent des scénarios différents selon la version. Pour la définition du
triangle, où les deux versions réussissent les 30 essais, la médiane baisse de
46 % et le nombre d'appels est divisé par deux.

Le candidat obtient 65 € en une phrase dans les 30 essais du scénario de
référence. Lorsque le taux manque, il demande ce taux à chaque essai. Ces deux
scénarios sont traités localement, sans requête de génération.

Sur chaque version, aucun essai ne dépasse le délai. Les 30 demandes de
lecture se terminent avec une réponse incorrecte et les 30 demandes multiétapes
se terminent en échec. Les traces du candidat ne montrent ni routage court ni
appel d'outil pour les demandes portant sur les fichiers. Aucune copie valide
n'a été produite. La campagne montre que ces demandes continuent d'emprunter le
parcours agentique, dont l'exécution reste à qualifier avec un modèle capable
d'appeler les outils dans cette configuration. Le taux global de 60 % décrit
uniquement ces cinq scénarios sur `qwen3.5:2b`.

## Validation complémentaire des outils

Les mêmes binaires ont été rejoués avec `qwen3.5:9b` et `gpt-oss:20b`, dont
Ollama déclare la prise en charge des outils. Chaque comparaison utilise le
même modèle sur les deux variantes, chargé sur la RTX 4080 SUPER. Le contexte
reste à 8 192 jetons et la température à zéro. La limite de sortie est de
512 jetons et le délai par essai de 45 secondes. Le
[fichier de résultats](short_answer_tools_20261127.json) contient les commandes,
les empreintes et le détail des opérations observées, y compris après expiration
du délai. Ces petits lots visent à vérifier l’exécution des outils. Ils ne
permettent pas de comparer des percentiles ni d’établir une régression.

| Modèle | Lecture réussie avant → après | Copie et relecture terminées avant → après | Détail de la tâche multiétape |
| --- | ---: | ---: | --- |
| Qwen3.5 9B | 1/1 → 1/1 | 0/1 → 0/1 | Deux délais dépassés. Le candidat lit la source et crée une copie exacte, puis relit la source. |
| GPT-OSS 20B | 2/2 → 1/2 | 0/2 → 0/2 | Avant, un délai dépassé et un échec. Après, deux délais dépassés. Aucune copie produite. |

La lecture est vérifiée par un appel `read_file` réussi sur le fichier de test
et une réponse qui correspond exactement à son contenu. Aucun essai sur les
fichiers n'a emprunté le parcours court. La lecture fonctionne sur les deux
versions avec les deux modèles, avec un délai dépassé sur le second essai
GPT-OSS du candidat.

Un premier lot Qwen 9B a produit six résultats sur huit. L’observateur SQLite
de diagnostic a verrouillé le profil temporaire au moment de son nettoyage
sous Windows. Ce lot est conservé à titre exploratoire. Dans le deuxième essai
multiétape du candidat, les événements capturés montrent la lecture de la
source, l’écriture de la copie puis sa relecture avec le contenu attendu. La
tâche continuait néanmoins de réécrire et relire la copie et a atteint le délai
de 45 secondes sans état terminal. Le benchmark récupère désormais les événements
via l’API même après expiration du délai.
