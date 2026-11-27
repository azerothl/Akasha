#!/usr/bin/env python3
"""Compare two Akasha daemons with one warmed-up, local Ollama model."""
from __future__ import annotations

import argparse
from collections import Counter
from contextlib import contextmanager
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
from pathlib import Path
import platform
import re
import sys
import tempfile
import threading
import time
from urllib.parse import urlsplit
from uuid import uuid4

from real_agentic_eval import (
    Daemon, binary_fingerprint, choose_port, clean_env, gpu_devices, http_json,
    ollama_processes, write_eval_tools_policy, write_router,
)

DISCOUNT = (
    "Un abonnement coûte 80 euros. Une remise de 25 % s'applique, puis on ajoute "
    "5 euros de frais. Quel est le total ? Réponds en une seule phrase en français avec le calcul."
)
CASES = ("question", "calcul", "ambigu", "lecture", "plusieurs_etapes")
TERMINAL = {"completed", "failed", "cancelled", "paused", "interrupted", "waiting_user_input"}


@contextmanager
def count_generations(upstream, timeout):
    calls = []

    class Relay(BaseHTTPRequestHandler):
        def relay(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", "0"))) or None
            if self.command == "POST" and urlsplit(self.path).path in {
                "/api/generate", "/api/chat", "/v1/chat/completions",
            }:
                calls.append(self.path)
            connection = HTTPConnection(upstream.hostname, upstream.port or 80, timeout=timeout + 5)
            try:
                connection.request(self.command, self.path, body, {"Content-Type": "application/json"})
                response = connection.getresponse()
                self.send_response(response.status)
                self.send_header("Content-Type", response.getheader("Content-Type", "application/json"))
                self.send_header("Connection", "close")
                self.end_headers()
                while chunk := response.read1(8192):
                    self.wfile.write(chunk)
                    self.wfile.flush()
            except (ConnectionError, TimeoutError):
                self.close_connection = True
            finally:
                connection.close()

        do_GET = do_POST = relay

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(("127.0.0.1", 0), Relay) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield f"http://127.0.0.1:{server.server_port}", calls
        finally:
            server.shutdown()
            thread.join()


def corpus(case, fixture, target, marker):
    def read_matches(tool, path):
        return (
            tool.get("tool") == "read_file"
            and marker in (tool.get("result_preview") or "")
            and any(path.name in arg for arg in tool.get("args", []))
        )

    if case == "question":
        def valid(text):
            lower = text.lower().strip()
            return (
                bool(re.search(r"\b(?:trois|3)\s+côtés\b", lower))
                and not re.search(r"polygone régulier|toujours équilatéral|tous ses côtés sont égaux", lower)
                and len(re.split(r"(?<=[.!?])\s+(?=\S)", lower)) == 1
            )
        return "Définis un triangle en une phrase.", lambda text, _: valid(text)
    if case == "calcul":
        def valid(text):
            lower = text.lower()
            totals = re.findall(r"\b(?:total|prix final|montant final)(?:\s+(?:final|est|s'élève|de|à|égal|vaut))*\s*[:=]?\s*(\d+(?:[,.]\d+)?)\s*(?:€|euros?)", lower)
            if not totals:
                totals = re.findall(r"=\s*(\d+(?:[,.]\d+)?)\s*(?:€|euros?)", lower)[-1:]
            return (totals and all(value in {"65", "65,0", "65.0", "65,00", "65.00"} for value in totals)
                    and re.search(r"\b80\b", lower)
                    and re.search(r"(?:\+\s*|ajout(?:e|er|ant)\s+|plus\s+)5\b", lower)
                    and re.search(r"(?:[×*]\s*(?:0[,.]75|75\s*%)|(?:[−-]|remise de)\s*25\s*%)", lower)
                    and len(re.split(r"(?<=[.!?])\s+(?=\S)", lower.strip())) == 1)
        return DISCOUNT, lambda text, _: valid(text)
    if case == "ambigu":
        return DISCOUNT.replace(" de 25 %", ""), lambda text, _: (
            "?" in text and bool(re.search(r"pourcentage|taux|\bremise\b", text, re.I))
        )
    if case == "lecture":
        return (
            f'Lis le fichier "{fixture.as_posix()}" avec read_file puis renvoie uniquement son contenu.',
            lambda text, tools: text.strip().strip("`") == marker
            and any(read_matches(tool, fixture) for tool in tools),
        )

    def check_target(_text, tools):
        operations = []
        for tool in tools:
            if read_matches(tool, fixture):
                operations.append("source")
            elif tool.get("tool") == "write_file":
                operations.append("write")
            elif read_matches(tool, target):
                operations.append("copy")
        return (
            any(operations[i:i + 3] == ["source", "write", "copy"] for i in range(len(operations)))
            and target.is_file() and target.read_text(encoding="utf-8") == marker
        )
    return (
        f'Lis "{fixture.as_posix()}" avec read_file, copie son contenu exact dans "{target.as_posix()}" '
        "avec write_file, puis relis la copie avec read_file pour vérifier qu'elle est identique.",
        check_target,
    )


def run_case(case, root, base, calls, timeout, index):
    scope = root / case
    scope.mkdir()
    marker = f"AKASHA-{uuid4().hex}"
    fixture, target = scope / "source.txt", scope / "copie.txt"
    fixture.write_text(marker, encoding="utf-8")
    prompt, verify = corpus(case, fixture, target, marker)
    previous = len(calls)
    start = time.monotonic()
    task_id = None
    answer = ""
    status = "timeout"
    error = None
    elapsed = None
    events = []
    try:
        accepted = http_json(base, "POST", "/api/message", {
            "message": prompt, "session_id": f"benchmark-{index}-{case}", "incognito": True,
        }, timeout=min(5, timeout))
        task_id = accepted["task_id"]
        while time.monotonic() - start < timeout:
            remaining = timeout - (time.monotonic() - start)
            task = http_json(base, "GET", f"/api/tasks/{task_id}", timeout=max(.01, min(5, remaining)))
            if task.get("status") in TERMINAL:
                status = task["status"]
                elapsed = time.monotonic() - start
                answer = (task.get("outcome") or {}).get("response") or ""
                events = http_json(base, "GET", f"/api/tasks/{task_id}/events").get("events", [])
                break
            time.sleep(0.1)
    except (RuntimeError, KeyError) as exc:
        status = "timeout" if time.monotonic() - start >= timeout else "error"
        error = str(exc)
    elapsed = elapsed if elapsed is not None else time.monotonic() - start
    tool_events = [
        event["payload"] for event in events
        if event.get("event_type") == "tool_invoked" and event.get("payload")
    ]
    tools = [event for event in tool_events if event.get("success")]
    return {
        "case": case, "repetition": index, "status": status,
        "correct": (status == "completed" or (case == "ambigu" and status == "waiting_user_input"))
        and bool(verify(answer, tools)),
        "latency_ms": round(elapsed * 1000, 1), "model_calls": len(calls) - previous,
        "generation_requests": calls[previous:], "task_id": task_id, "prompt": prompt,
        "expected_fixture": marker if case in {"lecture", "plusieurs_etapes"} else None,
        "copy_matches": target.is_file() and target.read_bytes() == marker.encode() if case == "plusieurs_etapes" else None,
        "tool_events": tool_events,
        "route": next((event.get("payload") for event in events if event.get("event_type") == "short_answer_route"), None),
        "answer": answer, "error": error,
    }


def summary(samples):
    correct = sorted(row["latency_ms"] for row in samples if row["correct"])
    def percentile(p):
        return correct[math.ceil(len(correct) * p) - 1] if correct else None
    return {
        "samples": len(samples), "correct": len(correct), "accuracy": len(correct) / len(samples),
        "p50_correct_ms": percentile(.5), "p95_correct_ms": percentile(.95),
        "model_calls": sum(row["model_calls"] for row in samples),
        "statuses": dict(Counter(row["status"] for row in samples)),
        "timeouts": sum(row["status"] == "timeout" for row in samples),
        "errors": sum(row["status"] in {"error", "failed"} for row in samples),
        "p95_sample_count": len(correct),
    }


def run_repetition(binary, label, args, proxy, calls, index, selected, raw):
    rows = []
    with tempfile.TemporaryDirectory(prefix="akasha-answer-", dir=args.work_dir) as tmp:
        root = Path(tmp)
        write_router(root, args.model, proxy, args.device)
        config_path = root / "llm_router.yaml"
        config = json.loads(config_path.read_text(encoding="utf-8"))
        config["global"].update(enable_fallback=False, default_timeout_secs=args.timeout)
        for route in config["task_types"].values():
            route["primary"]["config"].update(max_tokens=args.max_tokens, num_ctx=args.context_size)
        config_path.write_text(json.dumps(config), encoding="utf-8")
        fixtures = root / "fixtures"
        fixtures.mkdir()
        write_eval_tools_policy(root, fixtures, ["read_file", "write_file"])
        port = choose_port()
        base = f"http://127.0.0.1:{port}"
        env = {key: val for key, val in clean_env().items() if not key.startswith("AKASHA_")}
        env.update(AKASHA_DATA_DIR=str(root), AKASHA_PORT=str(port), AKASHA_BIND="127.0.0.1", AKASHA_EMBEDDINGS_OFFLINE="1")
        daemon = Daemon(binary, env, root / "daemon.log", base)
        try:
            daemon.start()
            version = http_json(base, "GET", "/api/status").get("version")
            for case in selected:
                if daemon.process is None:
                    daemon.start()
                row = run_case(case, fixtures, base, calls, args.timeout, index)
                rows.append(row)
                raw.write(json.dumps({"variant": label, **row}, ensure_ascii=False) + "\n")
                raw.flush()
                print(f"{index}/{args.repetitions} {label} {case} {row['status']} {row['latency_ms']} ms, "
                      f"correct={row['correct']}, appels={row['model_calls']}", flush=True)
                if row["status"] in {"timeout", "error"}:
                    daemon.stop()
        finally:
            daemon.stop()
    return version, rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--ollama", default="http://127.0.0.1:11434")
    parser.add_argument("--model", required=True)
    parser.add_argument("--device", choices=["cpu", "cuda"], default="cpu")
    parser.add_argument("--repetitions", type=int, default=30)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--context-size", type=int, default=4096)
    parser.add_argument("--max-tokens", type=int, default=256)
    parser.add_argument("--cases", nargs="+", choices=CASES, default=list(CASES))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--work-dir", type=Path, help="Répertoire de travail pour les profils jetables")
    args = parser.parse_args()
    upstream = urlsplit(args.ollama)
    if upstream.scheme != "http" or upstream.hostname not in {"127.0.0.1", "localhost", "::1"} or upstream.path not in {"", "/"}:
        parser.error("--ollama doit désigner une instance HTTP locale")
    if min(args.repetitions, args.timeout, args.context_size, args.max_tokens) < 1:
        parser.error("répétitions, délai, contexte et limite de sortie doivent être positifs")
    binaries = {"baseline": args.baseline.resolve(strict=True), "candidate": args.candidate.resolve(strict=True)}
    if binaries["baseline"].samefile(binaries["candidate"]):
        parser.error("les binaires avant et après doivent être distincts")
    if args.out.suffix.lower() != ".json":
        parser.error("--out doit se terminer par .json")
    args.ollama = args.ollama.rstrip("/")
    options = {"num_predict": 4, "num_ctx": args.context_size, "num_gpu": 0 if args.device == "cpu" else 99}
    http_json(args.ollama, "POST", "/api/generate", {
        "model": args.model, "prompt": "Bonjour", "think": False, "stream": False,
        "keep_alive": -1, "options": options,
    }, timeout=120)
    loaded = ollama_processes(args.ollama)
    warm = next((item for item in loaded if item["name"] in {args.model, args.model + ":latest"}), None)
    if warm is None or warm["size_vram_bytes"] is None or (warm["size_vram_bytes"] > 0) != (args.device == "cuda"):
        parser.error("Impossible de vérifier le chargement du modèle sur le matériel demandé")
    report = {
        "model": args.model, "device": args.device, "host": platform.platform(),
        "cpu": platform.processor(), "gpu": gpu_devices() if args.device == "cuda" else [],
        "command": [sys.executable, *sys.argv],
        "settings": {"temperature": 0, "num_ctx": args.context_size, "max_tokens": args.max_tokens, "thinking_level": "off",
                     "num_gpu": options["num_gpu"], "fallback": False},
        "ollama": http_json(args.ollama, "GET", "/api/version"), "warm_model": warm,
        "repetitions": args.repetitions, "timeout_s": args.timeout, "cases": args.cases,
    }
    for label, binary in binaries.items():
        report[label] = {"binary": binary_fingerprint(binary), "samples": []}
    with args.out.with_suffix(".jsonl").open("w", encoding="utf-8") as raw:
        with count_generations(upstream, args.timeout) as (proxy, calls):
            for index in range(1, args.repetitions + 1):
                order = list(binaries) if index % 2 else list(reversed(binaries))
                for label in order:
                    version, rows = run_repetition(binaries[label], label, args, proxy, calls, index, args.cases, raw)
                    report[label]["version"] = version
                    report[label]["samples"].extend(rows)
    for label in binaries:
        samples = report[label]["samples"]
        report[label]["overall"] = summary(samples)
        report[label]["summary"] = {case: summary([row for row in samples if row["case"] == case]) for case in args.cases}
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
