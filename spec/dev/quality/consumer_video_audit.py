"""Record a synthetic document flow against a real local daemon and model."""
import argparse
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import tempfile
from real_agentic_eval import ROOT, Daemon, clean_env, choose_port, write_router, http_json

def model_identity():
    base = "http://127.0.0.1:11434"
    selected = next(model for model in http_json(base, "GET", "/api/tags")["models"] if model["name"] == "qwen3.5:9b")
    return {"provider": "ollama", "model": selected["name"], "digest": selected["digest"],
            "bytes": selected["size"], "engine_version": http_json(base, "GET", "/api/version")["version"]}

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["binary", "runtime", "output"]:
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    args.output = args.output.resolve()
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="akasha-consumer-video-") as temporary:
        root = Path(temporary)
        data = root / "profile"
        data.mkdir()
        binary = root / args.binary.name
        shutil.copy2(args.binary, binary)
        write_router(data, "qwen3.5:9b", "http://127.0.0.1:11434", "auto")
        env = {key: value for key, value in clean_env().items() if not key.startswith("AKASHA_")}
        env.update(AKASHA_DATA_DIR=str(data), AKASHA_SPEC_DIR=str(ROOT / "spec"),
                   AKASHA_PORT=str(choose_port()), AKASHA_OCR_RUNTIME_DIR=str(args.runtime.resolve()))
        daemon = Daemon(binary, env, root / "daemon.log", "http://127.0.0.1:" + env["AKASHA_PORT"])
        before = model_identity()
        try:
            daemon.start()
            process = subprocess.run(
                ["npx.cmd" if os.name == "nt" else "npx", "playwright", "test", "--config",
                 "playwright.consumer-live.config.ts", "--output", str(args.output / "browser")],
                cwd=ROOT / "apps/akasha-ui",
                env={**os.environ, "CONSUMER_LIVE_DAEMON": daemon.base,
                     "CONSUMER_LIVE_SCREENSHOT": str(args.output / "source.png"),
                     "CONSUMER_LIVE_MARKDOWN": str(args.output / "report.md")},
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=240)
            (args.output / "browser.log").write_bytes(process.stdout)
            print(process.stdout.decode(errors="replace"))
            with closing(sqlite3.connect(f"file:{data / 'akasha.db'}?mode=ro", uri=True)) as connection:
                receipts = [json.loads(row[0]) for row in connection.execute(
                    "SELECT payload_json FROM task_events WHERE event_type='consumer_model_receipt'")]
            after = model_identity()
            verified = (process.returncode == 0 and before == after and bool(receipts)
                        and all(row.get("model_used") == "qwen3.5:9b" and row.get("provider_used") == "ollama" for row in receipts))
            receipt = {"binary_sha256": hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                       "mode": "live", "model_identity": before, "model_identity_after": after,
                       "playwright_exit_code": process.returncode, "model_receipts": receipts,
                       "verified": verified, "tasks": http_json(daemon.base, "GET", "/api/tasks")}
            (args.output / "receipt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
            return 0 if verified else 1
        finally:
            daemon.stop()
if __name__ == "__main__":
    raise SystemExit(main())
