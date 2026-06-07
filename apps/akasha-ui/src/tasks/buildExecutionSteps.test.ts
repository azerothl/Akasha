import { describe, expect, it } from "vitest";
import { buildExecutionSteps, buildStepChildMap, executionProgress } from "./buildExecutionSteps";

describe("buildStepChildMap", () => {
  it("maps step_id to child_task_id from sub_agent_spawned", () => {
    const map = buildStepChildMap([
      {
        event_type: "sub_agent_spawned",
        payload: { step_id: "s0", child_task_id: "abc-123", agent: "analyst" },
      },
    ]);
    expect(map.get("s0")).toEqual({ childTaskId: "abc-123", agent: "analyst" });
  });
});

describe("buildExecutionSteps", () => {
  const tasks = [
    { id: "root-1", status: "running", parent_task_id: undefined },
    { id: "child-1", status: "completed", parent_task_id: "root-1", assigned_agent: "coder", label: "Implement API" },
  ];

  it("links plan steps to child task status", () => {
    const { steps, emptyReason } = buildExecutionSteps(
      [
        {
          event_type: "plan_proposed",
          payload: {
            steps: [{ step_id: "s0", agent_type: "coder", intent_preview: "Build endpoint" }],
          },
        },
        {
          event_type: "sub_agent_spawned",
          payload: { step_id: "s0", child_task_id: "child-1", agent: "coder" },
        },
      ],
      tasks,
      "root-1",
      []
    );
    expect(emptyReason).toBe("none");
    expect(steps).toHaveLength(1);
    expect(steps[0].status).toBe("completed");
    expect(steps[0].childTaskId).toBe("child-1");
  });

  it("falls back to subtasks when no plan", () => {
    const { steps } = buildExecutionSteps([], tasks, "root-1", []);
    expect(steps).toHaveLength(1);
    expect(steps[0].source).toBe("subtask");
  });

  it("reports direct task empty reason", () => {
    const { emptyReason } = buildExecutionSteps([], [{ id: "solo", status: "completed" }], "solo", []);
    expect(emptyReason).toBe("direct");
  });
});

describe("executionProgress", () => {
  it("computes done ratio", () => {
    const p = executionProgress([
      { key: "1", title: "a", status: "completed", source: "plan" },
      { key: "2", title: "b", status: "pending", source: "plan" },
    ]);
    expect(p.done).toBe(1);
    expect(p.total).toBe(2);
    expect(p.progressPct).toBe(50);
  });
});
