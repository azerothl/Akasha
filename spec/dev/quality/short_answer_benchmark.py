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
import tempfile
import threading
import time
from urllib.parse import urlsplit
from uuid import uuid4

from real_agentic_eval import (
    Daemon, binary_fingerprint, choose_port, clean_env, http_json,
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
    if case == "question":
        return "Qu’est-ce qu’un hexagone ?", lambda text: bool(re.search(r"\b(?:six|6)\s+côtés\b", text.lower()))
    if case == "calcul":
        def valid(text):
            lower = text.lower()
            totals = re.findall(r"\b(?:total|prix final|montant final)(?:\s+(?:final|est|s'élève|de|à|égal|vaut))*\s*[:=]?\s*(\d+(?:[,.]\d+)?)\s*(?:€|euros?)", lower)
            if not totals:
                totals = re.findall(r"=\s*(\d+(?:[,.]\d+)?)\s*(?:€|euros?)", lower)[-1:]
            return (totals and all(value in {"65", "65,0", "65.0", "65,00", "65.00"} for value in totals)
                    and re.search(r"\b80\b", lower) and re.search(r"\b5\b", lower)
                    and re.search(r"(?:0[,.]75|25\s*%|75\s*%)", lower)
                    and len(re.split(r"(?<=[.!?])\s+(?=\S)", lower.strip())) == 1)
        return DISCOUNT, valid
    if case == "ambigu":
        return DISCOUNT.replace(" de 25 %", ""), lambda text: (
            "?" in text and bool(re.search(r"pourcentage|taux|\bremise\b", text, re.I))
        )
    if case == "lecture":
        return f"Lis le fichier {fixture.as_posix()} avec l’outil read_file puis donne son contenu exact.", lambda text: marker in text

    def check_target(_text):
        return target.is_file() and target.read_text(encoding="utf-8").strip() == marker
    return (
        f"Lis {fixture.as_posix()}, copie son contenu exact dans {target.as_posix()}, "
        "puis relis la copie pour vérifier qu’elle est identique.",
        check_target,
    )


def run_case(case, root, base, calls, timeout):
    scope = root / uuid4().hex
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
        accepted = http_json(base, "POST", "/api/message", {"message": prompt, "new_session": True, "incognito": True})
        task_id = accepted["task_id"]
        while time.monotonic() - start < timeout:
            task = http_json(base, "GET", f"/api/tasks/{task_id}", timeout=5)
            if task.get("status") in TERMINAL:
                status = task["status"]
                elapsed = time.monotonic() - start
                answer = (task.get("outcome") or {}).get("response") or ""
                events = http_json(base, "GET", f"/api/tasks/{task_id}/events").get("events", [])
                break
            time.sleep(0.1)
    except (RuntimeError, KeyError) as exc:
        status, error = "error", str(exc)
    elapsed = elapsed if elapsed is not None else time.monotonic() - start
    return {
        "case": case, "status": status, "correct": status == "completed" and bool(verify(answer)),
        "latency_ms": round(elapsed * 1000, 1), "model_calls": len(calls) - previous,
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
    }


def run_binary(binary, args, upstream, selected):
    rows = []
    version = None
    # Restart after a timeout so a previous generation cannot leak into the next sample.
    with count_generations(upstream, args.timeout) as (proxy, calls):
        for _ in range(args.repetitions):
            with tempfile.TemporaryDirectory(prefix="akasha-answer-bench-") as tmp:
                root = Path(tmp)
                write_router(root, args.model, proxy, args.device)
                config_path = root / "llm_router.yaml"
                config = json.loads(config_path.read_text(encoding="utf-8"))
                config["global"].update(enable_fallback=False, default_timeout_secs=args.timeout)
                for route in config["task_types"].values():
                    route["primary"]["config"].update(max_tokens=256, num_ctx=4096)
                config_path.write_text(json.dumps(config), encoding="utf-8")
                fixtures = root / "fixtures"
                fixtures.mkdir()
                write_eval_tools_policy(root, fixtures, ["read_file", "write_file"])
                port = choose_port()
                base = f"http://127.0.0.1:{port}"
                env = clean_env()
                env = {key: val for key, val in env.items() if not key.startswith("AKASHA_")}
                env.update(AKASHA_DATA_DIR=str(root), AKASHA_PORT=str(port), AKASHA_BIND="127.0.0.1", AKASHA_EMBEDDINGS_OFFLINE="1")
                daemon = Daemon(binary, env, root / "daemon.log", base)
                try:
                    daemon.start()
                    version = http_json(base, "GET", "/api/status").get("version")
                    for case in selected:
                        if daemon.process is None:
                            daemon.start()
                        row = run_case(case, fixtures, base, calls, args.timeout)
                        rows.append(row)
                        print(f"{binary.name} {case} {row['status']} {row['latency_ms']} ms, "
                              f"correct={row['correct']}, appels={row['model_calls']}", flush=True)
                        if row["status"] in {"timeout", "error"}:
                            daemon.stop()
                finally:
                    daemon.stop()
    return {
        "version": version, "binary": binary_fingerprint(binary),
        "summary": {case: summary([row for row in rows if row["case"] == case]) for case in selected},
        "samples": rows,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--ollama", default="http://127.0.0.1:11434")
    parser.add_argument("--model", required=True)
    parser.add_argument("--device", choices=["cpu", "cuda"], default="cpu")
    parser.add_argument("--repetitions", type=int, default=30)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--cases", nargs="+", choices=CASES, default=list(CASES))
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    upstream = urlsplit(args.ollama)
    if upstream.scheme != "http" or upstream.hostname not in {"127.0.0.1", "localhost", "::1"} or upstream.path not in {"", "/"}:
        parser.error("--ollama doit désigner une instance HTTP locale")
    if args.repetitions < 1 or args.timeout < 1:
        parser.error("répétitions et délai doivent être positifs")
    args.ollama = args.ollama.rstrip("/")
    options = {"num_predict": 4, "num_ctx": 4096, "num_gpu": 0 if args.device == "cpu" else 99}
    http_json(args.ollama, "POST", "/api/generate", {
        "model": args.model, "prompt": "Bonjour", "think": False, "stream": False, "options": options,
    }, timeout=120)
    loaded = ollama_processes(args.ollama)
    warm = next((item for item in loaded if item["name"] in {args.model, args.model + ":latest"}), None)
    if warm is None or warm["size_vram_bytes"] is None or (warm["size_vram_bytes"] > 0) != (args.device == "cuda"):
        parser.error("Impossible de vérifier le chargement du modèle sur le matériel demandé")
    report = {
        "model": args.model, "device": args.device, "host": platform.platform(),
        "ollama": http_json(args.ollama, "GET", "/api/version"), "warm_model": warm,
        "repetitions": args.repetitions, "timeout_s": args.timeout, "cases": args.cases,
        "baseline": run_binary(args.baseline.resolve(strict=True), args, upstream, args.cases),
        "candidate": run_binary(args.candidate.resolve(strict=True), args, upstream, args.cases),
    }
    args.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
