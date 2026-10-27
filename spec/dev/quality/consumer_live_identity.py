"""Retain and check local model identity around every live corpus workflow."""
import hashlib
import json
import re
from pathlib import Path

from real_agentic_eval import http_json

OLLAMA = "http://127.0.0.1:11434"


def local_identity(binary, model):
    tags = http_json(OLLAMA, "GET", "/api/tags")
    entry = next((item for item in tags.get("models", [])
                  if item.get("name") == model or item.get("model") == model), None)
    if entry is None:
        raise ValueError("The selected local model is no longer installed")
    if entry.get("remote_host") or entry.get("remote_model"):
        raise ValueError("A remote Ollama alias cannot qualify as a local model")
    if not re.fullmatch(r"[0-9a-fA-F]{64}", entry.get("digest", "")):
        raise ValueError("The local model digest is unavailable")
    version = http_json(OLLAMA, "GET", "/api/version").get("version")
    if not isinstance(version, str) or not version.strip():
        raise ValueError("The local engine version is unavailable")
    with Path(binary).open("rb") as stream:
        binary_digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"provider": "ollama", "model": model, "model_digest": entry["digest"],
            "model_bytes": entry.get("size", 0), "engine": "ollama:" + version,
            "binary_sha256": binary_digest}


class LiveIdentityGuard:
    def __init__(self, binary, model, output, snapshot=None):
        self.snapshot = snapshot or (lambda: local_identity(binary, model))
        self.output = Path(output)
        self.baseline = None
        self.observations = []

    def retain(self, **state):
        self.output.write_text(json.dumps({"schema_version": 1,
            "observations": self.observations, "verified": False, **state},
            ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    def reject(self, message):
        self.retain(error=message)
        raise ValueError(message)

    def capture(self, phase):
        try:
            identity = self.snapshot()
        except Exception as error:
            self.observations.append({"phase": phase, "error": str(error)})
            self.reject(f"Cannot verify live model identity at {phase}: {error}")
        self.observations.append({"phase": phase, "identity": identity})
        if self.baseline is None:
            self.baseline = identity
        elif identity != self.baseline:
            self.reject(f"Live model identity changed at {phase}; rerun the entire corpus")
        self.retain()

    def workflow(self, name, command, execute):
        self.capture(name + ":before")
        try:
            return execute(name, command)
        finally:
            self.capture(name + ":after")

    def verify_probe(self, probe, receipt):
        before, after = probe.get("identity"), probe.get("identity_after")
        if (receipt.get("exit_code") != 0 or probe.get("identity_changed") is not False
                or probe.get("probe_suite_passed") is not True
                or not isinstance(before, dict) or before != after):
            self.reject("The model probe failed or reported identity drift; rerun the entire corpus")
        if not self.baseline or any(before.get(key) != value for key, value in self.baseline.items()):
            self.reject("The model probe identity differs from the live workflows")
        self.retain(verified=True, probe_identity=before)
        return before
