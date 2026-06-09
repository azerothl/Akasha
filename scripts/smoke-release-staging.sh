#!/usr/bin/env bash
# Smoke-test a release staging folder (binaries + docs + scripts) like users get in the zip.
# Usage: from repo root, after building into staging/:  bash scripts/smoke-release-staging.sh staging
set -euo pipefail

STAGING="${1:-staging}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ ! -d "$STAGING" ]]; then
  echo "::error::Staging directory not found: $STAGING"
  exit 1
fi

STAGING_ABS="$(cd "$STAGING" && pwd)"

if [[ -f "$STAGING_ABS/akasha-daemon" ]]; then
  DAEMON="$STAGING_ABS/akasha-daemon"
elif [[ -f "$STAGING_ABS/akasha-daemon.exe" ]]; then
  DAEMON="$STAGING_ABS/akasha-daemon.exe"
else
  echo "::error::No akasha-daemon binary in $STAGING_ABS"
  exit 1
fi

for f in docs/user/index.json scripts spec/tools_policy.example.yaml; do
  if [[ ! -e "$STAGING_ABS/$f" ]]; then
    echo "::error::Expected $STAGING_ABS/$f missing"
    exit 1
  fi
done
if [[ ! -f "$STAGING_ABS/docs/user_guide.md" ]]; then
  echo "::error::Expected $STAGING_ABS/docs/user_guide.md missing (legacy fallback)"
  exit 1
fi

DATA_DIR="$(mktemp -d)"
PORT="$(python3 -c "import socket; s=socket.socket(); s.bind(('127.0.0.1',0)); print(s.getsockname()[1]); s.close()")"

export AKASHA_DATA_DIR="$DATA_DIR"
export AKASHA_PORT="$PORT"

# Run from the staging folder (same layout as end-user zip extract).
cd "$STAGING_ABS"
./"$(basename "$DAEMON")" >/dev/null 2>&1 &
DPID=$!
cd "$ROOT"
trap 'kill "$DPID" 2>/dev/null || true; rm -rf "$DATA_DIR"' EXIT

ok=0
for _ in $(seq 1 60); do
  if curl -sf "http://127.0.0.1:${PORT}/" >/dev/null; then
    ok=1
    break
  fi
  sleep 0.5
done
if [[ "$ok" != 1 ]]; then
  echo "::error::Daemon did not respond on port $PORT"
  exit 1
fi

curl -sf "http://127.0.0.1:${PORT}/" | grep -q '"status":"ok"' || { echo "::error::GET / body"; exit 1; }
curl -sf "http://127.0.0.1:${PORT}/api/status" | grep -q '"status":"ok"' || { echo "::error::GET /api/status"; exit 1; }
curl -sf "http://127.0.0.1:${PORT}/api/docs" | grep -q '"pages"' || { echo "::error::GET /api/docs index"; exit 1; }
curl -sf "http://127.0.0.1:${PORT}/api/docs/accueil" | grep -q '"content"' || { echo "::error::GET /api/docs/accueil"; exit 1; }

echo "Smoke OK: $STAGING_ABS (port $PORT)"
