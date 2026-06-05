import { describe, it, expect } from "vitest";
import {
  collapseStreamedProgressEvents,
  isTaskActiveStatus,
  isTaskTerminalStatus,
  mergeTaskEvents,
  normalizeTaskStatus,
} from "./taskEvents";

describe("collapseStreamedProgressEvents", () => {
  it("keeps only the latest streamed message per task and percent", () => {
    const events = [
      {
        event_type: "progress_update",
        task_id: "task-a",
        payload: { task_id: "task-a", progress_pct: 50, message: "Je corrige le build" },
      },
      {
        event_type: "progress_update",
        task_id: "task-a",
        payload: { task_id: "task-a", progress_pct: 50, message: "Je corrige le build en créant les fichiers manquants" },
      },
      {
        event_type: "progress_update",
        task_id: "task-a",
        payload: {
          task_id: "task-a",
          progress_pct: 50,
          message: "Je corrige le build en créant les fichiers manquants et en alignant les conventions de nommage.",
        },
      },
      {
        event_type: "progress_update",
        task_id: "task-a",
        payload: { task_id: "task-a", progress_pct: 80, message: "Je vérifie ensuite les imports et les chemins." },
      },
    ];

    const compact = collapseStreamedProgressEvents(events, "root");

    expect(compact).toHaveLength(2);
    expect((compact[0]?.payload as { message: string }).message).toContain("conventions de nommage");
    expect((compact[1]?.payload as { message: string }).message).toContain("imports et les chemins");
  });

  it("does not merge different tasks with same percent", () => {
    const events = [
      {
        event_type: "progress_update",
        task_id: "task-a",
        payload: { task_id: "task-a", progress_pct: 50, message: "A-50" },
      },
      {
        event_type: "progress_update",
        task_id: "task-b",
        payload: { task_id: "task-b", progress_pct: 50, message: "B-50" },
      },
    ];
    const compact = collapseStreamedProgressEvents(events, "root");
    expect(compact).toHaveLength(2);
  });
});

describe("task status helpers", () => {
  it("normalizes status casing", () => {
    expect(normalizeTaskStatus("Completed")).toBe("completed");
    expect(isTaskTerminalStatus("Completed")).toBe(true);
    expect(isTaskActiveStatus("Running")).toBe(true);
    expect(isTaskActiveStatus("completed")).toBe(false);
  });
});

describe("mergeTaskEvents", () => {
  it("unions events without dropping previous rows", () => {
    const prev = [{ event_type: "tool_invoked", at: "t1", payload: { tool: "grep" } }];
    const incoming = [{ event_type: "task_completed", at: "t2", payload: {} }];
    const merged = mergeTaskEvents(prev, incoming);
    expect(merged).toHaveLength(2);
  });

  it("returns previous list when incoming is empty", () => {
    const prev = [{ event_type: "progress_update", at: "t1" }];
    expect(mergeTaskEvents(prev, [])).toEqual(prev);
  });
});
