# Générations Studio : délai, reprise et suivi

Le chemin Studio utilise `LLMRouter::complete_stream_bounded`. Le budget total réserve du temps aux routes alternatives configurées, sans répéter le primaire avant un fallback. Trois tentatives au maximum sont autorisées ; un modèle imposé reste épinglé, avec une seule reprise de transport si les retries sont activés. Les fragments d'une génération abandonnée ne sont pas exécutables. Les outils des tours antérieurs restent couverts par le registre d'opérations et le checkpoint.

Les événements `llm_provider_attempt` et `llm_wait_heartbeat` indiquent la route réellement appelée, sa phase et son attente. Les erreurs finales donnent les modèles tentés et une marche à suivre. Un timeout ne déclenche pas `first_meaningful_progress`.

Une tâche Studio en échec peut reprendre depuis un checkpoint valide. Le moteur, l'API de reprise et le contrôle d'admission reconnaissent cet état. La réservation du projet empêche une reprise concurrente à une autre tâche active. Les autres restrictions de projet, rôle et opérations interrompues restent appliquées.

Le ticket est réconcilié à la finalisation et à la lecture du board. Une revue en échec devient bloquée mais conserve `evidence.resume_status = review`, afin que la reprise retourne au reviewer. Les anciennes demandes de création à ticket unique reçoivent un backlog sans remplacer le ticket ou son historique.

Le rapport `GET /api/tasks/:id/report` ne déduit pas une validation de `progress_pct = 100` : les résumés `done` demandent une fin réussie et des critères vérifiés, et sont dédupliqués.

Recette : dans le dépôt Code Studio, définir `CODE_STUDIO_DAEMON_BIN` vers le binaire reconstruit puis exécuter `npm run test:daemon:recovery`. Elle démarre un daemon isolé avec fournisseur HTTP scripté et conserve un rapport portant le SHA-256 du binaire. Elle couvre timeout, fallback, heartbeat, absence d'exécution des fragments abandonnés et reprise avec les mêmes identifiants. Une recette avec vrai LLM reste distincte.

## Admission des routes Studio

Le profil optionnel `task_types.studio` est prioritaire pour les tâches disque Studio, y compris la planification du contexte et les rôles délégués. En son absence, le profil de code existant reste utilisé. Seules les alternatives de ce profil peuvent servir de repli ; un modèle explicitement choisi reste épinglé. `POST /api/router/route` accepte aussi le champ `config` des routes (notamment `num_ctx`, `max_tokens`, `thinking_level`).

Avant la première génération avec outils, un probe inerte vérifie un appel natif `studio_probe` avec un argument exact. Aucun appel d'outil de ce probe n'est exécuté. L'estimation de contexte de la vraie demande est contrôlée séparément, sans supprimer ses instructions protégées. Le probe, son attente de verrou incluse, est limité à 20 s ; une réponse correcte est conservée une heure, un échec cinq minutes. La clé inclut route et contexte effectif ; le rechargement de configuration invalide le cache. Une qualification indique la validité de ce petit échange, pas l'aptitude à terminer un projet.

Les routes embarquées `akasha_core` / `akasha_embedded` n'exposent pas ce transport natif et ne servent plus de secours implicite pour les tâches avec outils. Les autres routes sont réellement interrogées : leur simple présence ou un résultat `is_available` n'est pas une preuve. Les diagnostics d'inférence embarquée n'attribuent plus leurs délais au réseau HTTP.

`POST /api/router/studio-qualify` avec `{provider,model}` permet une qualification explicite d'un modèle déjà configuré. Le catalogue `GET /api/router/studio-options` expose son résultat, contexte configuré, latence et date. Dans Code Studio, le bouton de vérification est désactivé pendant une tâche active. La recette opt-in `npm run test:daemon:models` attend `CODE_STUDIO_QUALIFY_MODELS`, un tableau JSON de routes à tester avec les services réels ; elle conserve un rapport sans toucher aux fichiers d'un projet.

Une génération sans premier contenu est bornée à 60 s maximum dans le budget existant. Les échecs d'exécution mettent aussi la route en quarantaine. Lorsque toutes les routes sont épuisées (`studio_routes_exhausted`), le ticket reste bloqué et le scheduler ne redispatche pas la demande inchangée après son délai. Une modification du profil ou une reprise explicite permet un nouvel essai. Choisir un autre modèle dans le composeur s'applique à une nouvelle demande : cela ne réécrit pas silencieusement le contrat d'une tâche existante.
