#!/usr/bin/env bash
# Bench hybrid memory recall (RRF + composite scores). Requires Rust toolchain.
set -euo pipefail
cd "$(dirname "$0")/.."
echo "Running akasha-store memory fusion tests (proxy for recall bench)..."
cargo test -p akasha-store memory_fusion hybrid_rrf --lib -- --nocapture
echo "Done. For full daemon integration bench, enable embeddings and compare AKASHA_MEMORY_RRF=0 vs 1."
