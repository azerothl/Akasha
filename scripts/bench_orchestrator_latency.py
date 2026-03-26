#!/usr/bin/env python3
import argparse
import json
import statistics
import time
import urllib.error
import urllib.request
from typing import Any, Dict, List, Optional

DEFAULT_SCENARIOS = {
    "simple": "Résume en une phrase ce qu'est Akasha.",
    "project": "Crée un plan de travail détaillé pour une API sécurisée, un notebook, du monitoring et des livrables dans workspace:/bench_demo/.",
    "tools": "Donne-moi la météo actuelle à Paris puis résume-la en 3 points.",
}


def http_json(url: str, method: str = "GET", payload: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
    data = None
    headers = {}
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    with urllib.request.urlopen(req, timeout=30) as resp:
        body = resp.read().decode("utf-8")
        return json.loads(body)


def percentile(values: List[float], q: float) -> float:
    if not values:
        return 0.0
    if len(values) == 1:
        return values[0]
    values = sorted(values)
    idx = (len(values) - 1) * q
    lo = int(idx)
    hi = min(lo + 1, len(values) - 1)
    frac = idx - lo
    return values[lo] * (1 - frac) + values[hi] * frac


def extract_timeline(events: List[Dict[str, Any]]) -> Dict[str, Any]:
    out: Dict[str, Any] = {}
    for event in events:
        if event.get("event_type") != "timeline_milestone":
            continue
        payload = event.get("payload") or {}
        name = payload.get("name")
        if not name:
            continue
        out[name] = payload
    return out


def run_once(base_url: str, message: str, timeout_s: int) -> Dict[str, Any]:
    task = http_json(
        f"{base_url}/api/message",
        method="POST",
        payload={"message": message, "new_session": True},
    )
    task_id = task["task_id"]
    start = time.time()
    status = {}
    events = []
    while time.time() - start < timeout_s:
        status = http_json(f"{base_url}/api/tasks/{task_id}")
        events_payload = http_json(f"{base_url}/api/tasks/{task_id}/events")
        events = events_payload.get("events", [])
        if status.get("status") in {"completed", "failed", "cancelled"}:
            break
        time.sleep(0.75)
    timeline = extract_timeline(events)
    return {
        "task_id": task_id,
        "status": status.get("status", "unknown"),
        "timeline": timeline,
        "progress_count": len(status.get("progress", [])),
    }


def summarize(results: List[Dict[str, Any]]) -> Dict[str, float]:
    ttfa = [float(r["timeline"].get("first_subagent_spawned", {}).get("elapsed_ms", 0)) for r in results if r["timeline"].get("first_subagent_spawned")]
    ttfr = [float(r["timeline"].get("first_meaningful_progress", {}).get("elapsed_ms", 0)) for r in results if r["timeline"].get("first_meaningful_progress")]
    decompose = [float(r["timeline"].get("decompose_end", {}).get("duration_ms", 0)) for r in results if r["timeline"].get("decompose_end")]
    return {
        "ttfa_p50_ms": percentile(ttfa, 0.50),
        "ttfa_p95_ms": percentile(ttfa, 0.95),
        "ttfa_p99_ms": percentile(ttfa, 0.99),
        "ttfr_p50_ms": percentile(ttfr, 0.50),
        "ttfr_p95_ms": percentile(ttfr, 0.95),
        "ttfr_p99_ms": percentile(ttfr, 0.99),
        "decompose_p50_ms": percentile(decompose, 0.50),
        "decompose_p95_ms": percentile(decompose, 0.95),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description="Benchmark Akasha orchestration latency milestones.")
    parser.add_argument("--base-url", default="http://127.0.0.1:3876", help="Akasha daemon base URL")
    parser.add_argument("--scenario", choices=list(DEFAULT_SCENARIOS.keys()) + ["custom"], default="simple")
    parser.add_argument("--message", default="", help="Custom message when --scenario custom")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args()

    message = args.message if args.scenario == "custom" else DEFAULT_SCENARIOS[args.scenario]
    if not message:
        raise SystemExit("Custom scenario requires --message")

    results = []
    for idx in range(args.runs):
        print(f"[{idx + 1}/{args.runs}] running scenario '{args.scenario}'...")
        try:
            result = run_once(args.base_url.rstrip("/"), message, args.timeout)
        except urllib.error.URLError as exc:
            raise SystemExit(f"Request failed: {exc}")
        results.append(result)
        print(json.dumps(result, ensure_ascii=False, indent=2))

    summary = summarize(results)
    print("\n=== Summary ===")
    print(json.dumps(summary, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
