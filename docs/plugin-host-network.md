# Plugin host: network sandbox

WASM plugins run without WASI. Optional **HTTP** is provided through a single host import:

- **`akasha::http_fetch`** — `(req_ptr, req_len, out_ptr, out_cap) -> i32`  
  Request body (UTF-8 JSON): `{ "url": "...", "method": "GET"|"POST", "headers": {}, "body": "..." }`  
  Response written to `out_ptr` (UTF-8 JSON): success `{ "ok": true, "status": number, "body_b64": "..." }` or `{ "ok": false, "error": "..." }`.

## Manifest

```toml
permissions = ["network"]

[network]
allowed_url_prefixes = [ "https://example.com/api" ]
max_response_bytes = 2000000
timeout_ms = 25000
https_only = true
max_requests_per_run = 8
```

If `permissions` does not include `network`, the import is still linked but calls return an error JSON (`network permission not declared`). If `allowed_url_prefixes` is empty, all requests are denied until configured.

## Security notes

- URLs must match one of the prefixes (string prefix match after optional trim of trailing `/`).
- Only `GET` and `POST` are allowed.
- Response bodies are truncated to `max_response_bytes`.

No plugin-id-specific rules exist in the host: policy is **entirely** driven by the loaded manifest.
