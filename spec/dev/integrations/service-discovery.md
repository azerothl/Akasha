# Service discovery (agnostic)

Akasha discovers local and LAN services through a **shared engine** in `akasha-core` and **declarative profiles** (Ollama, Home Assistant, …).

## API

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/discovery` | List built-in profiles (`id`, `display_name`, `port`, `install_url`) |
| GET | `/api/discovery/:service_id` | Run discovery for one profile → `{ service_id, instances[], install_url }` |

Each `instance` has `base_url`, `scope` (`local` | `network`), optional `metadata`.

## CLI

```bash
akasha discover                  # list profiles
akasha discover ollama           # scan for Ollama
akasha discover homeassistant    # scan for Home Assistant
akasha router discover           # alias → ollama (backward compatible)
```

## Environment

| Variable | Effect |
|----------|--------|
| `AKASHA_DISCOVERY_NETWORK=0` | Skip /24 network scan (local URLs only) |
| `AKASHA_DISCOVERY_TIMEOUT_MS` | Probe timeout (default 800) |

## Built-in profiles

Defined in `crates/akasha-core/src/service_discovery/profiles.rs`.

| id | Port | Probe |
|----|------|-------|
| `ollama` | 11434 | `GET /api/tags` → 200 |
| `homeassistant` | 8123 | `GET /api/` → 200, body contains `API running` |

## Adding a profile

1. Add a `ServiceProfile` constant in `profiles.rs` and register it in `PROFILES`.
2. Wire connector / CLI / doctor as needed (no engine changes).
3. Document `install_url` for empty discovery UX.

Future: `{data_dir}/discovery_profiles.yaml` for user-defined profiles.

## Code layout

```
crates/akasha-core/src/service_discovery/
  mod.rs       — public API
  types.rs     — DiscoveryEntry, ServiceProfile, …
  network.rs   — subnet scan
  probe.rs     — HTTP GET probe
  engine.rs    — discover_local / discover_network / discover
  profiles.rs  — built-in registry
```

`akasha-llm/src/discovery.rs` wraps the engine for Ollama backward compatibility.
