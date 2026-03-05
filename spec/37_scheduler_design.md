# Design du Scheduler — Récurrences et runs

Ce document décrit le design du service de planification des tâches récurrentes : mode standalone et mode cluster. Il s’appuie sur le [modèle de données](10_data_model.yaml) (schedule, schedule_exception, task_run), sur [NFR-009](04_non_fonctional_requirements.md) (robustesse scheduler, persistance après crash/restart) et [NFR-010](04_non_fonctional_requirements.md) (at-least-once + dédup, run_id).

---

## 1. Objectifs

- Planifier l’exécution de tâches récurrentes (règle type iCal RRULE ou cron).
- Garantir qu’une récurrence n’est pas perdue après crash/restart (persistance).
- Éviter les doubles exécutions (déduplication via `dedup_key` / `run_id`).
- En cluster : un seul nœud déclenche les runs ; les workers exécutent.

---

## 2. Mode standalone

- **Service scheduler** dans le Core (daemon) : processus dédié ou tâche périodique.
- **Tick** : toutes les X secondes (ex. 30 s), le scheduler évalue les `schedule` actifs et crée les `task_run` dont `planned_for` est due.
- **Persistance** : les schedules et task_run sont stockés en base (SQLite ou équivalent) ; relecture au redémarrage.
- **Déduplication** : chaque run prévu est identifié par un `dedup_key` (ex. `schedule_id + planned_for`) ; avant de lancer une exécution, vérification qu’aucun run avec ce `dedup_key` n’a déjà été traité (évite double-run après restart).

Référence : NFR-009, NFR-010.

---

## 3. Mode cluster

- **Leader unique** : un seul nœud (leader) est responsable du tick du scheduler et de la création des `task_run` à déclencher. Les autres nœuds ne font pas de tick.
- **Workers** : les runs créés (task_run en statut `queued`) sont pris en charge par les nœuds exécutants (workers) ; le leader peut être aussi worker.
- **Réplication** : les créations/mises à jour de task_run et les événements associés (task_run_created, task_run_scheduled, etc.) passent par le bus (event log) pour cohérence et traçabilité.
- **Élection de leader** : mécanisme d’élection (NATS KV, JetStream ou Raft selon la stack) pour désigner le nœud qui tient le scheduler ; reprise automatique en cas de perte du leader.

Voir [20_cluster_architecture.md](20_cluster_architecture.md) (tolérance de panne, élection leader) et [29_stack_technique.md](29_stack_technique.md) (NATS, persistence).

---

## 4. Flux résumé

1. **Tick** (périodique) : le scheduler (leader en cluster) lit les `schedule` actifs, applique les `schedule_exception` (skip/override), et pour chaque occurrence due crée un `task_run` avec `dedup_key`.
2. **Dédup** : si un task_run avec le même `dedup_key` existe déjà (créé avant crash), on ne recrée pas ; au plus on repasse en `queued` si besoin.
3. **Exécution** : un worker prend un task_run `queued`, crée ou associe une `task`, émet les événements (task_started, task_progress_updated, …), et met à jour le task_run (started_at, ended_at, status).
4. **UI** : l’onglet Calendrier affiche les schedules et les task_run à venir ; l’onglet Tâches affiche les tâches (dont celles issues des runs) en temps réel.

---

## 5. Événements associés

Voir [09_event_model.yaml](09_event_model.yaml) : `scheduler_tick`, `schedule_created`, `schedule_updated`, `schedule_deleted`, `task_run_created`, `task_run_scheduled`, `task_run_skipped`.
