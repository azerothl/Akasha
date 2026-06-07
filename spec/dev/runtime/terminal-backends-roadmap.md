# Terminal backends roadmap (Hermes parity)

## Today

- **One-shot shell:** `run_command`, `run_terminal`, `run_command_background` + `process list|poll|kill`.
- **Capabilities introspection:** `GET /api/terminal/capabilities` (JSON: current tools + **PTY HTTP** sous `/api/terminal/pty/sessions*`).
- **PTY tranche 1:** sessions via `portable-pty` (create / read / write / resize / close) — voir `spec/43_session_terminal.md`.

## Planned

| Backend | Use case | Notes |
|---------|----------|--------|
| **Local PTY (suite)** | REPL, TUI CLIs, pagers | Tranche 1 livrée (`/api/terminal/pty/sessions*`). Suite : persistance / transcript, outil agent `terminal_session`, session resume `spec/43_session_terminal.md`. |
| **Docker exec** | Toolchain containers | Align with `run_in_container` policies. |
| **SSH** | Remote dev / CI | Key management via vault; host allow-list. |
| **Managed sandboxes** | Hermes-style Daytona / Modal | Evaluate per deployment; not required for core OSS. |

## Prioritization

1. Local PTY + stable session IDs + resize + transcript persistence.  
2. SSH with strict host keys + jump host optional.  
3. Cloud backends only where product requires shared agent sandboxes.
