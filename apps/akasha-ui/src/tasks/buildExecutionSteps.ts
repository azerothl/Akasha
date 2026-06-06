export type TaskListItem = {
  id: string;
  status: string;
  label?: string;
  parent_task_id?: string;
  assigned_agent?: string;
};

export type TaskEvent = {
  event_type?: string;
  payload?: unknown;
};

export type TodoStep = {
  id?: string | null;
  title: string;
  status: string;
};

export type ExecutionStep = {
  key: string;
  stepId?: string;
  title: string;
  agent?: string;
  status: string;
  source: "plan" | "subtask" | "todo";
  childTaskId?: string;
  intent?: string;
};

export function buildStepChildMap(events: TaskEvent[]): Map<string, { childTaskId: string; agent?: string }> {
  const map = new Map<string, { childTaskId: string; agent?: string }>();
  for (const event of events) {
    if (event.event_type !== "sub_agent_spawned" || !event.payload || typeof event.payload !== "object") continue;
    const p = event.payload as Record<string, unknown>;
    const stepId = typeof p.step_id === "string" ? p.step_id.trim() : "";
    const childTaskId = typeof p.child_task_id === "string" ? p.child_task_id : "";
    if (!stepId || !childTaskId) continue;
    const agent = typeof p.agent === "string" ? p.agent : undefined;
    map.set(stepId, { childTaskId, agent });
  }
  return map;
}

function extractPlanSteps(events: TaskEvent[]): Array<{
  step_id?: string;
  agent_type?: string;
  intent_preview?: string;
  intent?: string;
}> {
  const planEv = events.find(
    (e) =>
      (e.event_type === "plan_proposed" || e.event_type === "plan_committed") &&
      e.payload &&
      typeof e.payload === "object" &&
      "steps" in e.payload
  );
  if (planEv?.payload && typeof planEv.payload === "object" && Array.isArray((planEv.payload as { steps?: unknown }).steps)) {
    return (planEv.payload as { steps: Array<{ step_id?: string; agent_type?: string; intent_preview?: string; intent?: string }> }).steps;
  }
  const decomposed = events.find(
    (e) => e.event_type === "task_decomposed" && e.payload && typeof e.payload === "object" && "agents" in e.payload
  );
  const agents =
    decomposed?.payload && typeof decomposed.payload === "object" && Array.isArray((decomposed.payload as { agents?: unknown }).agents)
      ? (decomposed.payload as { agents: string[] }).agents
      : [];
  return agents.map((agent_type, i) => ({ step_id: `s${i}`, agent_type, intent_preview: "" }));
}

function taskById(tasks: TaskListItem[], id: string): TaskListItem | undefined {
  return tasks.find((t) => t.id === id);
}

export function buildExecutionSteps(
  events: TaskEvent[],
  tasks: TaskListItem[],
  selectedTaskId: string | undefined,
  todos: TodoStep[]
): { steps: ExecutionStep[]; emptyReason: "none" | "direct" | "no_selection" } {
  if (!selectedTaskId) {
    return { steps: [], emptyReason: "no_selection" };
  }

  const childMap = buildStepChildMap(events);
  const planSteps = extractPlanSteps(events);
  const childTasks = tasks.filter((t) => t.parent_task_id === selectedTaskId);
  const steps: ExecutionStep[] = [];

  if (planSteps.length > 0) {
    for (const s of planSteps) {
      const stepId = s.step_id ?? "";
      const link = stepId ? childMap.get(stepId) : undefined;
      const child = link ? taskById(tasks, link.childTaskId) : undefined;
      const title = (s.intent_preview || s.intent || s.agent_type || stepId || "").trim() || stepId || "—";
      steps.push({
        key: `plan-${stepId || steps.length}`,
        stepId: stepId || undefined,
        title,
        agent: s.agent_type || link?.agent || child?.assigned_agent,
        status: child?.status ?? "pending",
        source: "plan",
        childTaskId: child?.id ?? link?.childTaskId,
        intent: s.intent_preview || s.intent,
      });
    }
  } else if (childTasks.length > 0) {
    for (const child of childTasks) {
      steps.push({
        key: `subtask-${child.id}`,
        title: child.label?.trim() || child.assigned_agent || child.id.slice(-8),
        agent: child.assigned_agent,
        status: child.status,
        source: "subtask",
        childTaskId: child.id,
      });
    }
  }

  for (const todo of todos) {
    const existing = steps.find((s) => s.title === todo.title && s.source !== "todo");
    if (existing) continue;
    steps.push({
      key: `todo-${todo.id ?? todo.title}-${steps.length}`,
      title: todo.title,
      status: todo.status,
      source: "todo",
    });
  }

  if (steps.length === 0) {
    return { steps: [], emptyReason: "direct" };
  }
  return { steps, emptyReason: "none" };
}

export function executionProgress(steps: ExecutionStep[]): { total: number; done: number; progressPct: number } {
  const trackable = steps.filter((s) => s.source !== "todo" || s.status === "done" || s.status === "cancelled");
  const total = trackable.length || steps.length;
  const done = (trackable.length ? trackable : steps).filter((s) => s.status === "done" || s.status === "completed").length;
  const progressPct = total > 0 ? Math.round((done / total) * 100) : 0;
  return { total, done, progressPct };
}
