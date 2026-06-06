# Orchestrator degraded paths (S-ORCH-01)

When the Code Studio / swarm orchestrator reaches final aggregation but planned deliverable files were never written by sub-agents, Akasha applies a **filesystem fallback** before reporting failure.

## `stub_body` fallback

Implementation: `write_missing_deliverables_fs_fallback` in `crates/akasha-daemon/src/agents/orchestrator.rs`.

### Trigger

After remediation passes, if deliverables listed in the project plan are still missing on disk (and are not binary formats), the orchestrator writes minimal UTF-8 placeholders so the user can edit or replace them.

### Skipped paths

- Absolute paths outside the workspace
- Unsafe relative paths
- Binary deliverables (`.pdf`, `.pptx`) — no text stub is written; the missing artifact remains visible to the user
- Directory deliverables — creates the directory plus a `.akasha_deliverable_dir` marker

### Stub formats

| Extension | Content |
|-----------|---------|
| `.ipynb` | Minimalnbformat 4 notebook with one markdown cell |
| `.json` | Placeholder object with `_akasha_auto_deliverable` metadata |
| `.py` | Comment + `NotImplementedError` |
| `.yaml` / `.yml` | Comment header + `placeholder: true` |
| `.md` / default | Markdown heading explaining auto-creation |

Each stub includes the root task UUID for traceability.

### Operator remediation

1. Inspect task events / studio diff for the root task.
2. Replace stub files with real content or re-run the delegated step with clearer acceptance criteria.
3. If stubs appear frequently, tighten planner output or enable stricter ticket enforcement in Code Studio.

### Related specs

- `docs/CODE_STUDIO_SPEC.md` (acceptance criteria)
- `spec/dev/roadmap/ROADMAP_FINAL_REGISTRY.md` — S-ORCH-01
