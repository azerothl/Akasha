# RFC - jcode-inspired integration for Akasha

Status: Draft  
Owner: Akasha core team  
Scope: `akasha-daemon`, `akasha-ui`, `akasha-tui`, `akasha-code-studio`, `Akasha_app`

## 1. Goals

This RFC defines an incremental integration path for high-value concepts inspired by jcode:

- Unified permission review queue across CLI/TUI/UI and HTTP API.
- Session transcript and operator summary after each relevant cycle.
- Opportunistic post-retrieval memory maintenance without adding user-visible latency.
- Code Studio swarm MVP with explicit coordinator and worker states.

Non-goals:

- Full runtime parity with jcode server internals.
- Replacing Akasha orchestration model.
- Shipping multi-agent autonomous swarms outside Code Studio MVP scope.

## 2. Phase map

### Phase A - Safety Queue + Session Reports contracts

Deliverables:

- Queue model and API contract for permission requests.
- Canonical transcript JSON schema.
- Operator summary payload and rendering requirements.

### Phase B - Permission review MVP (daemon + UI)

Deliverables:

- Pending/approved/denied/expired lifecycle.
- CLI/TUI/UI review actions.
- Audit trail events and decision source tracking.

### Phase C - Memory post-retrieval maintenance

Deliverables:

- Async maintenance worker attached to retrieval flow.
- Boost/decay/link tasks with bounded budget.
- Quality metrics exposure.

### Phase D - Code Studio swarm MVP

Deliverables:

- One coordinator (`studio_project_manager`) + bounded workers.
- Task graph + worker state stream.
- Cockpit visualization and conflict awareness.

### Phase E - Product communication updates

Deliverables:

- Site updates in `Akasha_app` for new operator-facing capabilities.
- Documentation updates for API and workflow onboarding.

## 3. Unified Safety Queue design

### 3.1 Data model

```json
{
  "id": "perm_req_...",
  "task_id": "task_...",
  "session_id": "session_...",
  "tool_id": "run_command",
  "scope": "project:studio/<uuid>",
  "action": "run_command",
  "description": "Run npm install in project root",
  "rationale": "Dependencies missing before build",
  "urgency": "low",
  "status": "pending",
  "created_at": "ISO-8601",
  "updated_at": "ISO-8601",
  "expires_at": "ISO-8601",
  "decision": null,
  "decided_by": null,
  "decided_via": null,
  "decision_note": null
}
```

Allowed statuses: `pending | approved | denied | expired`.

### 3.2 API

New endpoints (daemon):

- `GET /api/permissions/queue?status=pending|approved|denied|expired&limit=&cursor=`
- `GET /api/permissions/queue/:id`
- `POST /api/permissions/queue/:id/approve` body optional `{ "note": "..." }`
- `POST /api/permissions/queue/:id/deny` body optional `{ "note": "..." }`
- `POST /api/permissions/queue/:id/expire` (internal/operator usage)

Compatibility:

- Keep `GET/POST /api/permissions/decisions` and `DELETE /api/permissions/decisions/:id`.
- Queue decisions can optionally emit/refresh persistent decision entries when the user chooses "always allow/deny".

### 3.3 UI/CLI/TUI integration

- TUI: pending badge + review pane actions.
- Tauri/UI: queue table with task context, scope, rationale, and decision controls.
- CLI:
  - `akasha permissions queue list [--status pending]`
  - `akasha permissions queue approve <id> [--note ...]`
  - `akasha permissions queue deny <id> [--note ...]`

## 4. Session reporting contract

### 4.1 Transcript JSON (`transcripts/<timestamp>.json`)

```json
{
  "session_id": "session_...",
  "task_id": "task_...",
  "started_at": "ISO-8601",
  "ended_at": "ISO-8601",
  "assigned_agent": "studio_project_manager",
  "token_usage": {
    "input": 0,
    "output": 0,
    "total": 0
  },
  "actions": [
    {
      "type": "tool_call",
      "tool_id": "run_command",
      "summary": "Ran npm run build",
      "status": "ok",
      "timestamp": "ISO-8601"
    }
  ],
  "permission_requests": [
    {
      "request_id": "perm_req_...",
      "status": "pending"
    }
  ],
  "suggested_actions": [
    {
      "id": "open_preview",
      "label": "Open preview",
      "kind": "ui",
      "ui_action": "open_preview"
    }
  ]
}
```

### 4.2 Operator summary

A compact summary is generated for:

- Task completion events.
- Long-running autonomous cycles.
- Code Studio run completion.

Minimum fields:

- `done[]`, `needs_review[]`, `failed[]`, `next_steps[]`.

## 5. Memory post-retrieval maintenance (Phase C)

### 5.1 Trigger

After long-term memory retrieval returns results to the user-facing agent pipeline.

### 5.2 Async tasks

- Confidence boost for memories validated by downstream usage.
- Confidence decay for consistently retrieved-but-unused memories.
- Co-occurrence link reinforcement (`relates_to`-like edge metadata).
- Gap marker creation when retrieval quality is low.

### 5.3 Budget and safety

- Hard cap per turn: max processed memories + max maintenance time.
- Backoff under high daemon load.
- Never block response path.

### 5.4 Metrics

- `memory_retrieval_candidates_total`
- `memory_retrieval_used_total`
- `memory_confidence_boost_total`
- `memory_confidence_decay_total`
- `memory_gap_markers_total`
- `memory_retrieval_usefulness_ratio` (derived)

## 6. Code Studio swarm MVP (Phase D)

### 6.1 Roles

- `studio_project_manager`: coordinator only.
- `studio_worker_*`: bounded workers by specialization.

### 6.2 Worker states

- `spawned`
- `ready`
- `running`
- `blocked`
- `completed`
- `failed`
- `stopped`

### 6.3 Events

Expose structured events through `GET /api/tasks/:id/events` payloads:

- `studio_worker_state_changed`
- `studio_subtask_assigned`
- `studio_subtask_completed`
- `studio_subtask_failed`
- `studio_conflict_notice`

### 6.4 Constraints

- Respect `AKASHA_STUDIO_MAX_PARALLEL_OPS`.
- Swarm mode opt-in via task context (default off).
- Keep all writes inside project disk root registry.

## 7. Risks and mitigations

- Permission fatigue -> add urgency + batching + persistent decisions.
- Event noise -> bounded payloads and summarized views.
- Memory drift -> confidence decay and explicit reset controls.
- Parallelism conflicts -> conflict events and coordinator fallback to sequential mode.

## 8. Acceptance criteria

- Queue lifecycle observable end-to-end in API + at least one UI.
- Transcript file created for completed tasks/cycles with valid schema.
- Memory maintenance metrics available through existing metrics surface.
- Code Studio can run at least 2 parallel workers with visible state transitions.
