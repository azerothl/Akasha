#!/usr/bin/env python3
"""AR bench protocol runner for Akasha v0.11 (CPU-safe scaffolding).

Modes:
  --mock / --dry-run  Validate protocol JSON, emit mock results (no GPU / no daemon).
  --live              Hit a running daemon (requires GGUF + optional CUDA).

Examples:
  python3 spec/dev/quality/bench_ar_protocol.py --mock
  python3 spec/dev/quality/bench_ar_protocol.py --mock --json > /tmp/ar_mock.json
  PORT=3876 python3 spec/dev/quality/bench_ar_protocol.py --live --candidate qwen2.5-1.5b-instruct-q4
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
DEFAULT_PROTOCOL = Path(__file__).resolve().parent / "bench_ar_protocol.json"


def load_protocol(path: Path) -> dict:
    data = json.loads(path.read_text(encoding="utf-8"))
    assert data.get("schema_version") == 1, "schema_version must be 1"
    assert data.get("id") == "ar_fr_v1", "unexpected protocol id"
    prompts = data.get("prompts") or []
    assert len(prompts) == 5, f"expected 5 prompts, got {len(prompts)}"
    for p in prompts:
        assert p.get("id") and p.get("text"), f"prompt missing id/text: {p}"
    candidates = data.get("candidates") or []
    assert candidates, "candidates list empty"
    return data


def mock_run(protocol: dict, candidate_filter: str | None) -> dict:
    results = []
    for cand in protocol["candidates"]:
        if candidate_filter and cand["id"] != candidate_filter:
            continue
        if cand.get("skip_default_matrix") and not candidate_filter:
            continue
        for prompt in protocol["prompts"]:
            for ngl in protocol.get("ngl_matrix") or [99]:
                results.append(
                    {
                        "mode": "mock",
                        "candidate_id": cand["id"],
                        "arch": cand.get("arch"),
                        "tier": cand.get("tier"),
                        "prompt_id": prompt["id"],
                        "n_gpu_layers": ngl,
                        "ttft_s": 0.12,
                        "tok_per_s": 12.0 if ngl else 4.0,
                        "load_s": 1.5,
                        "ok": True,
                        "note": "synthetic — no inference",
                    }
                )
    return {
        "schema_version": 1,
        "protocol_id": protocol["id"],
        "mode": "mock",
        "decision_gate": protocol.get("decision_gate"),
        "watch_tier3": protocol.get("watch_tier3"),
        "results": results,
        "totals": {"rows": len(results), "ok": len(results)},
    }


def http_json(method: str, url: str, body: dict | None = None, timeout: float = 120.0) -> dict:
    data = None
    headers = {"Accept": "application/json"}
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8"))


def live_run(
    protocol: dict,
    base: str,
    candidate_filter: str | None,
    max_tokens: int,
) -> dict:
    caps = http_json("GET", f"{base}/api/capabilities")
    status = http_json("GET", f"{base}/api/router/embedded-status")
    if not status.get("embedded_available") and not status.get("ready_for_chat"):
        raise SystemExit(
            "daemon embedded LLM not ready — run with --mock or download a GGUF first"
        )

    results = []
    errors = 0
    for cand in protocol["candidates"]:
        if candidate_filter and cand["id"] != candidate_filter:
            continue
        if cand.get("skip_default_matrix") and not candidate_filter:
            continue
        # Live mode runs ngl from env if set, else a single pass (no daemon restart).
        ngl = int(os.environ.get("AKASHA_EMBEDDED_N_GPU_LAYERS", "99"))
        for prompt in protocol["prompts"]:
            row = {
                "mode": "live",
                "candidate_id": cand["id"],
                "arch": cand.get("arch"),
                "tier": cand.get("tier"),
                "prompt_id": prompt["id"],
                "n_gpu_layers": ngl,
                "ok": False,
            }
            t0 = time.time()
            try:
                task = http_json(
                    "POST",
                    f"{base}/api/message",
                    {
                        "message": prompt["text"],
                        "provider": protocol.get("provider", "akasha_embedded"),
                        "max_tokens": max_tokens,
                    },
                )
                task_id = task.get("task_id")
                if not task_id:
                    raise RuntimeError(f"no task_id: {task}")
                first_progress = None
                while True:
                    time.sleep(0.25)
                    t = http_json("GET", f"{base}/api/tasks/{task_id}")
                    st = t.get("status")
                    progress = t.get("progress") or []
                    if progress and first_progress is None:
                        first_progress = time.time() - t0
                    if st in ("done", "completed"):
                        dur = time.time() - t0
                        reply = t.get("result") or t.get("reply") or ""
                        text = reply if isinstance(reply, str) else str(reply)
                        approx_tokens = max(1, len(text.split()))
                        gen = max(0.001, dur - (first_progress or 0.0))
                        row.update(
                            {
                                "ok": True,
                                "ttft_s": round(first_progress, 2) if first_progress else None,
                                "load_s": None,
                                "tok_per_s": round(approx_tokens / gen, 1),
                                "duration_s": round(dur, 2),
                                "approx_tokens": approx_tokens,
                            }
                        )
                        break
                    if st == "failed":
                        raise RuntimeError(t.get("error") or "task failed")
                    if time.time() - t0 > 600:
                        raise RuntimeError("timeout 600s")
            except Exception as e:  # noqa: BLE001 — collect per-row errors
                errors += 1
                row["error"] = str(e)
            results.append(row)

    return {
        "schema_version": 1,
        "protocol_id": protocol["id"],
        "mode": "live",
        "capabilities_schema_version": caps.get("schema_version"),
        "runtime_gguf": (caps.get("runtime") or {}).get("gguf"),
        "decision_gate": protocol.get("decision_gate"),
        "results": results,
        "totals": {
            "rows": len(results),
            "ok": sum(1 for r in results if r.get("ok")),
            "errors": errors,
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--mock", "--dry-run", dest="mock", action="store_true")
    mode.add_argument("--live", action="store_true")
    parser.add_argument(
        "--protocol",
        type=Path,
        default=DEFAULT_PROTOCOL,
        help="Path to bench_ar_protocol.json",
    )
    parser.add_argument("--candidate", help="Only run this candidate id")
    parser.add_argument("--json", action="store_true", help="Print JSON only")
    parser.add_argument(
        "--port",
        type=int,
        default=int(os.environ.get("PORT", os.environ.get("AKASHA_PORT", "3876"))),
    )
    parser.add_argument("--max-tokens", type=int, default=0)
    args = parser.parse_args()

    protocol = load_protocol(args.protocol)
    max_tokens = args.max_tokens or int(protocol.get("max_tokens") or 128)

    if args.mock:
        out = mock_run(protocol, args.candidate)
    else:
        base = f"http://127.0.0.1:{args.port}"
        try:
            out = live_run(protocol, base, args.candidate, max_tokens)
        except urllib.error.URLError as e:
            print(f"live mode failed contacting {base}: {e}", file=sys.stderr)
            print("hint: use --mock without a daemon / GPU", file=sys.stderr)
            return 2

    if args.json:
        print(json.dumps(out, ensure_ascii=False, indent=2))
    else:
        print(f"protocol={out['protocol_id']} mode={out['mode']} rows={out['totals']['rows']}")
        for r in out["results"][:8]:
            print(
                f"  {r.get('candidate_id')} {r.get('prompt_id')} ngl={r.get('n_gpu_layers')} "
                f"ok={r.get('ok')} tok/s={r.get('tok_per_s')}"
            )
        if len(out["results"]) > 8:
            print(f"  … {len(out['results']) - 8} more rows")
        print("decision_gate:", json.dumps(out.get("decision_gate"), ensure_ascii=False))
    return 0 if out["totals"].get("ok", 0) > 0 or out["mode"] == "mock" else 1


if __name__ == "__main__":
    sys.exit(main())
