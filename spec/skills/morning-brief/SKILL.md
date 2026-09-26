---
name: morning-brief
description: Short local morning briefing from memory, open tasks, and notes — no network. Use when the user asks for a morning brief, daily recap, or what to do today.
license: MIT
when_to_use: >
  User asks for a morning briefing, daily recap, “what should I do today”,
  or to catch up after time away. Not for web research or a new project plan.
tools:
  - memory.recall
  - tasks.list
  - notes.list
  - notes.search
  - goal.complete
metadata:
  version: "1.0.0"
  portable: "akasha-daemon+akasha-os"
  p9_pilot: true
---

# Morning brief

**Language:** English | [Français](SKILL.fr.md)

**P9 portable pilot** — same file for the Akasha daemon (`spec/skills/` / `data_dir/skills/`) and akasha-os Preview (`var/skills/`). Do **not** use the network.

Interop note: `spec/dev/integrations/akasha-os-sibling-skills-modules.md`.

## Runtime: akasha-os (Preview)

Use only the tools listed in front matter.

1. `memory.recall` for durable preferences (UI language, what “today” means). One or two narrow queries.
2. `tasks.list` — open items only. Do **not** create or complete tasks.
3. `notes.list`, or `notes.search` with a short query. If the notes module is missing, skip and say so.
4. Write the briefing: **at most 8 short lines**. Facts first. Empty lists stay empty — do not invent work.
5. Do **not** call `web.search`, `web.browse`, or `net.fetch`.
6. `goal.complete` with the briefing as the result.

## Runtime: Akasha daemon

Ignore Preview tool names in front matter (`tools:` is for OS). Prefer daemon tools:

1. `memory_search` (or equivalent memory recall) for preferences / recent facts — narrow queries.
2. Inspect open work via available schedule / task tools (`list_scheduled_tasks` when present) and active todos if exposed.
3. If Life layer morning brief is configured, you may mention it (`POST /api/life/morning-brief` / schedule `morning_brief`) but **this skill** remains a one-shot local digest — do not send Telegram unless the user explicitly asks to notify.
4. Write the briefing: **at most 8 short lines** (or ~400 words max if the user asks for the Life-layer style digest). Facts first; no invented agenda.
5. Do **not** use `web_fetch`, browser tools, or install new skills for this turn.
6. Optional: `memory_store` a one-line recap only if the user asked to remember the brief.

## Shared rules

- Local only: no network research.
- Empty data → say so; do not fabricate events or citations.
- Keep the answer skimmable.
