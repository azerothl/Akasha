# AGENTS.md

## Cursor Cloud specific instructions

### Project overview

Akasha is a Rust workspace (17 crates) with a Tauri desktop UI (React/TypeScript/Vite in `apps/akasha-ui`). Documentation is in French. See `README.md` for the full structure.

### System dependencies (pre-installed)

- **Rust 1.70+** (update script handles `rustup update stable && rustup default stable`)
- **Node.js 18+** and npm (for `apps/akasha-ui`)
- **libssl-dev**, **pkg-config** (for `openssl-sys` crate)
- **libstdc++.so symlink** at `/usr/lib/x86_64-linux-gnu/libstdc++.so` (required by the `lld` linker for C++ deps like `esaxx-rs` and `onig_sys`)

### Build & test commands

- **Build:** `CXX=g++ cargo build` (set `CXX=g++` because the default clang `c++` can't find `<cstdint>` headers)
- **Test (Rust):** `CXX=g++ cargo test`
- **Lint:** `CXX=g++ cargo clippy`
- **Test (UI):** `cd apps/akasha-ui && npm run test`
- **Test (UI E2E / Playwright):** `cargo build -p akasha-daemon` then `cd apps/akasha-ui && npm run test:e2e:install && npm run test:e2e` (writes screenshots under `docs/screenshots/`)
- **Benchmarks:** `CXX=g++ cargo bench -p akasha-daemon`

See `spec/dev/quality/tests_and_benchmarks.md` for the full test inventory by crate. Refactor progress: `spec/dev/quality/REFACTOR_MONOREPO_TRACKING.md`.

### Release version alignment (GitHub)

Product version must match across the workspace Cargo.toml (`[workspace.package].version`) and the Tauri app (`apps/akasha-ui`: `package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, `tauri.conf.json`). Mismatch causes false “update available” banners versus `api/latest.json`.

**Recommended sequence (automated):**

1. Actions → **Sync release version** → set `version` (e.g. `0.8.0`, no `v` prefix) and `branch` (e.g. `main`). This runs `scripts/sync-release-version.py` and pushes one commit.
2. Pull locally, then create and push the tag: `git tag v0.8.0 && git push origin v0.8.0`.
3. The **Release** workflow builds and publishes artifacts. The `verify-release-version` job fails the tag build if the tag does not match the embedded versions (run step 1 first).

If `main` is branch-protected, add a repository secret `VERSION_SYNC_TOKEN` (PAT with `contents: write`) so the sync workflow can push.

Locally: `python3 scripts/sync-release-version.py X.Y.Z` from the repo root.

### Running the daemon

```bash
CXX=g++ cargo build
./target/debug/akasha start --foreground   # foreground with logs
./target/debug/akasha doctor               # diagnostics
```

The daemon listens on port **3876** (env `AKASHA_PORT`). Key API endpoints:
- `GET /` and `GET /api/status` — health check
- `POST /api/message` — send a message (returns `task_id`)
- `GET /api/tasks/:id` — check task status
- `GET /api/docs` — user guide

### Important gotchas

1. **`CXX=g++` is required** for `cargo build/test/clippy`. The default `c++` (clang 18) cannot find `<cstdint>` from `libstdc++-13-dev` in this environment.
2. **Embedded LLM backends**: default release builds use **Candle (Qwen3 0.6B, CPU)** and are slow without GPU. For fast local inference on NVIDIA GPUs, use the **`akasha-windows-x86_64-cuda`** release artifact (llama-cpp-4 + GGUF) or build with `--features embedded-llama-cpp,embedded-llama-cpp-cuda`, then run `akasha config models embedded-download`. Candle remains the CPU fallback when no GGUF is present (`AKASHA_EMBEDDED_BACKEND=auto`). For cloud/external speed, configure Ollama, OpenAI, or OpenRouter via `llm_router.yaml`.
3. **No external services required** for core functionality. SQLite is bundled (`rusqlite` with `bundled` feature), embeddings model is pre-bundled at `embedding_model/`, and the embedded LLM eliminates the hard dependency on Ollama.
4. **Data directory** defaults to `~/akasha`. Config files (`llm_router.yaml`, `tools_policy.yaml`, `akasha.env`, `connectors.env`) are stored there.
5. The Tauri desktop UI (`apps/akasha-ui`) requires the daemon to be running on port 3876. Run `npm install` then `npm run tauri dev` from that directory.

### Agent-ergonomic CLIs (AXI)

When the embedded agent runs shell commands (`run_command`), prefer **token-efficient, agent-oriented CLIs** where they help:

- **Principles and background:** [axi.md](https://axi.md/) — design goals (compact output, explicit totals, clear empty states).
- **Reference repo:** [github.com/kunchenguid/axi](https://github.com/kunchenguid/axi) — benchmarks, `gh-axi` (GitHub), `chrome-devtools-axi` (browser).
- **Install (global npm):** `npm install -g gh-axi` and/or `npm install -g chrome-devtools-axi` when you want the agent to drive GitHub or browser automation from the shell with less context overhead than raw `gh`/curl-only flows or very chatty snapshots.

Akasha still exposes built-in `browser navigate` / `browser snapshot` and vault-backed `run_command`; AXI tools are an optional complement.

### Optional: AXI skill for development

To align new tools or CLIs with the same principles while coding, contributors can install the upstream skill:

`npx skills add kunchenguid/axi`

This is for **developer workflows** (e.g. Cursor / Claude Code), not a runtime dependency of the daemon.
