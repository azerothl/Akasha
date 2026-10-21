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
import csv
import datetime as dt
import hashlib
import json
import os
import platform
import re
import subprocess
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


def _git_commit() -> str | None:
    try:
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT,
            capture_output=True,
            check=True,
            text=True,
            timeout=3,
        )
        return result.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return os.environ.get("GITHUB_SHA")


def _git_worktree_dirty() -> bool | None:
    try:
        result = subprocess.run(
            ["git", "status", "--porcelain"],
            cwd=ROOT,
            capture_output=True,
            check=True,
            text=True,
            timeout=3,
        )
        return bool(result.stdout.strip())
    except (OSError, subprocess.SubprocessError):
        return None


def _gpu_devices() -> list[str]:
    try:
        result = subprocess.run(
            ["nvidia-smi", "--query-gpu=name,memory.total", "--format=csv,noheader"],
            capture_output=True,
            check=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return []
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def _nvidia_driver_version() -> str | None:
    try:
        result = subprocess.run(
            ["nvidia-smi", "--query-gpu=driver_version", "--format=csv,noheader"],
            capture_output=True,
            check=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    versions = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return ", ".join(dict.fromkeys(versions)) or None


def _cuda_toolkit_version() -> str | None:
    cuda_path = os.environ.get("CUDA_PATH")
    nvcc = None
    if cuda_path:
        executable = "nvcc.exe" if os.name == "nt" else "nvcc"
        nvcc = Path(cuda_path) / "bin" / executable
    command = str(nvcc) if nvcc and nvcc.is_file() else "nvcc"
    try:
        result = subprocess.run(
            [command, "--version"],
            capture_output=True,
            check=True,
            text=True,
            timeout=5,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    match = re.search(r"release\s+([\d.]+)", result.stdout)
    return match.group(1) if match else None


def _gpu_sample_summary(path: Path | None) -> dict | None:
    if path is None or not path.is_file():
        return None
    memory_mib: list[int] = []
    utilization_pct: list[int] = []
    try:
        with path.open(encoding="utf-8", newline="") as sample_file:
            for row in csv.reader(sample_file):
                if len(row) < 3:
                    continue
                memory_match = re.search(r"(\d+)\s*MiB", row[1])
                utilization_match = re.search(r"(\d+)\s*%", row[2])
                if memory_match:
                    memory_mib.append(int(memory_match.group(1)))
                if utilization_match:
                    utilization_pct.append(int(utilization_match.group(1)))
    except OSError:
        return None
    if not memory_mib:
        return {"source": path.name, "sample_count": 0}
    return {
        "source": path.name,
        "sample_count": len(memory_mib),
        "memory_used_min_mib": min(memory_mib),
        "memory_used_max_mib": max(memory_mib),
        "utilization_max_pct": max(utilization_pct) if utilization_pct else None,
    }


def _model_sha256(model_path: object) -> tuple[str | None, str | None]:
    if not isinstance(model_path, str) or not model_path:
        return None, None
    path = Path(model_path).expanduser()
    if not path.is_file():
        return None, path.name
    digest = hashlib.sha256()
    try:
        with path.open("rb") as model:
            for block in iter(lambda: model.read(8 * 1024 * 1024), b""):
                digest.update(block)
    except OSError:
        return None, path.name
    return digest.hexdigest(), path.name


def _normalize_model(value: object) -> str:
    return re.sub(r"[^a-z0-9]", "", str(value).lower())


def _matches_model(actual: object, expected: str) -> bool:
    actual_key = _normalize_model(actual)
    expected_key = _normalize_model(expected)
    return bool(actual_key and expected_key and expected_key in actual_key)


def live_run(
    protocol: dict,
    base: str,
    candidate_filter: str | None,
    max_tokens: int,
    expected_model: str | None,
    gpu_samples_path: Path | None = None,
) -> dict:
    if not candidate_filter:
        raise SystemExit("live mode requires --candidate; one daemon configuration cannot measure every candidate")
    if not expected_model:
        raise SystemExit("live mode requires --expected-model so the actual routed model can be checked")
    selected = [cand for cand in protocol["candidates"] if cand["id"] == candidate_filter]
    if not selected:
        raise SystemExit(f"unknown candidate id: {candidate_filter}")

    caps = http_json("GET", f"{base}/api/capabilities")
    status = http_json("GET", f"{base}/api/router/embedded-status")
    if not status.get("embedded_available"):
        raise SystemExit(
            "daemon embedded LLM not ready — run with --mock or download a GGUF first"
        )

    results = []
    errors = 0
    ngl = int(os.environ.get("AKASHA_EMBEDDED_N_GPU_LAYERS", "99"))
    for prompt in protocol["prompts"]:
        row = {
            "mode": "live",
            "candidate_id": candidate_filter,
            "expected_model": expected_model,
            "arch": selected[0].get("arch"),
            "tier": selected[0].get("tier"),
            "prompt_id": prompt["id"],
            "n_gpu_layers": ngl,
            "actions": [],
            "tools": [],
            "ok": False,
        }
        started = time.perf_counter()
        try:
            response = http_json(
                "POST",
                f"{base}/api/complete",
                {"prompt": prompt["text"], "max_tokens": max_tokens, "temperature": 0.0},
                timeout=600.0,
            )
            wall_s = time.perf_counter() - started
            text = response.get("text")
            model_used = response.get("model_used")
            provider_used = response.get("provider_used")
            usage = response.get("usage") or {}
            completion_tokens = response.get("eval_count") or usage.get("completion_tokens")
            runtime_status = http_json("GET", f"{base}/api/router/embedded-status")
            runtime_model = runtime_status.get("active_model_id")
            model_match = _matches_model(model_used, expected_model) or (
                str(model_used).strip().lower() in {"default", "embedded"}
                and _matches_model(runtime_model, expected_model)
            )
            server_latency_ms = response.get("latency_ms")
            ttft_ns = response.get("ttft_ns")
            load_duration_ns = response.get("load_duration_ns")
            generation_duration_ns = response.get("generation_duration_ns")
            generation_s = (
                generation_duration_ns / 1_000_000_000
                if isinstance(generation_duration_ns, int) and generation_duration_ns > 0
                else None
            )
            row.update(
                {
                    "provider_used": provider_used,
                    "model_used": model_used,
                    "runtime_model": runtime_model,
                    "model_route": model_used,
                    "model_match": model_match,
                    "response_chars": len(text.strip()) if isinstance(text, str) else 0,
                    "completion_tokens": completion_tokens,
                    "token_count_source": "eval_count" if response.get("eval_count") else "usage.completion_tokens" if usage.get("completion_tokens") else None,
                    "duration_s": round(wall_s, 3),
                    "server_latency_ms": server_latency_ms,
                    "ttft_s": round(ttft_ns / 1_000_000_000, 6)
                    if isinstance(ttft_ns, int) and ttft_ns > 0
                    else None,
                    "ttft_status": "measured" if isinstance(ttft_ns, int) and ttft_ns > 0 else "not_exposed_by_backend",
                    "load_s": round(load_duration_ns / 1_000_000_000, 6)
                    if isinstance(load_duration_ns, int) and load_duration_ns > 0
                    else None,
                    "load_status": "measured_on_demand" if isinstance(load_duration_ns, int) and load_duration_ns > 0 else "model_already_loaded_or_not_exposed",
                    "generation_duration_s": round(generation_s, 6) if generation_s is not None else None,
                    "tok_per_s": round(completion_tokens / generation_s, 2)
                    if isinstance(completion_tokens, int) and completion_tokens > 0 and generation_s is not None
                    else None,
                }
            )
            if not isinstance(text, str) or not text.strip():
                row["error"] = "completion response text is empty"
            elif provider_used != protocol.get("provider"):
                row["error"] = f"routed provider {provider_used!r} does not match expected provider {protocol.get('provider')!r}"
            elif not row["model_match"]:
                row["error"] = f"routed model {model_used!r} / runtime model {runtime_model!r} does not match expected model {expected_model!r}"
            elif not isinstance(completion_tokens, int) or completion_tokens <= 0:
                row["error"] = "completion token count is unavailable; throughput cannot be reported"
            elif not isinstance(ttft_ns, int) or ttft_ns <= 0:
                row["error"] = "time to first token is unavailable"
            elif generation_s is None:
                row["error"] = "token generation duration is unavailable; throughput cannot be reported"
            else:
                row["ok"] = True
        except Exception as e:  # noqa: BLE001 — collect per-row errors
            row["duration_s"] = round(time.perf_counter() - started, 3)
            row["error"] = str(e)
        if not row["ok"]:
            errors += 1
        results.append(row)

    status = http_json("GET", f"{base}/api/router/embedded-status")
    runtime = caps.get("runtime") or {}
    model_sha256, model_filename = _model_sha256(status.get("model_path"))
    load_measured = any(isinstance(row.get("load_s"), (int, float)) for row in results)
    if not load_measured:
        errors += 1

    return {
        "schema_version": 1,
        "protocol_id": protocol["id"],
        "mode": "live",
        "generated_at_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "commit": _git_commit(),
        "source_tree_dirty": _git_worktree_dirty(),
        "environment": {
            "os": platform.platform(),
            "architecture": platform.machine(),
            "gpu_devices": _gpu_devices(),
            "nvidia_driver_version": _nvidia_driver_version(),
            "cuda_toolkit_version": _cuda_toolkit_version(),
            "backend": status.get("backend")
            or ("llama_cpp" if status.get("llama_cpp_compiled") else None),
            "device": status.get("device") or status.get("engine_mode"),
            "engine_mode": status.get("engine_mode"),
            "n_gpu_layers": status.get("n_gpu_layers"),
            "model_filename": model_filename,
            "model_sha256": model_sha256,
            "gpu_memory_samples": _gpu_sample_summary(gpu_samples_path),
            "initial_model_load_measured": load_measured,
        },
        "capabilities_schema_version": caps.get("schema_version"),
        "provider_expected": protocol.get("provider"),
        "runtime_gguf": runtime.get("gguf"),
        "runtime_model": status.get("active_model_id"),
        "decision_gate": protocol.get("decision_gate"),
        "results": results,
        "totals": {
            "rows": len(results),
            "ok": sum(1 for r in results if r.get("ok")),
            "errors": errors,
            "expected_rows": len(protocol["prompts"]),
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
    parser.add_argument(
        "--expected-model",
        help="Expected model_used value (or distinctive substring) returned by /api/complete; required for --live",
    )
    parser.add_argument(
        "--gpu-samples",
        type=Path,
        help="Optional nvidia-smi CSV with timestamp,memory.used,utilization.gpu columns",
    )
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
            out = live_run(
                protocol,
                base,
                args.candidate,
                max_tokens,
                args.expected_model,
                args.gpu_samples,
            )
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
    if out["mode"] == "mock":
        return 0
    totals = out["totals"]
    return 0 if totals.get("rows", 0) > 0 and totals.get("ok") == totals.get("expected_rows") else 1


if __name__ == "__main__":
    sys.exit(main())
