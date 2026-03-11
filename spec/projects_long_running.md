# Projets longs — contexte, isolation et achèvement

Ce document décrit comment faire en sorte que l’agent conserve le contexte, isole les fichiers et ne signale pas « terminé » prématurément pour des projets longs (roman, BD, projet de code, etc.).

---

## 1. Contexte projet conservé

Pour qu’à chaque échange l’agent retrouve le projet en cours (objectif, état, livrables déjà faits) :

- **Convention mémoire** : les entrées liées à un projet sont stockées en mémoire long terme avec une source du type `project:<nom>` (ex. `project:mon_roman`). L’agent peut utiliser `memory_store` pour enregistrer l’état du projet après chaque avancement significatif (objectif, étapes faites, livrables).
- **Rechargement** : lorsqu’une requête évoque un projet (création, suite, « continue »), le système peut injecter en tête du prompt un bloc « [Projet en cours] » construit à partir de `memory_search` sur des termes liés au projet (état, livrables).
- **Session** : en gardant le même `session_id` entre les messages, la mémoire court terme conserve les derniers échanges ; l’utilisateur peut envoyer « continue », « et la suite ? », « chapitre 2 » pour enchaîner.

Voir aussi [06_memory_model.md](06_memory_model.md) (court terme, long terme, compaction).

---

## 2. Isolation : ne pas impacter le reste du système

Pour que les fichiers du projet ne posent pas de problème au reste du système :

- **Répertoire dédié** : un projet = un répertoire dédié (ex. `~/akasha_projects/mon_roman`, `~/projets/code/ma_app`). Ne pas autoriser `"."` ou un répertoire parent large si l’on veut isoler : ajouter **uniquement** ce répertoire dans `allowed_read_paths` et `allowed_write_paths` de `tools_policy.yaml`.
- **Configuration** : voir [35_configuration_reference.md](35_configuration_reference.md) (section tools_policy.yaml) et [tools_policy.example.yaml](tools_policy.example.yaml) pour les bonnes pratiques et exemples.
- **Optionnel (évolution)** : overlay de politique par projet (restriction des chemins au `root_dir` du projet pendant le travail sur ce projet) ou entité « projet » explicite avec `root_dir`. Le sandbox conteneur pour tout le workspace projet est réservé à une phase ultérieure si besoin d’isolation forte.

---

## 3. Ne pas dire « terminé » trop tôt

- **Règles de prompt** : l’agent reçoit une règle explicite : pour les demandes à livrable substantiel (roman, BD, projet de code, série de chapitres/fichiers), ne jamais prétendre que c’est terminé après une seule réponse si le périmètre n’est pas entièrement livré ; indiquer ce qui a été fait, ce qui reste à faire, et que l’agent poursuivra au prochain message (ou via une sous-tâche). Ne pas dire « C’est terminé » ou « Voilà, c’est fait » tant que tous les livrables demandés ne sont pas réalisés.
- **« Continue » / « la suite »** : si l’utilisateur dit « continue » ou « la suite », l’agent reprend le projet en cours (contexte rechargé via mémoire ou bloc projet) et poursuit sans annoncer « terminé » tant que le périmètre n’est pas entièrement livré.
- **Allers-retours** : la même session + mémoire projet permettent des allers-retours par messages successifs. L’outil `sessions_spawn` peut être utilisé pour déléguer des sous-parties (ex. « génère le chapitre 2 ») ; le résultat est visible dans la liste des tâches. Une boucle orchestrateur multi-phases (attendre enfants, relancer sous-tâches jusqu’à critère projet) reste une évolution optionnelle.

---

## 4. Résumé des bonnes pratiques

| Objectif | Action |
|----------|--------|
| Conserver le contexte | Utiliser `memory_store` avec `source = "project:<nom>"` après chaque avancement ; garder la même session pour enchaîner. |
| Isoler les fichiers | Créer un répertoire dédié par projet et le lister seul dans `allowed_read_paths` / `allowed_write_paths`. |
| Éviter « terminé » trop tôt | Règle prompt + réponses qui indiquent clairement « fait / reste à faire / je continue au prochain message ». |
| Reprendre un projet | Utiliser « continue », « la suite », ou le nom du projet ; le bloc projet est rechargé depuis la mémoire long terme. |
