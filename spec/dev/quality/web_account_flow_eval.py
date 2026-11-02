#!/usr/bin/env python3
"""Exercise account registration through /api/message against a loopback-only service.

No public account or real credentials are used. The default Ollama-compatible
fixture tests the actual router, context guard, tools and persisted task state.
--model selects a real local Ollama model instead of the deterministic fixture.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from real_agentic_eval import ROOT, ROUTES, choose_port, clean_env, copy_daemon_with_runtime, http_json, write_router


class Fixture(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def send(self, value, status=200):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def forward_ollama(self, body=None):
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request("http://127.0.0.1:11434" + self.path, data=data,
                                         headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(request, timeout=240) as response:
            payload = response.read()
            self.send_response(response.status)
            self.send_header("Content-Type", response.headers.get("Content-Type", "application/json"))
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    def do_GET(self):
        state = self.server.state
        if self.path == "/api/tags":
            if state["real_model"]:
                return self.forward_ollama()
            self.send({"models": [{"name": "account-fixture:latest", "model": "account-fixture:latest", "size": 1000}]})
        elif self.path == "/agents.html":
            state["reads"] += 1
            body = ("Create one agent account with POST /api/v2/accounts: "
                    '{"name":"FixtureAgent","password":"SyntheticOnly-2026"}. '
                    "The response has apiToken. Verify it using GET /api/v2/me "
                    "with Authorization: Bearer <apiToken>. Keep the token private.").encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.end_headers()
            self.wfile.write(b"<html><body><main>" + body + b"</main></body></html>")
        elif self.path == "/api/v2/me":
            if self.headers.get("Authorization") != "Bearer " + state["token"]:
                return self.send({"error": "invalid_token"}, 401)
            state["verified"] += 1
            self.send({"id": "fixture-agent-1", "name": "FixtureAgent"})
        else:
            self.send({"error": "not_found"}, 404)

    def do_POST(self):
        state = self.server.state
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")
        if self.path == "/api/v2/accounts":
            state["create_attempts"] += 1
            if state["reject"]:
                return self.send({"error": "registration_unavailable"}, 503)
            if body != {"name": "FixtureAgent", "password": "SyntheticOnly-2026"}:
                return self.send({"error": "invalid_registration"}, 400)
            if state["creates"]:
                state["duplicate_attempts"] += 1
                return self.send({"error": "account_exists"}, 409)
            state["creates"] += 1
            self.send({"id": "fixture-agent-1", "name": "FixtureAgent", "apiToken": state["token"]}, 201)
        elif self.path == "/api/show":
            if state["real_model"]:
                return self.forward_ollama(body)
            self.send({"template": "{{ .System }} {{ .Messages }} {{ .Tools }}" if state["native_chat"] else "{{ .System }} {{ .Prompt }}", "model_info": {"general.architecture": "qwen", "qwen.context_length": 8192}, "parameters": "num_ctx 8192"})
        elif self.path in {"/api/chat", "/api/generate"}:
            text = json.dumps(body, ensure_ascii=False)
            state["request_bytes"].append(len(text.encode()))
            if state["native_chat"]:
                assert not body.get("tools"), "Incomplete native catalogue must not disable text tools"
                assert "Do not print TOOL:" not in text
            if state["steer_stop"]:
                state["model_started"].set()
                assert state["release_model"].wait(20), "Steering was not delivered"
            state["token_exposed_to_model"] |= state["token"] in text
            if state["real_model"]:
                return self.forward_ollama(body)
            if "strict routing selector" in text:
                state["selector_calls"] += 1
                response = '{"answer_mode":"delegate","task_type":"orchestrator"}'
            elif state["refuse"]:
                response = "Je ne peux pas utiliser d'outils ni accéder à votre ordinateur."
            elif state["read_only"] and "just summarize the requirements" in text:
                response = "Voici les conditions requises. Aucun compte n'a été créé."
            elif state["read_only"]:
                response = "TOOL: web_fetch " + state["origin"] + "/agents.html" if not state["reads"] else 'TOOL: ask_user\n{"question":"Continuer cette inscription fictive ?"}'
            elif state["cancel"]:
                response = 'TOOL: ask_user\n{"question":"Continuer cette inscription fictive ?"}'
            elif not state["reads"]:
                response = "TOOL: web_fetch " + state["origin"] + "/agents.html"
            elif state["reject"] and state["create_attempts"]:
                response = "Le service a refusé l'inscription. Aucun compte n'a été créé."
            elif not state["verified"]:
                response = "TOOL: run_command " + sys.executable + " " + str(state["client"]) + " " + state["origin"]
            else:
                response = "Le compte FixtureAgent a été créé et vérifié."
            self.send({"model": "account-fixture", "response": response, "message": {"role": "assistant", "content": response}, "done": True, "prompt_eval_count": len(text) // 3, "eval_count": 60})
        else:
            self.send({"error": "not_found"}, 404)


CLIENT = '''import json, sys, urllib.request
base=sys.argv[1]
body=json.dumps({"name":"FixtureAgent","password":"SyntheticOnly-2026"}).encode()
r=urllib.request.Request(base+"/api/v2/accounts",data=body,headers={"Content-Type":"application/json"},method="POST")
with urllib.request.urlopen(r) as response: account=json.load(response)
r=urllib.request.Request(base+"/api/v2/me",headers={"Authorization":"Bearer "+account["apiToken"]})
with urllib.request.urlopen(r) as response: me=json.load(response)
assert me["name"]=="FixtureAgent"
print(json.dumps({"verified":True,"id":me["id"],"name":me["name"]}))
'''


def run(daemon: Path, evidence: Path, model: str | None, cancel: bool = False, reject: bool = False, refuse: bool = False, read_only: bool = False, native_chat: bool = False, steer_stop: bool = False):
    evidence.mkdir(parents=True, exist_ok=True)
    profile = Path(tempfile.mkdtemp(prefix="web-account-", dir=evidence)).resolve()
    test_daemon = copy_daemon_with_runtime(daemon, profile / daemon.name)
    client = profile / "account_client.py"
    client.write_text(CLIENT, encoding="utf-8")
    service = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    origin = f"http://127.0.0.1:{service.server_port}"
    service.state = {"reads": 0, "creates": 0, "create_attempts": 0, "duplicate_attempts": 0, "verified": 0, "selector_calls": 0,
                     "request_bytes": [], "token_exposed_to_model": False,
                     "token": "fixture-token-must-never-leave-worker", "origin": origin,
                     "client": client, "cancel": cancel, "reject": reject, "refuse": refuse, "read_only": read_only, "real_model": model,
                     "native_chat": native_chat, "steer_stop": steer_stop, "model_started": threading.Event(), "release_model": threading.Event()}
    thread = threading.Thread(target=service.serve_forever, daemon=True)
    thread.start()
    write_router(profile, model or "account-fixture", origin, "cuda")
    router_path = profile / "llm_router.yaml"
    router = json.loads(router_path.read_text())
    for route in ROUTES:
        router["task_types"][route]["primary"]["config"].update(num_ctx=8192, thinking_level="off")
    router_path.write_text(json.dumps(router), encoding="utf-8")
    policy = {"allowed_read_paths": [str(profile)], "allowed_write_paths": [str(profile)],
              "allowed_web_domains": ["127.0.0.1"], "web_search_enabled": False,
              "web_crawl_enabled": False, "browser_enabled": False, "require_approval": [],
              "allowed_commands": [sys.executable, "powershell.exe", "curl.exe"],
              "command_timeout_secs": 20, "default_profile": "fixture",
              "tool_profiles": {"fixture": ["web_fetch", "run_command", "read_file"]}}
    (profile / "tools_policy.yaml").write_text(json.dumps(policy), encoding="utf-8")
    port = choose_port()
    base = f"http://127.0.0.1:{port}"
    env = {k: v for k, v in clean_env().items() if not k.startswith("AKASHA_")}
    env.update(AKASHA_DATA_DIR=str(profile), AKASHA_PORT=str(port), AKASHA_BIND="127.0.0.1",
               AKASHA_SPEC_DIR=str(ROOT / "spec"), AKASHA_LLM_TIMEOUT_SECS="240")
    with (profile / "daemon.log").open("w", encoding="utf-8") as log:
        process = subprocess.Popen([str(test_daemon)], env=env, cwd=profile, stdout=log, stderr=log,
                                   creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
        try:
            deadline = time.monotonic() + 60
            while True:
                if process.poll() is not None:
                    raise RuntimeError(f"Test daemon exited; inspect {profile / 'daemon.log'}")
                try:
                    http_json(base, "GET", "/api/status", timeout=1)
                    break
                except RuntimeError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.2)
            prompt = (f"Crée un compte agent de test sur {origin}/agents.html. "
                      "Nom : FixtureAgent. Mot de passe fictif : SyntheticOnly-2026. "
                      "Vérifie ensuite ce compte avec l'API documentée ; garde le jeton privé. "
                      "Ce service est une simulation locale, aucune inscription publique.")
            task_id = http_json(base, "POST", "/api/message", {"message": prompt, "session_id": "account-fixture"})["task_id"]
            if steer_stop:
                assert service.state["model_started"].wait(30)
                queued = http_json(base, "POST", "/api/message", {"message": "annule", "session_id": "account-fixture", "queue_mode": "steering", "target_task_id": task_id})
                assert queued.get("queued") and queued.get("task_id") == task_id, queued
                service.state["release_model"].set()
            deadline = time.monotonic() + (600 if model else 180)
            answered = False
            while time.monotonic() < deadline:
                task = http_json(base, "GET", f"/api/tasks/{task_id}")
                if (cancel or read_only) and not answered:
                    events = http_json(base, "GET", f"/api/tasks/{task_id}/events")["events"]
                    questions = [e["payload"] for e in events if e["event_type"] == "task_waiting_user_input"]
                    if questions:
                        q = questions[-1]
                        http_json(base, "POST", f"/api/tasks/{task_id}/human-reply", {"request_id": q["request_id"], "response": "Don't register; just summarize the requirements" if read_only else "stop"})
                        answered = True
                if task["status"] in {"completed", "failed", "cancelled", "interrupted"}:
                    if (cancel or steer_stop) and not (task.get("outcome") or {}).get("response"):
                        time.sleep(0.25)
                        continue
                    break
                time.sleep(0.25)
            else:
                raise RuntimeError(f"Account task did not finish: {profile}")
            events = http_json(base, "GET", f"/api/tasks/{task_id}/events")["events"]
            tools = [e["payload"]["tool"] for e in events if e["event_type"] == "tool_invoked"]
            children = http_json(base, "GET", f"/api/tasks/{task_id}/children")
            result = {"status": task["status"], "outcome": task.get("outcome"), "tools": tools,
                      "creates": service.state["creates"], "verified": service.state["verified"],
                      "create_attempts": service.state["create_attempts"], "duplicate_attempts": service.state["duplicate_attempts"],
                      "model_calls": len(service.state["request_bytes"]),
                      "selector_calls": service.state["selector_calls"], "children": children,
                      "max_request_bytes": max(service.state["request_bytes"], default=0),
                      "token_exposed_to_model": service.state["token_exposed_to_model"],
                      "model": model or "deterministic-fixture", "profile": str(profile)}
            (profile / "report.json").write_text(json.dumps(result, indent=2, ensure_ascii=False), encoding="utf-8")
            assert not children.get("children"), result
            assert not service.state["token_exposed_to_model"], result
            if read_only:
                assert answered and task["status"] == "completed" and service.state["creates"] == 0, result
                assert "conditions" in task["outcome"]["response"].lower(), result
                assert len(service.state["request_bytes"]) == 3 and "web_fetch" in tools, result
            elif cancel or steer_stop:
                assert (answered or steer_stop) and task["status"] == "cancelled" and service.state["creates"] == 0, result
                assert task["outcome"]["result"] == "cancelled", result
                assert "annul" in task["outcome"]["response"].lower(), result
                assert len(service.state["request_bytes"]) == 1, result
            elif reject or refuse:
                assert task["status"] == "failed" and service.state["creates"] == 0, result
            else:
                assert task["status"] == "completed" and service.state["creates"] == 1 and service.state["verified"] >= 1, result
                assert service.state["duplicate_attempts"] == 0, result
                assert "web_fetch" in tools and "run_command" in tools and "web_search" not in tools and "web_crawl" not in tools, result
            assert service.state["selector_calls"] == 0, result
            print(json.dumps(result, ensure_ascii=False))
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
            service.shutdown()
            service.server_close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--model", help="Real local Ollama model; default uses a deterministic fixture")
    parser.add_argument("--cancel", action="store_true")
    parser.add_argument("--reject", action="store_true", help="Service rejects registration; task must fail explicitly")
    parser.add_argument("--refuse", action="store_true", help="Model invents a lack of tools; task must never succeed")
    parser.add_argument("--read-only", action="store_true", help="Human withdraws registration and asks for a summary only")
    parser.add_argument("--native-chat", action="store_true", help="Native-chat model retains the complete text tool catalogue")
    parser.add_argument("--steer-stop", action="store_true", help="Cancel through the steering queue during the first model call")
    args = parser.parse_args()
    run(args.daemon, args.evidence, args.model, args.cancel, args.reject, args.refuse, args.read_only, args.native_chat, args.steer_stop)
