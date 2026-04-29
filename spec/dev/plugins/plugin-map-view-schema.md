# Plugin tool result: map view (`view: "map"`)

This contract is **agnostic of any specific plugin**. Any WASM or native tool may emit a JSON payload that the Akasha UI renders as a map (metrics, optional routes, Leaflet trace, optional OSM embed). The optional **maps** plugin in `Akasha_plugins` is one producer.

## Top-level fields (subset)

| Field | Type | Description |
|-------|------|-------------|
| `view` | string | Must be `"map"`. |
| `summary` | string | Human-readable summary. |
| `detail` | string (optional) | Extra context, limitations, attribution hints. |
| `geometry_kind` | string | `road_network` — polyline follows a road-routing engine; `great_circle_estimate` — geographic estimate between resolved points, not road geometry. |
| `geometry` | GeoJSON-like | Primary `LineString` or equivalent; UI also reads `routes[].geometry`. |
| `routes` | array (optional) | Named alternatives; each has `id`, `label`, `mode`, `geometry`, `distance_m`, `duration_s`, `steps`. |
| `steps` | array (optional) | Primary step list: `{ "instruction": string, "distance_m"?: number }`. |
| `bbox` | object (optional) | `min_lat`, `max_lat`, `min_lon`, `max_lon` for framing. |
| `osm_embed_url` / `osm_browse_url` | string (optional) | Optional OpenStreetMap embed / browse links. |
| `map_attribution` | string (optional) | Data attribution line. |

Coordinates in GeoJSON-style arrays use **`[longitude, latitude]`**. Internal UI points use `{ x: lon, y: lat }`.

## Host capability: `akasha::http_fetch` (plugins)

Plugins compiled for `wasm32-unknown-unknown` may declare an import:

- Module: `akasha`
- Function: `http_fetch(req_ptr, req_len, out_ptr, out_cap) -> i32`

The host implements this **only** when the plugin manifest grants `permissions = ["network"]` and a `[network]` section defines `allowed_url_prefixes`, limits, etc. Request/response format is JSON (see `akasha-plugin-host`). This keeps **no routing provider logic** inside Akasha beyond the sandboxed HTTP primitive.

## See also

- `crates/akasha-plugin-api/src/manifest.rs` — `PluginNetworkConfig`
- `crates/akasha-plugin-host/src/lib.rs` — linker and `http_fetch` implementation
