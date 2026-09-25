#!/usr/bin/env bash
# Embedded model bench — wrapper for bench_embedded.ps1 logic on Unix (uses curl + python3).
set -euo pipefail

BACKEND="${BACKEND:-auto}"
PORT="${PORT:-3876}"
JSON="${JSON:-0}"
STRICT="${STRICT:-0}"

BASE="http://127.0.0.1:${PORT}"
STATUS=$(curl -sf "${BASE}/api/router/embedded-status")
echo "$STATUS" | python3 -c "import json,sys; d=json.load(sys.stdin); sys.exit(0 if d.get('embedded_available') else 1)" || {
  echo "Embedded LLM not available" >&2
  exit 1
}

export AKASHA_EMBEDDED_BACKEND="$BACKEND"
BODY='{"message":"Hello in one sentence.","provider":"akasha_embedded","max_tokens":64}'
START=$(python3 -c "import time; print(time.time())")
TASK=$(curl -sf -X POST -H "Content-Type: application/json" -d "$BODY" "${BASE}/api/message")
TASK_ID=$(echo "$TASK" | python3 -c "import json,sys; print(json.load(sys.stdin).get('task_id',''))")
[ -n "$TASK_ID" ] || { echo "No task_id" >&2; exit 1; }

while true; do
  sleep 0.3
  T=$(curl -sf "${BASE}/api/tasks/${TASK_ID}")
  ST=$(echo "$T" | python3 -c "import json,sys; print(json.load(sys.stdin).get('status',''))")
  if [ "$ST" = "done" ] || [ "$ST" = "completed" ]; then
    END=$(python3 -c "import time; print(time.time())")
    DUR=$(python3 -c "print(round(float('$END')-float('$START'),2))")
    echo "duration_s: $DUR backend: $BACKEND"
    exit 0
  fi
  if [ "$ST" = "failed" ]; then
    echo "Task failed" >&2
    exit 1
  fi
done
