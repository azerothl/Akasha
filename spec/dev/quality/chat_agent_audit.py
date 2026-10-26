#!/usr/bin/env python3
"""Chat/agent acceptance through real HTTP, streaming, tools and SQLite.

The default provider is a deterministic local OpenAI-compatible server. It
injects the two malformed replies reported on 2026-10-25, then checks recovery,
actual filesystem/web tool effects, routing and final task outcomes. It never
opens the user's Akasha database. --model switches the same factual/read/write
cases to a real local Ollama model (no external account or paid calls).
"""
from __future__ import annotations

import argparse
import contextlib
import datetime as dt
import hashlib
import json
import os
import re
import sqlite3
import subprocess
import tempfile
import threading
import time
from collections import Counter
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from real_agentic_eval import ROOT, Daemon, choose_port, clean_env, http_json, write_router


class Provider:
    def __init__(self, root: Path):
        self.root = root
        self.case = ""
        self.turns: Counter[str] = Counter()
        self.errors: list[str] = []
        self.requests: list[dict] = []
        self.base = ""

    def reply(self, body: dict) -> tuple[str, dict | None]:
        text = "\n".join(str(m.get("content", "")) for m in body.get("messages", []))
        if "You are a strict routing selector" in text:
            return json.dumps({"answer_mode": "delegate" if self.case == "plan_echo" or self.case.startswith("delegation_") else "direct", "task_type": "conversation"}), None
        if "SATISFACTORY" in text and "NEEDS_REFINEMENT" in text:
            return "SATISFACTORY", None
        if "Full multi-sentence instructions for this step" in text and "You may output ONLY a JSON object" in text:
            if self.case.startswith("delegation_"):
                return json.dumps({"steps":[
                    {"step_id":"s0", "agent_type":"documentalist", "intent":f"Lis le fichier {self.root / 'canary.txt'} et donne son contenu.", "depends_on":[]},
                    {"step_id":"s1", "agent_type":"conversation", "intent":"Synthétise le contenu obtenu avec le marqueur DELEGATION_OK.", "depends_on":["s0"]},
                ]}), None
            return json.dumps({"steps": [{"step_id": "s0", "agent_type": "documentalist", "intent": "Full multi-sentence instructions for this step (not a one-liner)", "deliverables": ["workspace:/optional/path.md"]}]}), None
        if any(s in text for s in ["Extract only facts", "Extract stable facts", "You are a memory", "memory extraction"]):
            return "[]", None
        case = self.case
        turn = self.turns[case]
        self.turns[case] += 1
        self.requests.append({"case": case, "turn": turn, "tools": [t["function"]["name"] for t in body.get("tools", [])]})

        def call(name: str, arguments: dict):
            names = [t["function"]["name"] for t in body.get("tools", [])]
            if name not in names:
                self.errors.append(f"{case}: {name} was not advertised")
            return "", {"id": f"call_{case}_{turn}", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}

        if case == "qa":
            return "La capitale de la France est Paris.", None
        if case == "installed_version":
            version = re.search(r"running Akasha daemon product version is ([\d.]+)", text)
            if not version:
                self.errors.append("installed_version: authoritative runtime version missing")
            return f"Le daemon Akasha utilisé est en version {version.group(1).rstrip('.') if version else 'unknown'}.", None
        if case == "followup":
            if "Paris" not in text:
                self.errors.append("followup: previous answer missing from session context")
            return "Je confirme : Paris est la capitale de la France.", None
        if case == "incognito":
            if "Paris" in text:
                self.errors.append("incognito: session history should be excluded")
            return "Peux-tu préciser ta question ?", None
        if case == "cancel":
            time.sleep(2)
            return "LATE_RESULT_MUST_NOT_COMPLETE", None
        if case == "delegation_failure":
            return "<|tool_call_start|>[read_file(path='unparsed')]<|tool_call_end|>", None
        if case == "delegation_success":
            if turn == 0:
                return call("read_file", {"path":str(self.root / "canary.txt")})
            if "CANARY_42" not in text:
                self.errors.append("delegation_success: dependency result missing")
            return "DELEGATION_OK : CANARY_42", None
        if case in {"json_repair", "json_stuck"} and (turn == 0 or case == "json_stuck"):
            return 'Je vais vérifier la version pour toi.\n```json\n{"type":"tool_use","tool_name":"web_search","input":{"query":"Akasha audit release"}}\n```', None
        if case == "plain_promise" and turn == 0:
            return "Je vais vérifier la version pour toi.", None
        if case == "lfm_repair" and turn == 0:
            return f"<|tool_call_start|>[read_file(path='{self.root / 'canary.txt'}')]<|tool_call_end|>", None
        if case == "fenced_repair" and turn == 0:
            return f"```text\nTOOL: read_file {self.root / 'canary.txt'}\n```", None
        if case in {"read", "lfm_repair", "fenced_repair", "promise_stuck"}:
            if turn == (1 if case in {"lfm_repair", "fenced_repair"} else 0):
                return call("read_file", {"path": str(self.root / "canary.txt")})
            if case == "promise_stuck":
                return "Je vais consulter le fichier, un instant.", None
            if "CANARY_42" not in text:
                self.errors.append(f"{case}: actual read result missing")
            return "Le fichier contient CANARY_42.", None
        if case in {"search", "json_repair", "plain_promise"}:
            offset = 1 if case in {"json_repair", "plain_promise"} else 0
            if turn == offset:
                return call("web_search", {"query": "Akasha audit release", "max_results": 3})
            if turn == offset + 1:
                return call("web_fetch", {"url": self.base + "/release"})
            if "9.8.7-AUDIT" not in text:
                self.errors.append(f"{case}: actual web page missing")
            return f"La source de test indique 9.8.7-AUDIT. [Source]({self.base}/release)", None
        if case in {"ask_denied", "write_denied"}:
            if turn == 0:
                # Legacy spelling tests the runtime role boundary too, independent of native schemas.
                return f"TOOL: write_file {self.root / ('blocked' if case == 'write_denied' else 'ask-write.txt')}\nUNAUTHORIZED", None
            return "Le fichier a été créé.", None  # A false model claim must not override a failed receipt.
        if case == "write_verify":
            path = str(self.root / "answer.txt")
            if turn == 0:
                return call("write_file", {"path": path, "content": "Bonjour été 🙂"})
            if turn == 1:
                return call("read_file", {"path": path})
            if "Bonjour été 🙂" not in text:
                self.errors.append("write_verify: actual verification result missing")
            return "Le fichier answer.txt a été créé et relu : Bonjour été 🙂.", None
        if case == "example":
            return "Exemple de syntaxe à expliquer :\n```text\nTOOL: run_command echo DEMO\n```", None
        if case in {"false_file_claim", "read_only_claim"}:
            if case == "read_only_claim" and turn == 0:
                return call("read_file", {"path": str(self.root / "canary.txt")})
            return "Le fichier demandé a été créé.", None
        if case == "plan_echo":
            return "Comparaison terminée : les documents de test sont identiques.", None
        return "Réponse de test.", None


def provider_server(provider: Provider):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *args):
            pass

        def do_GET(self):
            if self.path.startswith("/search"):
                data = json.dumps({"results": [{"title": "Release fixture", "url": provider.base + "/release", "content": "Release audit fixture: 9.8.7-AUDIT"}]}).encode()
                content_type = "application/json"
            else:
                data = b"<html><body><h1>Akasha audit fixture</h1><p>Version: 9.8.7-AUDIT. This is synthetic acceptance data.</p></body></html>"
                content_type = "text/html"
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            try:
                self.wfile.write(data)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
            if self.path == "/control":
                provider.case = body["case"]
                provider.turns[provider.case] = 0
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.end_headers()
                self.wfile.write(b'{"ok":true}')
                return
            try:
                text, tool = provider.reply(body)
            except Exception as error:
                provider.errors.append(str(error))
                text, tool = "Provider fixture error", None
            model = body.get("model", "audit-model")
            if body.get("stream"):
                chunks = []
                if tool:
                    chunks.append({"choices": [{"delta": {"tool_calls": [{"index": 0, **tool}]}, "finish_reason": None}]})
                else:
                    chunks.extend({"choices": [{"delta": {"content": text[i:i+3]}, "finish_reason": None}]} for i in range(0, len(text), 3))
                chunks.append({"model": model, "choices": [{"delta": {}, "finish_reason": "tool_calls" if tool else "stop"}], "usage": {"prompt_tokens": 100, "completion_tokens": 30}})
                data = ("".join("data: " + json.dumps(chunk, ensure_ascii=False) + "\n\n" for chunk in chunks) + "data: [DONE]\n\n").encode()
                content_type = "text/event-stream"
            else:
                message = {"role": "assistant", "content": text}
                if tool:
                    message["tool_calls"] = [tool]
                data = json.dumps({"model": model, "choices": [{"message": message, "finish_reason": "tool_calls" if tool else "stop"}], "usage": {"prompt_tokens": 100, "completion_tokens": 30}}).encode()
                content_type = "application/json"
            self.send_response(200)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            try:
                self.wfile.write(data)
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    provider.base = f"http://127.0.0.1:{server.server_port}"
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def run(args):
    results = []
    source_paths = subprocess.check_output(["git", "diff", "--name-only", "HEAD"], cwd=ROOT, text=True).splitlines()
    source_paths += ["crates/akasha-daemon/src/chat_contract.rs", "spec/dev/quality/chat_agent_audit.py", "apps/akasha-ui/e2e/chat-agent-results.spec.ts", "apps/akasha-ui/playwright.chat-audit.config.ts"]
    source_hashes = {path: hashlib.sha256((ROOT / path).read_bytes()).hexdigest() for path in sorted(set(source_paths)) if (ROOT / path).is_file() and "test-results" not in path}
    with tempfile.TemporaryDirectory(prefix="akasha-chat-audit-") as temp:
        root = Path(temp)
        data = root / "data"
        data.mkdir()
        (root / "canary.txt").write_text("CANARY_42", encoding="utf-8")
        (root / "blocked").mkdir()
        provider = Provider(root)
        server = provider_server(provider)
        env = {k: v for k, v in clean_env().items() if not k.upper().startswith("AKASHA_")}
        port = choose_port()
        env.update({"AKASHA_DATA_DIR": str(data), "AKASHA_SPEC_DIR": str(ROOT / "spec"), "AKASHA_PORT": str(port), "OPENAI_API_KEY": "audit-local-only", "AKASHA_LLM_TIMEOUT_SECS": "90", "AKASHA_LLM_FIRST_CHUNK_SECS": "90", "AKASHA_MAX_RESPONSE_TOKENS": "1024", "AKASHA_SELECTOR_TIMEOUT_MS": "2000"})
        if args.cli.is_file():
            subprocess.run([str(args.cli), "init", "--defaults"], cwd=ROOT, env=env, check=True, capture_output=True, timeout=30)
        if args.model:
            write_router(data, args.model, args.ollama_url, "auto")
        else:
            route = {"primary": {"provider": "openai", "model": "audit-model"}, "fallback": []}
            config = {"global": {"enable_fallback": False, "default_max_retries": 0, "default_timeout_secs": 30}, "providers": {"openai": {"base_url": provider.base + "/v1"}}, "task_types": {name: route for name in ["conversation", "system", "system_diagnostic", "orchestrator", "code_generation", "utility", "search"]}}
            (data / "llm_router.yaml").write_text(json.dumps(config), encoding="utf-8")
        policy = {"allowed_read_paths": [str(root)], "allowed_write_paths": [str(root / "answer.txt")], "allowed_commands": [], "allowed_web_domains": ["127.0.0.1"], "web_search_enabled": True, "search_provider": "searxng", "search_fallback_chain": [], "searxng_url": provider.base, "calendar_read_enabled": False, "default_profile": "audit", "tool_profiles": {"audit": ["read_file", "write_file", "web_search", "web_fetch", "run_command"]}}
        (data / "tools_policy.yaml").write_text(json.dumps(policy), encoding="utf-8")
        base = f"http://127.0.0.1:{port}"
        log_path = root / "daemon.log"
        daemon = Daemon(args.daemon, env, log_path, base)
        try:
            daemon.start()
            cases = [
                ("qa", "Quelle est la capitale de la France ?", "ask", "completed", "Paris"),
                ("followup", "?", "ask", "completed", "Paris"),
                ("installed_version", "Salut, on est sur quelle version d’Akasha là ?", "ask", "completed", re.search(r'\[workspace.package\]\s*version\s*=\s*"([^"]+)"', (ROOT / "Cargo.toml").read_text()).group(1)),
                ("read", f"Lis le fichier {root / 'canary.txt'} et donne exactement son contenu.", "ask", "completed", "CANARY_42"),
                ("search", "Recherche la version de test Akasha audit release, ouvre la source et cite sa version.", "ask", "completed", "9.8.7-AUDIT"),
                ("write_verify", f"Crée le fichier {root / 'answer.txt'} avec exactement Bonjour été 🙂, puis relis-le et donne son contenu.", "code", "completed", "Bonjour été 🙂"),
                ("ask_denied", f"Crée le fichier {root / 'ask-write.txt'} avec UNAUTHORIZED.", "ask", "failed", "validées"),
                ("example", "Explique la syntaxe TOOL: avec un exemple de commande dans un bloc de code, sans l'exécuter.", "ask", "completed", "DEMO"),
            ]
            if not args.model:
                cases += [
                    ("incognito", "?", "ask", "completed", "préciser"),
                    ("json_repair", "Recherche Akasha audit release et ouvre sa source pour répondre avec la version de test.", "ask", "completed", "9.8.7-AUDIT"),
                    ("plain_promise", "Recherche Akasha audit release et ouvre sa source pour répondre avec la version de test.", "ask", "completed", "9.8.7-AUDIT"),
                    ("lfm_repair", f"Lis le fichier {root / 'canary.txt'} et donne son contenu.", "ask", "completed", "CANARY_42"),
                    ("fenced_repair", f"Lis le fichier {root / 'canary.txt'} et donne son contenu.", "ask", "completed", "CANARY_42"),
                    ("json_stuck", "Recherche Akasha audit release et donne sa version.", "ask", "failed", "format"),
                    ("promise_stuck", f"Lis le fichier {root / 'canary.txt'} et donne son contenu.", "ask", "failed", "réponse demandée"),
                    ("write_denied", f"Crée le fichier {root / 'blocked'} avec UNAUTHORIZED.", "code", "failed", "validées"),
                    ("false_file_claim", f"Crée le fichier {root / 'claim.txt'} avec test.", "code", "failed", "validées"),
                    ("read_only_claim", f"Crée le fichier {root / 'claim.txt'} avec test.", "code", "failed", "validées"),
                    ("plan_echo", "Effectue une comparaison détaillée de ces deux documents de test identiques et fournis une synthèse argumentée de leurs similitudes, limites et différences.", None, "completed", "documents de test sont identiques"),
                    ("delegation_success", "Organise un projet multi-étapes de lecture du document de test, puis confie sa synthèse à un second spécialiste dépendant du premier résultat.", None, "completed", "DELEGATION_OK"),
                    ("delegation_failure", "Organise un projet multi-étapes de lecture du document de test, puis confie sa synthèse à un second spécialiste dépendant du premier résultat.", None, "failed", ""),
                    ("cancel", "Attends la réponse du modèle de test.", "ask", "cancelled", ""),
                ]
            else:
                cases = [(case, message, mode,
                    "completed" if case == "ask_denied" else status,
                    "mode" if case == "ask_denied" else "TOOL:" if case == "example" else text)
                    for case, message, mode, status, text in cases]
            if args.cases:
                cases = [case for case in cases if case[0] in args.cases.split(",")]
            for case, message, mode, expected_status, expected_text in cases:
                provider.case = case
                before_errors = len(provider.errors)
                started = time.monotonic()
                body = {"message": message + "\n\n[Préférence de réponse : privilégie web_search pour vérifier les informations actuelles et cite les sources utilisées.]", "display_message": message, "session_id": "audit-context" if case in {"qa", "followup", "incognito"} else f"audit-{case}", "incognito": case not in {"qa", "followup"}}
                if mode:
                    body["composer_mode"] = mode
                if not args.model:
                    # The fixture is an unverified OpenAI adapter, even on
                    # loopback. Authorize only this controlled synthetic task.
                    body["remote_consents"] = [{"recipient": f"model:openai:{provider.base}/v1", "data_classes": ["message", "history", "memory", "documents", "tool_results"]}]
                ack = http_json(base, "POST", "/api/message", body)
                task_id = ack["task_id"]
                if case == "cancel":
                    time.sleep(0.25)
                    http_json(base, "POST", f"/api/tasks/{task_id}/cancel", {})
                deadline = time.monotonic() + (180 if args.model else 60)
                task = {}
                while time.monotonic() < deadline:
                    task = http_json(base, "GET", f"/api/tasks/{task_id}")
                    if task.get("status") in {"completed", "failed", "cancelled", "interrupted", "waiting_user_input"}:
                        break
                    time.sleep(0.15)
                events = http_json(base, "GET", f"/api/tasks/{task_id}/events").get("events", [])
                if case == "cancel":
                    time.sleep(2.5)
                    task = http_json(base, "GET", f"/api/tasks/{task_id}")
                    events = http_json(base, "GET", f"/api/tasks/{task_id}/events").get("events", [])
                progress = task.get("progress", [])
                # Check receipts against the complete persisted stream, not
                # only the UI's bounded live event cache.
                time.sleep(0.1)
                with contextlib.closing(sqlite3.connect(f"file:{data / 'akasha.db'}?mode=ro", uri=True)) as receipt_db:
                    events = [{"event_type":kind, "payload":json.loads(payload or "{}")} for kind,payload in receipt_db.execute("SELECT event_type,payload_json FROM task_events WHERE task_id=? ORDER BY seq", (task_id,))]
                reply = next((p.get("message", "") for p in reversed(progress) if p.get("message")), "")
                tools = [e.get("payload", {}) for e in events if e.get("event_type") == "tool_invoked"]
                visible = "".join(e.get("payload", {}).get("delta", "") for e in events if e.get("event_type") == "assistant_text_delta")
                checks = {"expected_status": task.get("status") == expected_status, "expected_response": expected_text.casefold() in reply.casefold(), "no_protocol_leak": "<|tool_call_start|>" not in reply + visible and '"tool_use"' not in reply + visible}
                if args.model:
                    checks["requested_model_used"] = str(task.get("last_turn_model_used") or "").endswith(args.model)
                if mode == "ask":
                    checks["direct_ask"] = task.get("assigned_agent") == "ask" and not any(e.get("event_type") == "subtask_started" for e in events)
                if case in {"read", "lfm_repair", "fenced_repair"}:
                    checks["read_executed"] = any(t.get("tool") == "read_file" and t.get("success") for t in tools)
                if case in {"search", "json_repair", "plain_promise"}:
                    checks["search_and_fetch_executed"] = all(any(t.get("tool") == name and t.get("success") for t in tools) for name in ["web_search", "web_fetch"])
                if case == "write_verify":
                    checks["exact_file_bytes"] = (root / "answer.txt").read_bytes() == "Bonjour été 🙂".encode() if (root / "answer.txt").is_file() else False
                    checks["write_and_read_executed"] = all(any(t.get("tool") == name and t.get("success") for t in tools) for name in ["write_file", "read_file"])
                if case == "ask_denied":
                    checks["no_mutation"] = not (root / "ask-write.txt").exists()
                if case == "example":
                    checks["example_not_executed"] = not tools
                if case == "json_stuck":
                    checks["bounded_repairs"] = provider.turns[case] == 3
                if case == "plan_echo":
                    plans = [e.get("payload", {}) for e in events if e.get("event_type") == "plan_proposed"]
                    checks["no_template_plan"] = all("Full multi-sentence" not in json.dumps(p) for p in plans)
                if case.startswith("delegation_"):
                    starts = [e.get("payload", {}) for e in events if e.get("event_type") == "subtask_started"]
                    checks["dependency_dispatch"] = len(starts) == (2 if case == "delegation_success" else 1)
                if case == "cancel":
                    checks["no_late_completion"] = not any(e.get("event_type") == "task_completed" for e in events)
                checks["fixture_assertions"] = len(provider.errors) == before_errors
                with contextlib.closing(sqlite3.connect(f"file:{data / 'akasha.db'}?mode=ro", uri=True)) as db:
                    db.row_factory = sqlite3.Row
                    lineage = [dict(row) for row in db.execute("WITH RECURSIVE lineage(id) AS (SELECT ? UNION SELECT t.id FROM tasks t JOIN lineage p ON t.parent_task_id=p.id) SELECT id,parent_task_id,status,assigned_agent,outcome_json FROM tasks WHERE id IN (SELECT id FROM lineage)", (task_id,))]
                    for item in lineage:
                        item["outcome"] = json.loads(item.pop("outcome_json") or "{}")
                        item["final_progress"] = [dict(row) for row in db.execute("SELECT progress_pct,message FROM task_progress WHERE task_id=? ORDER BY seq DESC LIMIT 2", (item["id"],))]
                    checks["outcome_consistency"] = all(not item["outcome"].get("status") or item["outcome"]["status"] == item["status"] for item in lineage)
                    diagnostic_events = [{"event_type": e.get("event_type"), "payload":e.get("payload")} for e in events if e.get("event_type") in {"task_decomposed", "plan_proposed", "subtask_started", "subtask_completed", "task_completed", "task_failed"}]
                row = {"case": case, "task_id":task_id, "passed": all(checks.values()), "status": task.get("status"), "agent": task.get("assigned_agent"), "duration_ms": round((time.monotonic()-started)*1000), "checks": checks, "response": reply, "tools": [{"tool": t.get("tool"), "success": t.get("success")} for t in tools], "lineage":lineage, "events":diagnostic_events}
                row["event_counts"] = dict(Counter(e["event_type"] for e in events))
                row["model_used"] = task.get("last_turn_model_used")
                row["tool_trace"] = [e for e in events if "tool" in e["event_type"]]
                if case == "write_verify":
                    row["actual_file"] = (root / "answer.txt").read_text(encoding="utf-8") if (root / "answer.txt").is_file() else None
                results.append(row)
                print(json.dumps({k: row[k] for k in ["case", "passed", "status", "duration_ms", "checks"]}), flush=True)
            if args.ui:
                ui_env = {**os.environ, "AUDIT_DAEMON_BASE": base, "AUDIT_PROVIDER_BASE": provider.base, "AUDIT_SCREENSHOTS": str(args.output.parent.resolve())}
                command = ["npx.cmd" if os.name == "nt" else "npx", "playwright", "test", "--config", "playwright.chat-audit.config.ts"]
                ui = subprocess.run(command, cwd=ROOT / "apps/akasha-ui", env=ui_env, text=True, capture_output=True, timeout=240)
                print(ui.stdout, flush=True)
                if ui.stderr:
                    print(ui.stderr, flush=True)
                results.append({"case":"react_ui_to_daemon", "passed":ui.returncode == 0, "checks":{"playwright":ui.returncode == 0}, "output":ui.stdout})
        finally:
            daemon.stop()
            server.shutdown()
            args.output.parent.mkdir(parents=True, exist_ok=True)
            report = {"created_at":dt.datetime.now(dt.timezone.utc).isoformat(), "model": args.model or "deterministic-local-fixture", "source": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(), "dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).strip()), "source_sha256":source_hashes, "daemon_sha256":hashlib.sha256(args.daemon.read_bytes()).hexdigest(), "results": results, "provider_assertions": provider.errors, "requests": provider.requests}
            args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8")
            args.output.with_suffix(".log").write_bytes(log_path.read_bytes() if log_path.exists() else b"")
    return 0 if results and all(row["passed"] for row in results) else 1


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", type=Path, default=ROOT / "target/debug/akasha-daemon.exe")
    parser.add_argument("--cli", type=Path, default=ROOT / "target/debug/akasha.exe")
    parser.add_argument("--model")
    parser.add_argument("--cases", help="Comma-separated subset for a focused rerun")
    parser.add_argument("--ui", action="store_true")
    parser.add_argument("--ollama-url", default="http://127.0.0.1:11434")
    parser.add_argument("--output", type=Path, default=ROOT / "docs/validation/chat-agent-audit-2026-10-26/deterministic.json")
    raise SystemExit(run(parser.parse_args()))
