#!/usr/bin/env bash
# First-boot smoke for a published Linux x86_64 CPU release zip (issue #141).
# Downloads (or uses a local zip), extracts to a clean dir, temp HOME, init, daemon, doctor, one embedded reply.
#
# Usage:
#   bash scripts/smoke-release-artifact-linux.sh
#   RELEASE_TAG=v0.11.0 bash scripts/smoke-release-artifact-linux.sh
#   ARTIFACT_ZIP=/path/to/akasha-linux-x86_64.zip bash scripts/smoke-release-artifact-linux.sh
#   SKIP_INFERENCE=1 bash scripts/smoke-release-artifact-linux.sh   # HTTP + doctor only (faster)
#
# Env:
#   AKASHA_REPO     GitHub repo (default: azerothl/Akasha)
#   RELEASE_TAG     Release tag to download (default: v0.11.0)
#   ARTIFACT_NAME   Zip name (default: akasha-linux-x86_64.zip)
#   INFERENCE_TIMEOUT_SECS  Max wait for first embedded reply (default: 240)
set -euo pipefail

AKASHA_REPO="${AKASHA_REPO:-azerothl/Akasha}"
RELEASE_TAG="${RELEASE_TAG:-v0.11.0}"
ARTIFACT_NAME="${ARTIFACT_NAME:-akasha-linux-x86_64.zip}"
INFERENCE_TIMEOUT_SECS="${INFERENCE_TIMEOUT_SECS:-240}"
SKIP_INFERENCE="${SKIP_INFERENCE:-0}"

WORK="$(mktemp -d)"
FAKE_HOME="$(mktemp -d)"
EXTRACT="$WORK/extract"
LOG="$WORK/smoke.log"
PORT="$(python3 -c "import socket; s=socket.socket(); s.bind(('127.0.0.1',0)); print(s.getsockname()[1]); s.close()")"

cleanup() {
  if [[ -n "${SMOKE_EVIDENCE_DIR:-}" ]]; then
    mkdir -p "$SMOKE_EVIDENCE_DIR"
    for evidence in "$LOG" "$WORK/task.json" "$WORK/doctor.log" "$WORK/embedded-status.json"; do
      [[ ! -f "$evidence" ]] || cp -- "$evidence" "$SMOKE_EVIDENCE_DIR/"
    done
  fi
  if [[ -n "${DPID:-}" ]]; then
    kill "$DPID" 2>/dev/null || true
    wait "$DPID" 2>/dev/null || true
  fi
  rm -rf "$WORK" "$FAKE_HOME"
}
trap cleanup EXIT

log() { echo "[smoke-release-artifact] $*" | tee -a "$LOG"; }

if [[ -n "${ARTIFACT_ZIP:-}" ]]; then
  ZIP="$ARTIFACT_ZIP"
  [[ -f "$ZIP" ]] || { log "::error::ARTIFACT_ZIP not found: $ZIP"; exit 1; }
else
  ZIP="$WORK/$ARTIFACT_NAME"
  log "Downloading $AKASHA_REPO $RELEASE_TAG $ARTIFACT_NAME"
  gh release download "$RELEASE_TAG" --repo "$AKASHA_REPO" --pattern "$ARTIFACT_NAME" --dir "$WORK"
fi

mkdir -p "$EXTRACT"
unzip -q "$ZIP" -d "$EXTRACT"
# Zip may flatten at root or one subfolder
if [[ -f "$EXTRACT/akasha" ]]; then
  ROOT="$EXTRACT"
elif [[ -d "$EXTRACT/akasha-linux-x86_64" && -f "$EXTRACT/akasha-linux-x86_64/akasha" ]]; then
  ROOT="$EXTRACT/akasha-linux-x86_64"
else
  ROOT="$(find "$EXTRACT" -maxdepth 2 -name akasha -type f | head -1 | xargs dirname)"
fi
[[ -f "$ROOT/akasha" && -f "$ROOT/akasha-daemon" ]] || {
  log "::error::akasha binaries not found under $EXTRACT"
  exit 1
}

chmod +x "$ROOT/akasha" "$ROOT/akasha-daemon" "$ROOT/akasha-tui"
[[ -d "$ROOT/scripts" ]] && chmod +x "$ROOT/scripts/"*.sh 2>/dev/null || true

export HOME="$FAKE_HOME"
unset AKASHA_DATA_DIR || true
export AKASHA_PORT="$PORT"
cd "$ROOT"

log "HOME=$HOME AKASHA_PORT=$PORT extract=$ROOT"

INIT_START=$(date +%s)
./akasha --version | tee -a "$LOG"
./akasha init --defaults 2>&1 | tee -a "$LOG"
INIT_END=$(date +%s)
log "init_duration_s=$((INIT_END - INIT_START))"

[[ -f "$HOME/akasha/llm_router.yaml" ]] || {
  log "::error::data dir missing after init"
  exit 1
}

DAEMON_START=$(date +%s)
env -u GH_TOKEN -u GITHUB_TOKEN ./akasha-daemon >>"$LOG" 2>&1 &
DPID=$!
BASE="http://127.0.0.1:${PORT}"

ok=0
for _ in $(seq 1 90); do
  if curl -sf "${BASE}/api/status" >/dev/null 2>&1; then
    ok=1
    break
  fi
  sleep 1
done
DAEMON_END=$(date +%s)
if [[ "$ok" != 1 ]]; then
  log "::error::daemon did not become healthy within 90s"
  tail -40 "$LOG" || true
  exit 1
fi
log "daemon_ready_duration_s=$((DAEMON_END - DAEMON_START))"

DOCTOR_OK=1
if ./akasha doctor 2>&1 | tee "$WORK/doctor.log" | tee -a "$LOG"; then
  log "doctor_ok=true"
else
  DOCTOR_OK=0
  log "::error::doctor failed; diagnostics retained"
fi

curl -sf "${BASE}/" | grep -q '"status":"ok"' || { log "::error::GET /"; exit 1; }
curl -sf "${BASE}/api/status" | grep -q '"status":"ok"' || { log "::error::GET /api/status"; exit 1; }

EMBED_STATUS=$(curl -sf "${BASE}/api/router/embedded-status")
echo "$EMBED_STATUS" | tee "$WORK/embedded-status.json" | tee -a "$LOG"
echo "$EMBED_STATUS" | python3 -c "import json,sys; d=json.load(sys.stdin); assert d.get('embedded_available') is True, d" \
  || { log "::error::embedded not available"; exit 1; }

if [[ "$SKIP_INFERENCE" == 1 ]]; then
  if [[ "$DOCTOR_OK" != 1 ]]; then
    exit 1
  fi
  log "SKIP_INFERENCE=1 — smoke OK (no LLM call)"
  exit 0
fi

# Exercise one explicitly routed specialist request, not the multi-agent orchestrator.
# provider/max_tokens are not accepted by /api/message in this published release.
BODY='{"message":"Réponds uniquement par le mot OK.","composer_mode":"technical_writer"}'
INF_START=$(date +%s)
TASK_JSON=$(curl -sf -X POST -H "Content-Type: application/json" -d "$BODY" "${BASE}/api/message")
TASK_ID=$(echo "$TASK_JSON" | python3 -c "import json,sys; print(json.load(sys.stdin).get('task_id',''))")
[[ -n "$TASK_ID" ]] || { log "::error::no task_id: $TASK_JSON"; exit 1; }
log "task_id=$TASK_ID"

deadline=$((INF_START + INFERENCE_TIMEOUT_SECS))
while [[ $(date +%s) -lt $deadline ]]; do
  sleep 2
  T=$(curl -sf "${BASE}/api/tasks/${TASK_ID}")
  printf '%s\n' "$T" > "$WORK/task.raw.json"
  python3 - "$WORK/task.raw.json" "$WORK/task.json" <<'PY'
import json
import re
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    task = json.load(f)
for entry in task.get("progress", []):
    if isinstance(entry, dict) and isinstance(entry.get("message"), str):
        entry["message"] = re.sub(r"<think>.*?</think>", "", entry["message"], flags=re.DOTALL).strip()
with open(sys.argv[2], "w", encoding="utf-8") as f:
    json.dump(task, f, ensure_ascii=False, indent=2)
PY
  ST=$(echo "$T" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status',''))")
  if [[ "$ST" == "done" || "$ST" == "completed" ]]; then
    if ! python3 - "$WORK/task.json" <<'PY'
import json
import re
import sys

with open(sys.argv[1], encoding="utf-8") as f:
    task = json.load(f)

assert task.get("assigned_agent") == "technical_writer", "wrong assigned agent"
assert task.get("last_turn_model_used") == "embedded", "embedded model was not used"
assert task.get("last_turn_tokens_out", 0) > 0, "model returned no completion tokens"
messages = [
    entry.get("message", "")
    for entry in task.get("progress", [])
    if isinstance(entry, dict) and isinstance(entry.get("message"), str)
]
reply = next((message for message in reversed(messages) if message.strip()), "")
reply = re.sub(r"<think>.*?</think>", "", reply, flags=re.DOTALL).strip()
normalized = " ".join(reply.split())
if normalized != "OK":
    raise SystemExit(f"final model reply must be exactly 'OK'; got {normalized[:160]!r}")
PY
    then
      log "::error::model produced no valid answer for the deterministic prompt"
      exit 1
    fi
    INF_END=$(date +%s)
    log "first_inference_duration_s=$((INF_END - INF_START))"
    python3 -m json.tool "$WORK/task.json" >>"$LOG" 2>/dev/null || cat "$WORK/task.json" >>"$LOG"
    if [[ "$DOCTOR_OK" != 1 ]]; then
      log "::error::doctor failed; artifact is not validated"
      exit 1
    fi
    log "smoke OK: $RELEASE_TAG $ARTIFACT_NAME on $(uname -s)/$(uname -m)"
    exit 0
  fi
  if [[ "$ST" == "failed" || "$ST" == "error" ]]; then
    log "::error::task failed: $T"
    exit 1
  fi
done

log "::error::inference timed out after ${INFERENCE_TIMEOUT_SECS}s"
if [[ -f "$WORK/task.json" ]]; then
  python3 -m json.tool "$WORK/task.json" | tee -a "$LOG"
fi
curl -sf "${BASE}/api/router/embedded-status" | tee "$WORK/embedded-status.json" | tee -a "$LOG" || true
tail -100 "$LOG" || true
exit 1
