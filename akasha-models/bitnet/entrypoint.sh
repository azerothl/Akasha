#!/bin/bash
set -e
# BitNet llama-server: port 8080. Model = first .gguf in BITNET_MODEL_PATH or /models.
MODEL_PATH="${BITNET_MODEL_PATH:-/models}"
GGUF=$(find "$MODEL_PATH" -name "*.gguf" 2>/dev/null | head -1)
if [ -z "$GGUF" ]; then
  echo "No GGUF in $MODEL_PATH. Mount a volume: -v /path/to/models:/models (see README)."
  exit 1
fi
exec /BitNet/build/bin/llama-server -m "$GGUF" --host 0.0.0.0 --port 8080 -c 2048 -t ${BITNET_THREADS:-2} -cb
