//! Periodically dispatch ready Code Studio Kanban work when a project is idle.
use crate::agents::{MainAgent, TaskPriority};
use crate::api_studio::{
    self, studio_list_ticket_events, studio_list_tickets, studio_ticket_prerequisite_done,
    StudioTicket,
};
use crate::gateway::{handle_envelope, MessageEnvelope};
use akasha_store::{TaskStatus, TaskStore};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

const DEFAULT_INTERVAL_SECS: u64 = 60;
const DEFAULT_RETRY_COOLDOWN_SECS: i64 = 300;
const MAX_AUTOMATIC_FAILURE_ATTEMPTS: usize = 3;

type ProjectDispatchLocks = std::sync::Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>;

/// Shared reservation used by user-created and scheduled Code Studio tasks.
pub(crate) fn project_dispatch_lock(project_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<ProjectDispatchLocks> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut locks = locks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(lock) = locks.get(project_id).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(tokio::sync::Mutex::new(()));
    locks.insert(project_id.to_string(), Arc::downgrade(&lock));
    lock
}

pub fn spawn(data_dir: PathBuf, store_path: PathBuf, main_agent: MainAgent) {
    if std::env::var("AKASHA_STUDIO_KANBAN_DISPATCH_ENABLED").as_deref() == Ok("0") {
        tracing::info!("Code Studio Kanban auto-dispatch disabled by configuration");
        return;
    }
    let interval_secs = dispatch_interval_secs(
        std::env::var("AKASHA_STUDIO_KANBAN_DISPATCH_INTERVAL_SECS")
            .ok()
            .as_deref(),
    );
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            // Tokio's first tick is immediate, so the backlog is checked as soon
            // as the daemon starts instead of waiting a full interval.
            interval.tick().await;
            let dispatched = dispatch_idle_projects(&data_dir, &store_path, &main_agent).await;
            if dispatched > 0 {
                tracing::info!(
                    projects = dispatched,
                    "Code Studio Kanban ready work dispatched"
                );
            }
        }
    });
}

fn dispatch_interval_secs(configured: Option<&str>) -> u64 {
    configured
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_INTERVAL_SECS)
        .max(60)
}

async fn dispatch_idle_projects(
    data_dir: &Path,
    store_path: &Path,
    main_agent: &MainAgent,
) -> usize {
    let projects_dir = crate::studio::studio_projects_base(data_dir);
    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(error) => {
            tracing::warn!(error = %error, path = %projects_dir.display(), "Cannot scan Code Studio projects for ready Kanban work");
            return 0;
        }
    };
    let tasks = match TaskStore::open(store_path).and_then(|store| store.get_all()) {
        Ok(tasks) => tasks,
        Err(error) => {
            tracing::warn!(error = %error, "Cannot inspect active tasks before Kanban dispatch");
            return 0;
        }
    };
    let active_projects: std::collections::HashSet<String> = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Queued
                    | TaskStatus::Running
                    | TaskStatus::WaitingUserInput
            )
        })
        .filter_map(|task| task.studio_project_id.clone())
        .collect();
    let task_statuses: HashMap<String, TaskStatus> = tasks
        .into_iter()
        .map(|task| (task.id.to_string(), task.status))
        .collect();

    let mut dispatched = 0;
    for entry in entries.flatten() {
        let root = entry.path();
        if !root.is_dir() || !root.join(".akasha-studio.json").is_file() {
            continue;
        }
        let Some(project_id) = crate::studio::studio_project_id_from_disk_root(data_dir, &root)
        else {
            continue;
        };
        let project_lock = project_dispatch_lock(&project_id);
        let _project_reservation = project_lock.lock().await;
        if active_projects.contains(&project_id) {
            continue;
        }
        // Recheck after taking the same reservation used by user requests;
        // an earlier task-store snapshot alone leaves a dispatch race.
        if project_has_active_task(store_path, &project_id) {
            continue;
        }

        if let Err(error) = api_studio::studio_migrate_legacy_request_parent_dependencies(&root) {
            tracing::warn!(
                project_id,
                error = %error,
                "Cannot migrate legacy Code Studio request-ticket relationships"
            );
        }
        let _ = api_studio::studio_upgrade_initial_request(&root);
        let _ = api_studio::studio_complete_initial_backlog_if_ready(&root);
        let initial_ticket = studio_list_tickets(&root).into_iter().find(|ticket| {
            crate::api_studio::is_initial_backlog_root(ticket)
        });
        if let Some(ticket) = initial_ticket.as_ref() {
            if let Err(error) = api_studio::studio_seed_initial_backlog(&root, &ticket.id) {
                tracing::warn!(
                    project_id,
                    ticket_id = %ticket.id,
                    error = %error,
                    "Cannot seed the initial Code Studio Kanban backlog"
                );
            }
        }
        let tickets_before_reconciliation = studio_list_tickets(&root);
        for ticket in &tickets_before_reconciliation {
            record_terminal_task_failure(&root, ticket, &task_statuses, store_path);
            record_completed_task_without_review(&root, ticket, &task_statuses, store_path);
        }
        // Re-read after reconciliation so this pass reflects terminal task
        // failures immediately instead of leaving stale in-progress tickets.
        let tickets = studio_list_tickets(&root);
        let initial_ticket = tickets.iter().find(|ticket| {
            crate::api_studio::is_initial_backlog_root(ticket)
        }).cloned();
        let needs_backlog_planning = initial_ticket
            .as_ref()
            .is_some_and(|summary| initial_backlog_needs_planning(&root, summary, &tickets));
        let request_needing_correction_planning = (!needs_backlog_planning)
            .then(|| tickets.iter().find(|ticket| is_user_request_needing_corrective_planning(ticket, &tickets)))
            .flatten();
        let request_needing_planning = (!needs_backlog_planning && request_needing_correction_planning.is_none())
            .then(|| tickets.iter().find(|ticket| is_user_request_needing_planning(ticket, &tickets)))
            .flatten();
        let needs_user_request_correction_planning = request_needing_correction_planning.is_some();
        let needs_user_request_planning = request_needing_planning.is_some();
        let open_ticket_count = tickets
            .iter()
            .filter(|ticket| ticket.status != "done" && !is_initial_backlog_summary(ticket))
            .count();
        if open_ticket_count == 0 && !needs_backlog_planning {
            continue;
        }
        let now = chrono::Utc::now();
        let mut ready: Vec<StudioTicket> = if needs_backlog_planning {
            initial_ticket
                .as_ref()
                .filter(|ticket| ticket.status != "done")
                .filter(|ticket| studio_ticket_prerequisite_done(&root, ticket))
                .filter(|ticket| {
                    ticket_can_be_retried(ticket, &root, &task_statuses, store_path, now)
                })
                .cloned()
                .into_iter()
                .collect()
        } else if needs_user_request_planning || needs_user_request_correction_planning {
            request_needing_planning
                .or(request_needing_correction_planning)
                .filter(|ticket| ticket.status != "done")
                .filter(|ticket| studio_ticket_prerequisite_done(&root, ticket))
                .filter(|ticket| {
                    ticket_can_be_retried(ticket, &root, &task_statuses, store_path, now)
                })
                .cloned()
                .into_iter()
                .collect()
        } else {
            tickets
                .iter()
                .filter(|ticket| !is_initial_backlog_summary(ticket))
                .filter(|ticket| {
                    !is_user_request_tracker(ticket, &tickets)
                        || user_request_children_done(ticket, &tickets)
                })
                .filter(|ticket| ticket.status != "done")
                .filter(|ticket| studio_ticket_prerequisite_done(&root, ticket))
                .filter(|ticket| {
                    ticket_can_be_retried(ticket, &root, &task_statuses, store_path, now)
                })
                .cloned()
                .collect()
        };
        ready.sort_by(|left, right| {
            ticket_dispatch_priority(left, &task_statuses)
                .cmp(&ticket_dispatch_priority(right, &task_statuses))
                .then_with(|| {
                    failed_dispatch_count(left, &root, &task_statuses).cmp(&failed_dispatch_count(
                        right,
                        &root,
                        &task_statuses,
                    ))
                })
                .then_with(|| left.created_at.cmp(&right.created_at))
        });
        if !should_dispatch_pass(ready.len()) {
            continue;
        }
        if let Some(ticket) = ready.first_mut() {
            if let Err(error) = transition_ready_request_parent_to_review(&root, ticket, &tickets) {
                tracing::warn!(
                    project_id,
                    ticket_id = %ticket.id,
                    error = %error,
                    "Could not move completed Kanban parent into review"
                );
                continue;
            }
        }
        let selected_ticket = ready.first();
        let selected_ticket_json = selected_ticket.map(|ticket| {
            serde_json::json!({
                "id": ticket.id,
                "title": ticket.title,
                "initial_backlog_root": ticket.initial_backlog_root,
                "status": ticket.status,
                "parent_ticket_id": ticket.parent_ticket_id,
                "creation_key": ticket.creation_key,
                "assigned_agent": ticket.assigned_agent,
                "review_agent": ticket.review_agent,
                "depends_on_ticket_ids": ticket.depends_on_ticket_ids,
                "acceptance_criteria": ticket.acceptance_criteria,
                "description": ticket.description,
            })
        });
        let selected_agent = selected_ticket
            .map(|ticket| ticket_agent_for_dispatch(ticket, &tickets));
        let current_ticket_marker = ready
            .first()
            .map(|ticket| {
                format!(
                    "[Ticket Kanban obligatoire]\n- ticket_id: {}\n- titre: {}\n- agent: {}\n\n",
                    ticket.id,
                    ticket.title,
                    selected_agent.unwrap_or(ticket.assigned_agent.as_str())
                )
            })
            .unwrap_or_default();
        let selected_ticket_summary = selected_ticket.is_some_and(is_initial_backlog_summary);
        let selected_user_request_tracker =
            selected_ticket.is_some_and(|ticket| is_user_request_tracker(ticket, &tickets));
        let selected_ticket_json = selected_ticket_json
            .map(|ticket| serde_json::to_string(&ticket).unwrap_or_else(|_| "{}".to_string()))
            .unwrap_or_else(|| "aucun ticket prêt".to_string());
        let previous_failure = selected_ticket.and_then(|ticket| {
            terminal_failure_context(ticket, &task_statuses, store_path)
                .or_else(|| incomplete_task_context(ticket, &task_statuses, store_path))
                .or_else(|| recorded_ticket_execution_context(&root, ticket))
        });
        let message = if selected_ticket_summary {
            format!(
                "{current_ticket_marker}[Planification Kanban initiale Code Studio]\nProjet: {project_id}\nTicket global: {selected_ticket_json}\nLe backlog initial contient encore des phases génériques. Lis la demande et adapte d’abord les tickets gabarits existants; ne crée que les tâches réellement manquantes. La racine `workspace:/` est l’unique racine de l’application : inspecte la stack existante et ne planifie pas un second projet imbriqué. Chaque ticket de travail doit porter parent_ticket_id = id du ticket global et une creation_key stable unique dans ce parent. N’utilise depends_on_ticket_ids que pour une vraie dépendance d’exécution entre tickets de travail : le parent sert au suivi et ne bloque pas le dispatch. Relis la liste après chaque création/mise à jour et réutilise les IDs retournés; ne recrée jamais un ticket déjà présent. Le Kanban doit couvrir les fonctionnalités demandées, leur intégration et leur validation, avec des critères observables. N'édite pas les fichiers source pendant ce passage et ne marque pas le ticket global terminé. Termine quand au moins trois tickets précis remplacent les gabarits et que seuls les vrais prérequis sont ordonnés; le balayage suivant dispatchera le premier ticket prêt. Passage planifié, pas une nouvelle demande utilisateur."
            )
        } else if needs_user_request_correction_planning {
            format!(
                "{current_ticket_marker}[Planification Kanban des corrections demandées en review]\nProjet: {project_id}\nTicket global: {selected_ticket_json}\nLe reviewer a demandé des corrections pour le ticket global. Lis la review et les tickets du projet, puis crée ou réutilise un ou plusieurs tickets de correction avec parent_ticket_id = id du ticket global, une creation_key stable, des critères observables et des agents responsables. Ne mets pas le ticket global dans depends_on_ticket_ids : son rôle est de suivre et valider les corrections, pas de les bloquer. Ne modifie aucun fichier source pendant cette planification et ne relance pas le reviewer avant que les corrections soient terminées. Le balayage suivant dispatchera les tickets de correction prêts. Passage planifié, pas une nouvelle demande utilisateur."
            )
        } else if needs_user_request_planning {
            format!(
                "{current_ticket_marker}[Planification Kanban d'une demande d'évolution Code Studio]\nProjet: {project_id}\nTicket global: {selected_ticket_json}\nAvant toute modification des fichiers, transforme cette demande en travail suivi dans le Kanban. Décide du découpage à partir des critères et du périmètre: crée ou réutilise au moins un ticket actionnable avec parent_ticket_id = id de ce ticket global et une creation_key stable. Une évolution simple reçoit exactement un ticket de travail; utilise plusieurs tickets pour les changements indépendants ou séquentiels. Ajoute dans depends_on_ticket_ids uniquement les vrais prérequis entre tickets de travail, jamais le ticket global. Avant de créer, relis les tickets et réutilise l’ID existant si le travail est déjà planifié. Chaque ticket doit avoir des critères d'acceptation observables, un agent responsable et un reviewer. Mets à jour le ticket global avec ses critères vérifiables et conserve-le comme suivi. Ne modifie aucun fichier source et ne ferme pas le ticket global pendant cette planification. Le balayage suivant dispatchera le premier ticket prêt. Passage planifié, pas une nouvelle demande utilisateur."
            )
        } else if selected_user_request_tracker {
            format!(
                "{current_ticket_marker}[Revue finale du ticket parent Kanban]\nProjet: {project_id}\nTicket global: {selected_ticket_json}\nTous les tickets enfants liés sont terminés. Tu es le reviewer assigné : vérifie chaque critère du ticket parent à partir des preuves et du dépôt. N'effectue pas une nouvelle implémentation générale. Si les critères sont satisfaits, passe le ticket parent en done avec les preuves observées. Sinon, repasse-le en in_progress et crée ou réactive les tickets de correction manquants sous ce parent; seuls les vrais prérequis de correction vont dans depends_on_ticket_ids. Passage planifié, pas une nouvelle demande utilisateur."
            )
        } else if let Some(failure_context) = previous_failure {
            format!(
                "{current_ticket_marker}[Reprise Kanban après échec d'un agent]\nProjet: {project_id}\nTicket: {selected_ticket_json}\nÉchec précédent: {failure_context}\nLe précédent passage s'est terminé sans valider le ticket. Reprends dans un nouveau contexte à partir de l'état réel des fichiers. Commence par le premier critère encore en échec et les éléments de diagnostic ci-dessus. Ne répète pas un appel d'outil identique qui a déjà échoué ou tourné en boucle. Ne prétends pas qu'une commande ou un changement a réussi sans son résultat observé. Si le ticket est trop large, mets à jour le Kanban avec des sous-tâches ciblées et des dépendances; s'il est bloqué par une limite persistante, consigne la cause et les prochaines étapes au lieu de relancer la même tentative. Passage planifié, pas une nouvelle demande utilisateur."
            )
        } else {
            format!(
                "{current_ticket_marker}[Passage automatique du Kanban Code Studio]\nProjet: {project_id}\nTickets ouverts prêts à traiter: {}\nTicket sélectionné pour ce passage: {}\nLe balayage daemon a déjà vérifié le tableau, les dépendances et l'absence de tâche active pour ce projet. Tu es l'agent assigné au ticket sélectionné: traite-le directement, sans redéléguer et sans prendre un autre ticket. Pour un ticket de travail, implémente ses critères puis exécute les vérifications adaptées; mets à jour le ticket avec les fichiers et résultats réellement observés, et passe-le en review uniquement si les critères sont satisfaits. Pour un ticket de review, inspecte le travail sans modifier les fichiers source puis consigne le verdict et les preuves; ferme-le uniquement si tout est validé, sinon repasse-le en in_progress avec les corrections nécessaires. Si un bug distinct apparaît, crée un ticket avec résultat attendu, résultat observé et reproduction. S'il n'y a aucun ticket prêt, n'implémente rien. Passage planifié, pas une nouvelle demande utilisateur.",
                ready.len(), selected_ticket_json
            )
        };
        let mut envelope = MessageEnvelope::api(
            format!(
                "studio-kanban-{project_id}-{}",
                uuid::Uuid::new_v4().simple()
            ),
            message,
            None,
            TaskPriority::Scheduled,
            false,
        );
        envelope.display_request = Some("Suivi automatique des tickets Kanban ouverts".to_string());
        envelope.studio_disk_root = Some(root.clone());
        envelope.studio_forced_agent = selected_agent.map(str::to_string);
        envelope.studio_execution =
            api_studio::studio_project_model_preference(&root).map(|model| {
                crate::studio_execution::Execution {
                    model: Some(model),
                    ..crate::studio_execution::Execution::default()
                }
            });
        // User requests may arrive after the initial project snapshot above.
        // Re-read the task store immediately before dispatch so a scan racing
        // with a newly-created Code Studio task does not start a second writer.
        if project_has_active_task(store_path, &project_id) {
            continue;
        }
        match handle_envelope(main_agent, store_path, envelope).await {
            Ok(task_id) => {
                if let Some(ticket) = selected_ticket {
                    let _ = api_studio_event(&root, ticket, task_id, project_id.as_str());
                    let _ = api_studio::studio_attach_task_to_ticket(
                        &root,
                        &ticket.id,
                        &task_id.to_string(),
                        "kanban_scheduler",
                    );
                }
                dispatched += 1;
            }
            Err(error) => tracing::warn!(
                project_id,
                error = %error,
                    "Could not start scheduled Code Studio Kanban ticket"
            ),
        }
    }
    dispatched
}

fn should_dispatch_pass(ready_ticket_count: usize) -> bool {
    ready_ticket_count > 0
}

fn ticket_agent_for_dispatch<'a>(
    ticket: &'a StudioTicket,
    tickets: &[StudioTicket],
) -> &'a str {
    if is_initial_backlog_summary(ticket) {
        return "studio_project_manager";
    }
    if is_user_request_needing_planning(ticket, tickets) {
        return "studio_project_manager";
    }
    if is_user_request_tracker(ticket, tickets) && user_request_children_done(ticket, tickets) {
        return if ticket.review_outcome.as_deref() == Some("changes_requested") {
            "studio_project_manager"
        } else {
            ticket.review_agent.as_str()
        };
    }
    if ticket.status == "review" || ticket.evidence.resume_status.as_deref()==Some("review") {
        ticket.review_agent.as_str()
    } else {
        ticket.assigned_agent.as_str()
    }
}

fn transition_ready_request_parent_to_review(
    project_root: &Path,
    ticket: &mut StudioTicket,
    tickets: &[StudioTicket],
) -> Result<(), String> {
    let review_ready_parent = is_user_request_tracker(ticket, tickets)
        && user_request_children_done(ticket, tickets)
        && ticket.review_outcome.as_deref() != Some("changes_requested");
    if review_ready_parent && ticket.status != "review" {
        ticket.status = "review".to_string();
        ticket.updated_at = chrono::Utc::now().to_rfc3339();
        api_studio::studio_upsert_ticket(project_root, ticket.clone())?;
    }
    Ok(())
}

fn is_user_request_tracker(ticket: &StudioTicket, tickets: &[StudioTicket]) -> bool {
    ticket.requested_by == "user_chat"
        && tickets.iter().any(|candidate| {
            candidate.id != ticket.id
                && ticket_belongs_to_parent(candidate, &ticket.id)
        })
}

fn is_user_request_needing_planning(ticket: &StudioTicket, tickets: &[StudioTicket]) -> bool {
    ticket.requested_by == "user_chat"
        && ticket.status != "done"
        && ticket.status != "blocked"
        && !is_initial_backlog_summary(ticket)
        && !tickets.iter().any(|candidate| {
            candidate.id != ticket.id
                && ticket_belongs_to_parent(candidate, &ticket.id)
        })
}

fn is_user_request_needing_corrective_planning(ticket: &StudioTicket, tickets: &[StudioTicket]) -> bool {
    is_user_request_tracker(ticket, tickets)
        && ticket.status != "done"
        && ticket.status != "blocked"
        && user_request_children_done(ticket, tickets)
        && ticket.review_outcome.as_deref() == Some("changes_requested")
}

fn user_request_children_done(ticket: &StudioTicket, tickets: &[StudioTicket]) -> bool {
    let mut children = tickets.iter().filter(|candidate| {
        candidate.id != ticket.id
            && ticket_belongs_to_parent(candidate, &ticket.id)
    });
    let Some(first) = children.next() else {
        return false;
    };
    first.status == "done" && children.all(|child| child.status == "done")
}

fn ticket_belongs_to_parent(ticket: &StudioTicket, parent_id: &str) -> bool {
    ticket.parent_ticket_id.as_deref() == Some(parent_id)
        || ticket
            .depends_on_ticket_ids
            .iter()
            .any(|dependency| dependency == parent_id)
}

fn initial_backlog_needs_planning(
    project_root: &Path,
    summary: &StudioTicket,
    _tickets: &[StudioTicket],
) -> bool {
    is_initial_backlog_summary(summary)
        && crate::studio_planning::validate(project_root, summary).is_err()
}

pub(crate) fn project_has_active_task(store_path: &Path, project_id: &str) -> bool {
    let Ok(store) = TaskStore::open(store_path) else {
        // Fail closed: never dispatch when project activity cannot be checked.
        return true;
    };
    let Ok(tasks) = store.get_all() else {
        return true;
    };
    tasks.iter().any(|task| {
        task.studio_project_id.as_deref() == Some(project_id)
            && matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Queued
                    | TaskStatus::Running
                    | TaskStatus::WaitingUserInput
            )
    })
}

fn ticket_can_be_retried(
    ticket: &StudioTicket,
    project_root: &Path,
    task_statuses: &HashMap<String, TaskStatus>,
    store_path: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    if ticket.status == "todo" {
        return true;
    }
    let automatically_blocked =
        ticket.status == "blocked" && has_automatic_failure_for_current_task(ticket, project_root);
    if ticket.status != "in_progress" && ticket.status != "review" && !automatically_blocked {
        return false;
    }
    if automatically_blocked {
        let attempts = automatic_failure_attempt_count(ticket, project_root);
        if attempts >= MAX_AUTOMATIC_FAILURE_ATTEMPTS {
            return false;
        }
        // Exhausted Studio routes and local context rejection require a route
        // change or explicit resume, rather than another identical dispatch.
        if requires_route_change(ticket, task_statuses, store_path) {
            return false;
        }
    }
    let related_task_terminal = ticket
        .related_task_id
        .as_ref()
        .and_then(|id| task_statuses.get(id))
        .is_none_or(|status| {
            matches!(
                status,
                TaskStatus::Completed
                    | TaskStatus::Failed
                    | TaskStatus::Cancelled
                    | TaskStatus::Interrupted
            )
        });
    if !related_task_terminal {
        return false;
    }
    let last_dispatch = studio_list_ticket_events(project_root, &ticket.id)
        .into_iter()
        .filter(|event| matches!(
            event.event_type.as_str(),
            "ticket_dispatch_scheduled" | "ticket_execution_failed" | "ticket_execution_incomplete"
        ))
        .filter_map(|event| chrono::DateTime::parse_from_rfc3339(&event.at).ok())
        .max();
    let cooldown_secs = std::env::var("AKASHA_STUDIO_KANBAN_RETRY_COOLDOWN_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(DEFAULT_RETRY_COOLDOWN_SECS)
        .clamp(60, 3600);
    let failed_attempts = failed_dispatch_count(ticket, project_root, task_statuses);
    let effective_cooldown = retry_cooldown_secs(cooldown_secs, failed_attempts);
    retry_cooldown_elapsed(last_dispatch, now, effective_cooldown)
}

fn requires_route_change(
    ticket: &StudioTicket,
    task_statuses: &HashMap<String, TaskStatus>,
    store_path: &Path,
) -> bool {
    let Some(task_id) = ticket.related_task_id.as_deref() else {
        return false;
    };
    if !task_statuses.get(task_id).is_some_and(|status| {
        matches!(
            status,
            TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted
        )
    }) {
        return false;
    }
    let Ok(task_id) = uuid::Uuid::parse_str(task_id) else {
        return false;
    };
    TaskStore::open(store_path)
        .ok()
        .and_then(|store| store.get_outcome(task_id).ok().flatten())
        .is_some_and(|outcome| {
            let reason=outcome.to_string();
            reason.contains("context_window_preflight_rejected") || reason.contains("studio_routes_exhausted")
                || ["studio_no_verified_progress", "studio_command_verification_failed", "studio_round_budget_exhausted"].iter().any(|code| reason.contains(code))
        })
}

fn has_automatic_failure_for_current_task(ticket: &StudioTicket, project_root: &Path) -> bool {
    let Some(task_id) = ticket.related_task_id.as_deref() else {
        return false;
    };
    studio_list_ticket_events(project_root, &ticket.id).iter().any(|event| {
        matches!(
            event.event_type.as_str(),
            "ticket_execution_failed" | "ticket_execution_incomplete"
        ) && event
            .payload
            .as_ref()
            .and_then(|payload| payload.get("task_id"))
            .and_then(serde_json::Value::as_str)
            == Some(task_id)
    })
}

fn automatic_failure_attempt_count(ticket: &StudioTicket, project_root: &Path) -> usize {
    let mut task_ids = std::collections::HashSet::new();
    studio_list_ticket_events(project_root, &ticket.id)
        .into_iter()
        .filter(|event| {
            matches!(
                event.event_type.as_str(),
                "ticket_execution_failed" | "ticket_execution_incomplete"
            )
        })
        .filter_map(|event| event.payload)
        .filter_map(|payload| {
            payload
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .filter(|task_id| task_ids.insert(task_id.clone()))
        .count()
}

fn failed_dispatch_count(
    ticket: &StudioTicket,
    project_root: &Path,
    task_statuses: &HashMap<String, TaskStatus>,
) -> u32 {
    studio_list_ticket_events(project_root, &ticket.id)
        .into_iter()
        .filter(|event| event.event_type == "ticket_dispatch_scheduled")
        .filter_map(|event| event.payload)
        .filter_map(|payload| payload.get("task_id")?.as_str().map(str::to_string))
        .filter(|task_id| {
            task_statuses.get(task_id).is_some_and(|status| {
                matches!(
                    status,
                    TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted
                )
            })
        })
        .count()
        .min(u32::MAX as usize) as u32
}

fn retry_cooldown_secs(base_secs: i64, failed_attempts: u32) -> i64 {
    let exponent = failed_attempts.saturating_sub(1).min(6);
    base_secs
        .saturating_mul(1_i64 << exponent)
        .clamp(base_secs, 3600)
}

fn ticket_dispatch_priority(
    ticket: &StudioTicket,
    task_statuses: &HashMap<String, TaskStatus>,
) -> u8 {
    match ticket.status.as_str() {
        "review" => 0,
        "todo" => 1,
        "in_progress"
            if ticket
                .related_task_id
                .as_ref()
                .and_then(|id| task_statuses.get(id))
                .is_some_and(|status| {
                    matches!(
                        status,
                        TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted
                    )
                }) =>
        {
            2
        }
        _ => 3,
    }
}

fn terminal_failure_context(
    ticket: &StudioTicket,
    task_statuses: &HashMap<String, TaskStatus>,
    store_path: &Path,
) -> Option<String> {
    let task_id = ticket.related_task_id.as_ref()?;
    let status = task_statuses.get(task_id)?;
    if !matches!(
        status,
        TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted
    ) {
        return None;
    }
    let task_uuid = uuid::Uuid::parse_str(task_id).ok()?;
    let outcome = TaskStore::open(store_path)
        .ok()?
        .get_outcome(task_uuid)
        .ok()?;
    let termination_reason = outcome
        .as_ref()
        .and_then(|value| {
            value
                .get("termination_reason")
                .and_then(|item| item.as_str())
        })
        .unwrap_or("unknown");
    let reason = outcome
        .as_ref()
        .and_then(|value| value.get("reason").and_then(|item| item.as_str()))
        .unwrap_or("aucune cause structurée n'est enregistrée");
    let reason = reason.chars().take(500).collect::<String>();
    Some(format!(
        "task_id={task_id}, statut={}, motif={}, cause={reason}",
        status.as_str(),
        termination_reason
    ))
}

fn incomplete_task_context(
    ticket: &StudioTicket,
    task_statuses: &HashMap<String, TaskStatus>,
    store_path: &Path,
) -> Option<String> {
    if ticket.status != "in_progress" {
        return None;
    }
    let task_id = ticket.related_task_id.as_ref()?;
    if task_statuses.get(task_id) != Some(&TaskStatus::Completed) {
        return None;
    }
    let task_uuid = uuid::Uuid::parse_str(task_id).ok()?;
    let outcome = TaskStore::open(store_path)
        .ok()?
        .get_outcome(task_uuid)
        .ok()??;
    let acceptance_verified = outcome
        .get("acceptance_verified")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let confirmed_file_mutation = outcome
        .get("confirmed_file_mutation")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let failed_tool_calls = outcome
        .get("failed_tool_calls")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let details = ["verification", "checks"]
        .iter()
        .flat_map(|key| {
            outcome
                .get(*key)
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|check| {
            matches!(
                check.get("kind").and_then(serde_json::Value::as_str),
                Some("tool_protocol_retry") | Some("tool_failure")
            )
        })
        .filter_map(|check| check.get("detail").and_then(serde_json::Value::as_str))
        .take(3)
        .map(|detail| detail.chars().take(300).collect::<String>())
        .collect::<Vec<_>>();
    Some(format!(
        "task_id={task_id}, run completed while ticket remained in_progress, acceptance_verified={acceptance_verified}, confirmed_file_mutation={confirmed_file_mutation}, failed_tool_calls={failed_tool_calls}, details={}",
        if details.is_empty() {
            "no structured tool failure details".to_string()
        } else {
            details.join(" | ")
        }
    ))
}

fn record_completed_task_without_review(
    project_root: &Path,
    ticket: &StudioTicket,
    task_statuses: &HashMap<String, TaskStatus>,
    store_path: &Path,
) {
    if ticket.status != "in_progress" {
        return;
    }
    let Some(task_id) = ticket.related_task_id.as_deref() else {
        return;
    };
    if task_statuses.get(task_id) != Some(&TaskStatus::Completed) {
        return;
    }
    // A verified planning pass hands the still-open tracker to its children.
    // It is not an incomplete implementation, and must not incur a retry cooldown.
    if is_initial_backlog_summary(ticket)
        && TaskStore::open(store_path).ok()
            .and_then(|store| uuid::Uuid::parse_str(task_id).ok().and_then(|id|store.get_outcome(id).ok().flatten()))
            .is_some_and(|outcome| outcome["planning_verified"]==true)
        && crate::studio_planning::validate(project_root,ticket).is_ok()
    { return; }
    let Some(context) = incomplete_task_context(ticket, task_statuses, store_path) else {
        return;
    };
    if studio_list_ticket_events(project_root, &ticket.id)
        .iter()
        .any(|event| {
            event.event_type == "ticket_execution_incomplete"
                && event
                    .payload
                    .as_ref()
                    .is_some_and(|payload| {
                        payload.get("task_id").and_then(serde_json::Value::as_str)
                            == Some(task_id)
                            && payload.get("detail").and_then(serde_json::Value::as_str)
                                == Some(context.as_str())
                    })
        })
    {
        return;
    }
    if ticket.status == "in_progress" {
        let mut blocked = ticket.clone();
        blocked.status = "blocked".to_string();
        blocked.evidence.summary = Some(format!("Vérification incomplète : {context}"));
        blocked.updated_at = chrono::Utc::now().to_rfc3339();
        let _ = api_studio::studio_upsert_ticket(project_root, blocked);
    }
    let _ = api_studio::studio_append_ticket_event(
        project_root,
        &ticket.id,
        "ticket_execution_incomplete",
        "kanban_scheduler",
        Some(serde_json::json!({
            "task_id": task_id,
            "detail": context,
        })),
    );
}

fn recorded_ticket_execution_context(
    project_root: &Path,
    ticket: &StudioTicket,
) -> Option<String> {
    studio_list_ticket_events(project_root, &ticket.id)
        .into_iter()
        .rev()
        .find(|event| {
            matches!(
                event.event_type.as_str(),
                "ticket_execution_incomplete" | "ticket_execution_failed"
            )
        })
        .and_then(|event| event.payload)
        .and_then(|payload| payload.get("detail").and_then(serde_json::Value::as_str).map(str::to_string))
}

fn record_terminal_task_failure(
    project_root: &Path,
    ticket: &StudioTicket,
    task_statuses: &HashMap<String, TaskStatus>,
    store_path: &Path,
) {
    // A delayed event from an older run must not overwrite a newer assignment
    // or a reviewed result.
    let Some(current)=api_studio::studio_get_ticket(project_root,&ticket.id) else {return;};
    if current.related_task_id != ticket.related_task_id || current.status == "done" {return;}
    let Some(task_id) = ticket.related_task_id.as_deref() else {
        return;
    };
    let Some(status) = task_statuses.get(task_id).filter(|status| {
        matches!(
            status,
            TaskStatus::Failed | TaskStatus::Cancelled | TaskStatus::Interrupted
        )
    }) else {
        return;
    };
    let context = terminal_failure_context(ticket, task_statuses, store_path);
    if matches!(current.status.as_str(),"in_progress"|"review") {
        let mut blocked=current;
        blocked.evidence.resume_status=Some(blocked.status.clone());
        blocked.status="blocked".into();
        blocked.evidence.summary=Some(context.clone().unwrap_or_else(||format!("La tâche liée s’est terminée avec le statut {}.",status.as_str())));
        blocked.updated_at=chrono::Utc::now().to_rfc3339();
        if api_studio::studio_upsert_ticket(project_root,blocked).is_err() {return;}
    }
    let terminal_updated_at=uuid::Uuid::parse_str(task_id).ok().and_then(|id|TaskStore::open(store_path).ok()?.get(id).ok().flatten()).map(|task|task.updated_at.to_rfc3339());
    if studio_list_ticket_events(project_root, &ticket.id)
        .iter()
        .any(|event| {
            event.event_type == "ticket_execution_failed"
                && event
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("task_id"))
                    .and_then(|value| value.as_str())
                    == Some(task_id)
                && event.payload.as_ref().and_then(|payload|payload.get("task_updated_at")).and_then(|value|value.as_str()) == terminal_updated_at.as_deref()
        })
    {
        return;
    }
    let _ = api_studio::studio_append_ticket_event(
        project_root,
        &ticket.id,
        "ticket_execution_failed",
        "kanban_scheduler",
        Some(serde_json::json!({
            "task_id": task_id,
            "task_status": status.as_str(),
            "task_updated_at": terminal_updated_at,
            "detail": context,
        })),
    );
}

/// Terminal task status is reconciled independently of automatic dispatch.
/// Called on completion and when the board is read, including after a restart.
pub(crate) fn reconcile_project_tickets(project_root:&Path,store_path:&Path) {
    let Ok(store)=TaskStore::open(store_path) else {return;};
    let tickets=studio_list_tickets(project_root);
    let statuses:HashMap<String,TaskStatus>=tickets.iter().filter_map(|ticket| {
        let id=ticket.related_task_id.as_ref()?;
        let task=store.get(uuid::Uuid::parse_str(id).ok()?).ok()??;
        Some((id.clone(),task.status))
    }).collect();
    for ticket in tickets {
        record_terminal_task_failure(project_root,&ticket,&statuses,store_path);
    }
}

fn is_initial_backlog_summary(ticket: &StudioTicket) -> bool {
    crate::api_studio::is_initial_backlog_root(ticket)
}

fn retry_cooldown_elapsed(
    last_dispatch: Option<chrono::DateTime<chrono::FixedOffset>>,
    now: chrono::DateTime<chrono::Utc>,
    cooldown_secs: i64,
) -> bool {
    last_dispatch.is_none_or(|at| {
        now.signed_duration_since(at.with_timezone(&chrono::Utc))
            .num_seconds()
            >= cooldown_secs
    })
}

fn api_studio_event(
    project_root: &Path,
    ticket: &StudioTicket,
    task_id: uuid::Uuid,
    project_id: &str,
) -> Result<(), String> {
    api_studio::studio_append_ticket_event(
        project_root,
        &ticket.id,
        "ticket_dispatch_scheduled",
        "kanban_scheduler",
        Some(serde_json::json!({
            "task_id": task_id,
            "project_id": project_id,
            "source": "periodic_kanban_scan"
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api_studio::{StudioTicketAcceptanceCriterion, StudioTicketEvidence};

    fn ticket(status: &str, related_task_id: Option<&str>) -> StudioTicket {
        StudioTicket {
            initial_backlog_root: false,
            id: "ticket-1".to_string(),
            project_id: "project-1".to_string(),
            title: "Implement feature".to_string(),
            description: "Feature request".to_string(),
            status: status.to_string(),
            requested_by: "user_chat".to_string(),
            assigned_agent: "studio_fullstack".to_string(),
            review_agent: "studio_reviewer".to_string(),
            parent_ticket_id: None,
            creation_key: None,
            depends_on_ticket_ids: Vec::new(),
            depends_on_ticket_id: None,
            related_task_id: related_task_id.map(str::to_string),
            acceptance_criteria: vec![StudioTicketAcceptanceCriterion {
                id: Some("result".to_string()),
                text: "Feature works".to_string(),
                kind: None,
                path: None,
                argv: None,
            }],
            evidence: StudioTicketEvidence::default(),
            review_outcome: None,
            review_notes: None,
            corrective_steps: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn copied_criterion_on_a_work_phase_keeps_its_specialist_owner() {
        let mut phase = ticket("todo", None);
        phase.requested_by = "system_initial_backlog".into();
        phase.parent_ticket_id = Some("real-root".into());
        phase.creation_key = Some("initial:real-root:validation".into());
        phase.acceptance_criteria[0].id = Some("initial-kanban-backlog".into());
        assert!(!is_initial_backlog_summary(&phase));
        assert_eq!(ticket_agent_for_dispatch(&phase, &[phase.clone()]), "studio_fullstack");
    }
    #[test]
    fn initial_request_summary_is_not_dispatched_as_an_implementation_ticket() {
        let mut summary = ticket("in_progress", None);
        summary.initial_backlog_root = true;
        summary.acceptance_criteria[0].id = Some("initial-kanban-backlog".to_string());
        assert!(is_initial_backlog_summary(&summary));
        assert!(!is_initial_backlog_summary(&ticket("todo", None)));
    }

    #[test]
    fn user_request_tracker_waits_for_children_then_routes_to_reviewer() {
        let mut parent = ticket("in_progress", None);
        parent.id = "request".to_string();
        let mut child = ticket("todo", None);
        child.id = "work".to_string();
        child.requested_by = "studio_project_manager".to_string();
        child.depends_on_ticket_ids = vec![parent.id.clone()];
        let tickets = vec![parent.clone(), child.clone()];

        assert!(is_user_request_tracker(&parent, &tickets));
        assert!(!user_request_children_done(&parent, &tickets));
        assert_eq!(
            ticket_agent_for_dispatch(&parent, &tickets),
            "studio_fullstack"
        );

        child.status = "done".to_string();
        let tickets = vec![parent.clone(), child];
        assert!(user_request_children_done(&parent, &tickets));
        assert_eq!(
            ticket_agent_for_dispatch(&parent, &tickets),
            "studio_reviewer"
        );

        parent.review_outcome = Some("changes_requested".to_string());
        let tickets = vec![parent.clone(), tickets[1].clone()];
        assert!(is_user_request_needing_corrective_planning(&parent, &tickets));
        assert_eq!(
            ticket_agent_for_dispatch(&parent, &tickets),
            "studio_project_manager"
        );
    }

    #[test]
    fn every_new_chat_evolution_is_planned_into_kanban_work_before_dispatch() {
        let request = ticket("in_progress", Some("completed-chat-task"));
        assert!(is_user_request_needing_planning(&request, std::slice::from_ref(&request)));
        assert_eq!(
            ticket_agent_for_dispatch(&request, std::slice::from_ref(&request)),
            "studio_project_manager"
        );

        let mut work = ticket("todo", None);
        work.id = "work".to_string();
        work.requested_by = "studio_project_manager".to_string();
        work.depends_on_ticket_ids = vec![request.id.clone()];
        let tickets = vec![request.clone(), work];
        assert!(!is_user_request_needing_planning(&request, &tickets));
        assert!(is_user_request_tracker(&request, &tickets));
    }

    #[test]
    fn completed_chat_requests_do_not_starve_open_kanban_work() {
        let mut completed = ticket("done", None);
        completed.id = "completed-request".to_string();
        let mut open = ticket("todo", None);
        open.id = "open-request".to_string();
        let tickets = vec![completed, open.clone()];

        let selected = tickets
            .iter()
            .find(|candidate| is_user_request_needing_planning(candidate, &tickets));

        assert_eq!(selected.map(|candidate| candidate.id.as_str()), Some("open-request"));
    }

    #[test]
    fn blocked_chat_requests_do_not_starve_open_kanban_work() {
        let mut blocked = ticket("blocked", None);
        blocked.id = "blocked-request".to_string();
        let mut open = ticket("todo", None);
        open.id = "open-request".to_string();
        let tickets = vec![blocked, open.clone()];

        let selected = tickets
            .iter()
            .find(|candidate| is_user_request_needing_planning(candidate, &tickets));

        assert_eq!(selected.map(|candidate| candidate.id.as_str()), Some("open-request"));
    }

    #[test]
    fn completed_parent_with_old_review_feedback_does_not_trigger_replanning() {
        let mut parent = ticket("done", Some("parent-task"));
        let mut child = ticket("done", Some("child-task"));
        child.id = "child".to_string();
        child.depends_on_ticket_ids = vec![parent.id.clone()];
        parent.review_outcome = Some("changes_requested".to_string());
        let tickets = vec![parent.clone(), child];

        assert!(!is_user_request_needing_corrective_planning(&parent, &tickets));
    }

    #[test]
    fn empty_scheduled_pass_is_skipped_unless_backlog_planning_is_needed() {
        assert!(!should_dispatch_pass(0));
        assert!(should_dispatch_pass(1));
    }

    #[test]
    fn scheduled_kanban_scan_defaults_to_one_minute_and_respects_safe_minimum() {
        assert_eq!(dispatch_interval_secs(None), 60);
        assert_eq!(dispatch_interval_secs(Some("invalid")), 60);
        assert_eq!(dispatch_interval_secs(Some("10")), 60);
        assert_eq!(dispatch_interval_secs(Some("120")), 120);
    }

    #[test]
    fn scheduled_passes_go_to_the_ticket_owner_then_the_reviewer() {
        let mut current = ticket("in_progress", Some("work-task"));
        current.requested_by = "studio_project_manager".to_string();
        assert_eq!(
            ticket_agent_for_dispatch(&current, &[]),
            "studio_fullstack"
        );

        assert_eq!(
            ticket_agent_for_dispatch(&current, &[]),
            "studio_fullstack"
        );

        current.review_outcome = Some("changes_requested".to_string());
        assert_eq!(
            ticket_agent_for_dispatch(&current, &[]),
            "studio_fullstack"
        );

        current.status = "review".to_string();
        assert_eq!(
            ticket_agent_for_dispatch(&current, &[]),
            "studio_reviewer"
        );
    }

    #[test]
    fn completed_unverified_work_keeps_its_implementation_owner_and_failure_context() {
        use akasha_store::{Task, TaskStatus, TaskStore};

        let dir = tempfile::tempdir().expect("task store");
        let store_path = dir.path().join("tasks.db");
        let store = TaskStore::open(&store_path).expect("open task store");
        let task_id = uuid::Uuid::new_v4();
        let now = chrono::Utc::now();
        store
            .insert(&Task {
                id: task_id,
                parent_task_id: None,
                status: TaskStatus::Running,
                assigned_agent: "studio_fullstack".to_string(),
                created_at: now,
                updated_at: now,
                initial_message: None,
                studio_project_id: Some("project-1".to_string()),
            })
            .expect("insert task");
        store
            .finalize_outcome(
                task_id,
                TaskStatus::Completed,
                serde_json::json!({
                    "acceptance_verified": false,
                    "confirmed_file_mutation": false,
                    "failed_tool_calls": 2,
                    "checks": [{
                        "kind": "tool_protocol_retry",
                        "detail": "Qwen parameter body incomplete"
                    }]
                }),
            )
            .expect("save outcome");

        let mut current = ticket("in_progress", Some(&task_id.to_string()));
        current.requested_by = "studio_project_manager".to_string();
        let statuses = HashMap::from([(task_id.to_string(), TaskStatus::Completed)]);
        assert_eq!(
            ticket_agent_for_dispatch(&current, &[]),
            "studio_fullstack"
        );
        let context = incomplete_task_context(&current, &statuses, &store_path)
            .expect("unverified completed task context");
        assert!(context.contains("acceptance_verified=false"));
        assert!(context.contains("failed_tool_calls=2"));
        assert!(context.contains("Qwen parameter body incomplete"));
    }

    #[test]
    fn incomplete_initial_backlog_schedules_the_project_manager_before_work() {
        let mut summary = ticket("in_progress", None);
        summary.id = "initial-request".to_string();
        summary.initial_backlog_root = true;
        summary.acceptance_criteria[0].id = Some("initial-kanban-backlog".to_string());
        let mut generic = ticket("todo", None);
        generic.id = "generic-phase".to_string();
        generic.title = "Établir le socle du projet".to_string();
        generic.acceptance_criteria[0].id = Some("initial-phase-0".to_string());
        let dir = tempfile::tempdir().expect("project root");
        assert!(initial_backlog_needs_planning(
            dir.path(),
            &summary,
            &[summary.clone(), generic]
        ));
        assert_eq!(
            ticket_agent_for_dispatch(&summary, &[]),
            "studio_project_manager"
        );

        let mut foundation = ticket("todo", None);
        foundation.id = "foundation".to_string();
        foundation.title = "Create the Bomberman arena and player controls".to_string();
        foundation.acceptance_criteria[0].id = Some("arena-controls".to_string());
        let mut features = ticket("todo", None);
        features.id = "features".to_string();
        features.title = "Implement bombs, explosions, and destructible crates".to_string();
        features.acceptance_criteria[0].id = Some("bombs-and-crates".to_string());
        features.depends_on_ticket_ids = vec![foundation.id.clone()];
        let mut validation = ticket("todo", None);
        validation.id = "validation".to_string();
        validation.title = "Verify the playable Bomberman flow".to_string();
        validation.acceptance_criteria[0].id = Some("playable-validation".to_string());
        validation.depends_on_ticket_ids = vec![features.id.clone()];
        let mut refined = vec![summary.clone(), foundation, features, validation];
        for child in refined.iter_mut().skip(1) {
            child.parent_ticket_id = Some(summary.id.clone());
        }
        for item in &refined { api_studio::studio_upsert_ticket(dir.path(), item.clone()).unwrap(); }
        assert!(!initial_backlog_needs_planning(
            dir.path(),
            &summary,
            &refined
        ));
    }

    #[test]
    fn project_activity_is_rechecked_before_scheduled_dispatch() {
        let dir = tempfile::tempdir().expect("task store directory");
        let store_path = dir.path().join("tasks.db");
        let store = TaskStore::open(&store_path).expect("open task store");
        let now = chrono::Utc::now();
        store
            .insert(&akasha_store::Task {
                id: uuid::Uuid::new_v4(),
                parent_task_id: None,
                status: TaskStatus::Running,
                assigned_agent: "studio_project_manager".to_string(),
                created_at: now,
                updated_at: now,
                initial_message: None,
                studio_project_id: Some("project-active".to_string()),
            })
            .expect("insert active project task");

        assert!(project_has_active_task(&store_path, "project-active"));
        assert!(!project_has_active_task(&store_path, "another-project"));
    }

    #[test]
    fn project_dispatch_reservation_is_shared_by_project_id() {
        let first = project_dispatch_lock("same-project");
        let second = project_dispatch_lock("same-project");
        let other = project_dispatch_lock("other-project");
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert!(!std::sync::Arc::ptr_eq(&first, &other));
    }

    #[test]
    fn completed_user_request_parent_enters_review_before_reviewer_dispatch() {
        let dir = tempfile::tempdir().expect("project root");
        let mut parent = ticket("in_progress", None);
        parent.id = "request-parent".to_string();
        let mut child = ticket("done", None);
        child.id = "request-child".to_string();
        child.parent_ticket_id = Some(parent.id.clone());
        let tickets = vec![parent.clone(), child];

        transition_ready_request_parent_to_review(dir.path(), &mut parent, &tickets)
            .expect("persist review transition");
        assert_eq!(parent.status, "review");
        let saved = studio_list_tickets(dir.path())
            .into_iter()
            .find(|candidate| candidate.id == parent.id)
            .expect("saved parent");
        assert_eq!(saved.status, "review");
        assert_eq!(ticket_agent_for_dispatch(&saved, &tickets), "studio_reviewer");
    }

    #[test]
    fn activity_check_fails_closed_when_task_store_is_unavailable() {
        let dir = tempfile::tempdir().expect("directory");
        let missing_store = dir.path().join("missing").join("tasks.db");
        assert!(project_has_active_task(&missing_store, "project-1"));
    }

    #[test]
    fn dispatches_todo_and_review_but_not_blocked_tickets() {
        let dir = tempfile::tempdir().expect("project root");
        let store_path = dir.path().join("tasks.db");
        let statuses = HashMap::new();
        let now = chrono::Utc::now();
        assert!(ticket_can_be_retried(
            &ticket("todo", None),
            dir.path(),
            &statuses,
            &store_path,
            now
        ));
        assert!(ticket_can_be_retried(
            &ticket("review", None),
            dir.path(),
            &statuses,
            &store_path,
            now
        ));
        assert!(!ticket_can_be_retried(
            &ticket("blocked", None),
            dir.path(),
            &statuses,
            &store_path,
            now
        ));
    }

    #[test]
    fn terminal_failures_block_the_ticket_and_automatic_retries_are_bounded() {
        let dir = tempfile::tempdir().expect("project root");
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).expect("create project root");
        let task_store_path = dir.path().join("tasks.db");
        let _store = TaskStore::open(&task_store_path).expect("create task store");
        let failed_task = "00000000-0000-0000-0000-000000000001";
        let mut blocked = ticket("in_progress", Some(failed_task));
        blocked.id = "failed-ticket".to_string();
        api_studio::studio_upsert_ticket(&root, blocked.clone()).expect("save ticket");
        let mut statuses = HashMap::new();
        statuses.insert(failed_task.to_string(), TaskStatus::Failed);

        record_terminal_task_failure(&root, &blocked, &statuses, &task_store_path);
        let blocked = studio_list_tickets(&root)
            .into_iter()
            .find(|candidate| candidate.id == "failed-ticket")
            .expect("reconciled ticket");
        assert_eq!(blocked.status, "blocked");
        assert!(blocked
            .evidence
            .summary
            .as_deref()
            .unwrap_or_default()
            .contains("failed"));
        assert!(has_automatic_failure_for_current_task(&blocked, &root));

        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        assert!(ticket_can_be_retried(
            &blocked,
            &root,
            &statuses,
            &task_store_path,
            future
        ));
        for task_id in [
            "00000000-0000-0000-0000-000000000002",
            "00000000-0000-0000-0000-000000000003",
        ] {
            api_studio::studio_append_ticket_event(
                &root,
                &blocked.id,
                "ticket_execution_failed",
                "kanban_scheduler",
                Some(serde_json::json!({"task_id": task_id, "detail": "failed"})),
            )
            .expect("record later failed retry");
        }
        assert_eq!(automatic_failure_attempt_count(&blocked, &root), 3);
        assert!(!ticket_can_be_retried(
            &blocked,
            &root,
            &statuses,
            &task_store_path,
            future
        ));
    }

    #[test]
    fn reconciliation_works_without_scheduler_and_does_not_block_a_newer_assignment() {
        let dir=tempfile::tempdir().unwrap();let root=dir.path().join("project");std::fs::create_dir_all(&root).unwrap();
        let db=dir.path().join("tasks.db");let store=TaskStore::open(&db).unwrap();let id=uuid::Uuid::new_v4();let now=chrono::Utc::now();
        store.insert(&akasha_store::Task{id,parent_task_id:None,status:TaskStatus::Failed,assigned_agent:"studio_project_manager".into(),created_at:now,updated_at:now,initial_message:None,studio_project_id:Some("project".into())}).unwrap();
        let mut failed=ticket("in_progress",Some(&id.to_string()));failed.id="execution".into();api_studio::studio_upsert_ticket(&root,failed).unwrap();
        reconcile_project_tickets(&root,&db);reconcile_project_tickets(&root,&db);
        let mut blocked=api_studio::studio_get_ticket(&root,"execution").unwrap();assert_eq!(blocked.status,"blocked");
        assert_eq!(studio_list_ticket_events(&root,"execution").iter().filter(|event|event.event_type=="ticket_execution_failed").count(),1);
        blocked.related_task_id=Some(uuid::Uuid::new_v4().to_string());blocked.status="in_progress".into();api_studio::studio_upsert_ticket(&root,blocked).unwrap();
        reconcile_project_tickets(&root,&db);assert_eq!(api_studio::studio_get_ticket(&root,"execution").unwrap().status,"in_progress");
    }
    #[test]
    fn failed_reviewer_attempt_is_blocked_but_resumes_the_review_role() {
        let dir = tempfile::tempdir().expect("project root");
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).expect("create project root");
        let store_path = dir.path().join("tasks.db");
        let _store = TaskStore::open(&store_path).expect("create task store");
        let failed_task = "00000000-0000-0000-0000-000000000004";
        let mut review = ticket("review", Some(failed_task));
        review.id = "review-retry".to_string();
        review.requested_by = "studio_fullstack".to_string();
        api_studio::studio_upsert_ticket(&root, review.clone()).expect("save review ticket");
        let statuses = HashMap::from([(failed_task.to_string(), TaskStatus::Failed)]);

        record_terminal_task_failure(&root, &review, &statuses, &store_path);
        let saved = studio_list_tickets(&root)
            .into_iter()
            .find(|candidate| candidate.id == review.id)
            .expect("review ticket remains available");
        assert_eq!(saved.status, "blocked");
        assert_eq!(saved.evidence.resume_status.as_deref(),Some("review"));
        assert_eq!(ticket_agent_for_dispatch(&saved, &[]), "studio_reviewer");
        assert!(ticket_can_be_retried(
            &saved,
            &root,
            &statuses,
            &store_path,
            chrono::Utc::now() + chrono::Duration::hours(1)
        ));
    }

    #[test]
    fn local_context_preflight_does_not_repeat_the_same_request() {
        for (reason, expected_now, expected_after_cooldown) in [
            (
                "context_window_preflight_rejected: nothing was sent",
                false,
                false,
            ),
            ("provider context_window_exceeded: request rejected", false, true),
            ("studio_routes_exhausted: provider_generation_failed: invalid native tools", false, false),
            ("studio_no_verified_progress: repeated edits", false, false),
            ("studio_command_verification_failed: npm build failed", false, false),
            ("studio_round_budget_exhausted: no fresh milestone", false, false),
        ] {
            let dir = tempfile::tempdir().expect("project root");
            let root = dir.path().join("project");
            std::fs::create_dir_all(&root).expect("create project root");
            let store_path = dir.path().join("tasks.db");
            let store = TaskStore::open(&store_path).expect("create task store");
            let task_id = uuid::Uuid::new_v4();
            store
                .insert(&akasha_store::Task {
                    id: task_id,
                    parent_task_id: None,
                    status: TaskStatus::Failed,
                    assigned_agent: "studio_project_manager".to_string(),
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                    initial_message: None,
                    studio_project_id: Some("project-1".to_string()),
                })
                .expect("insert failed planning task");
            store
                .finalize_outcome(
                    task_id,
                    TaskStatus::Failed,
                    serde_json::json!({
                        "termination_reason": "provider_error",
                        "reason": reason,
                    }),
                )
                .expect("save failure outcome");

            let mut failed = ticket("in_progress", Some(&task_id.to_string()));
            failed.id = "initial-request".to_string();
            failed.initial_backlog_root = true;
            failed.acceptance_criteria[0].id = Some("initial-kanban-backlog".to_string());
            api_studio::studio_upsert_ticket(&root, failed.clone()).expect("save request ticket");
            let statuses = HashMap::from([(task_id.to_string(), TaskStatus::Failed)]);
            record_terminal_task_failure(&root, &failed, &statuses, &store_path);
            api_studio::studio_append_ticket_event(
                &root,
                &failed.id,
                "ticket_dispatch_scheduled",
                "kanban_scheduler",
                Some(serde_json::json!({"task_id": task_id.to_string()})),
            )
            .expect("record recent dispatch");

            let blocked = studio_list_tickets(&root)
                .into_iter()
                .find(|candidate| candidate.id == failed.id)
                .expect("read blocked request ticket");
            assert_eq!(
                ticket_can_be_retried(
                    &blocked,
                    &root,
                    &statuses,
                    &store_path,
                    chrono::Utc::now()
                ),
                expected_now,
                "unexpected immediate retry decision for {reason}"
            );
            assert_eq!(
                ticket_can_be_retried(
                    &blocked,
                    &root,
                    &statuses,
                    &store_path,
                    chrono::Utc::now() + chrono::Duration::hours(1)
                ),
                expected_after_cooldown,
                "unexpected cooldown retry decision for {reason}"
            );
        }
    }

    #[test]
    fn retries_stale_in_progress_ticket_only_after_its_task_finishes() {
        let dir = tempfile::tempdir().expect("project root");
        let task_id = "00000000-0000-0000-0000-000000000001";
        let mut statuses = HashMap::new();
        statuses.insert(task_id.to_string(), TaskStatus::Running);
        let current = ticket("in_progress", Some(task_id));
        assert!(!ticket_can_be_retried(
            &current,
            dir.path(),
            &statuses,
            &dir.path().join("tasks.db"),
            chrono::Utc::now()
        ));
        statuses.insert(task_id.to_string(), TaskStatus::Failed);
        assert!(ticket_can_be_retried(
            &current,
            dir.path(),
            &statuses,
            &dir.path().join("tasks.db"),
            chrono::Utc::now()
        ));
    }

    #[test]
    fn retry_cooldown_is_respected_after_a_failed_dispatch() {
        let now = chrono::Utc::now();
        let last = now.fixed_offset();
        assert!(!retry_cooldown_elapsed(Some(last), now, 60));
        assert!(retry_cooldown_elapsed(
            Some(last - chrono::Duration::seconds(61)),
            now,
            60
        ));
        assert!(retry_cooldown_elapsed(None, now, 60));
    }

    #[test]
    fn failed_attempts_back_off_and_fresh_work_is_prioritized() {
        assert_eq!(retry_cooldown_secs(300, 0), 300);
        assert_eq!(retry_cooldown_secs(300, 1), 300);
        assert_eq!(retry_cooldown_secs(300, 2), 600);
        assert_eq!(retry_cooldown_secs(300, 3), 1200);
        assert_eq!(retry_cooldown_secs(300, 8), 3600);

        let todo = ticket("todo", None);
        let failed = ticket("in_progress", Some("failed-task"));
        let mut statuses = HashMap::new();
        statuses.insert("failed-task".to_string(), TaskStatus::Failed);
        assert!(
            ticket_dispatch_priority(&todo, &statuses)
                < ticket_dispatch_priority(&failed, &statuses)
        );

        let dir = tempfile::tempdir().expect("project root");
        let task_id = "00000000-0000-0000-0000-000000000009";
        let mut failed = ticket("in_progress", Some(task_id));
        failed.id = "retry-ticket".to_string();
        api_studio::studio_append_ticket_event(
            dir.path(),
            &failed.id,
            "ticket_dispatch_scheduled",
            "kanban_scheduler",
            Some(serde_json::json!({"task_id": task_id})),
        )
        .expect("record dispatch");
        assert_eq!(failed_dispatch_count(&failed, dir.path(), &statuses), 0);
        statuses.insert(task_id.to_string(), TaskStatus::Failed);
        assert_eq!(failed_dispatch_count(&failed, dir.path(), &statuses), 1);
    }

    #[test]
    fn scheduled_retry_includes_the_previous_task_failure_context() {
        let dir = tempfile::tempdir().expect("task store directory");
        let store_path = dir.path().join("tasks.db");
        let store = TaskStore::open(&store_path).expect("open task store");
        let task_id = uuid::Uuid::new_v4();
        store
            .insert(&akasha_store::Task {
                id: task_id,
                parent_task_id: None,
                status: TaskStatus::Failed,
                assigned_agent: "studio_fullstack".to_string(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                initial_message: None,
                studio_project_id: Some("project-1".to_string()),
            })
            .expect("insert failed task");
        store
            .finalize_outcome(
                task_id,
                TaskStatus::Failed,
                serde_json::json!({
                    "status": "failed",
                    "termination_reason": "tool_loop",
                    "reason": "The same read call repeated."
                }),
            )
            .expect("save failure outcome");

        let current = ticket("in_progress", Some(&task_id.to_string()));
        let mut statuses = HashMap::new();
        statuses.insert(task_id.to_string(), TaskStatus::Failed);
        let context = terminal_failure_context(&current, &statuses, &store_path)
            .expect("failure detail should be available to the retry");
        assert!(context.contains("tool_loop"));
        assert!(context.contains("The same read call repeated."));
    }
}
