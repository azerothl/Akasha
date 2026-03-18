# Agent reply contracts (soft validation)

Optional YAML files in this directory define checks on sub-agent final replies (orchestrated multi-step flows).

Fields:

- `agent_type` (required): matches `assigned_agent` on the child task.
- `min_response_chars`: minimum character count (after trim).
- `response_must_contain`: list of substrings that must appear in the reply.
- `ignore_case`: if true, substring checks use case-insensitive matching.

On violation, the daemon emits `contract_violation` on the root task timeline and still aggregates the reply.
