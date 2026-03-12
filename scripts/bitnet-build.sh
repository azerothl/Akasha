#!/usr/bin/env bash
# Build BitNet llama-server (for testing Akasha BitNet provider).
# Usage: ./bitnet-build.sh [BITNET_DIR]
#   BITNET_DIR: where to clone/build BitNet (default: ./BitNet next to this script).
# Prereqs: git, cmake, clang (or gcc), make/ninja.
# After build: see spec/runbooks/03_bitnet_server_setup.md to download a model and run the server.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PARENT_DIR="$(dirname "$SCRIPT_DIR")"
BITNET_DIR="${1:-$PARENT_DIR/BitNet}"

if [[ -d "$BITNET_DIR" && ! -f "$BITNET_DIR/CMakeLists.txt" ]]; then
    echo "Error: $BITNET_DIR exists but is not a BitNet repo (no CMakeLists.txt)." >&2
    exit 1
fi

if [[ ! -d "$BITNET_DIR" ]]; then
    echo "Cloning BitNet into $BITNET_DIR ..."
    git clone --recursive https://github.com/microsoft/BitNet.git "$BITNET_DIR"
else
    echo "Using existing BitNet at $BITNET_DIR"
    (cd "$BITNET_DIR" && git submodule update --init --recursive)
fi

cd "$BITNET_DIR"
BUILD_DIR="build"

# BitNet requires include/bitnet-lut-kernels.h to be generated before CMake (see setup_env.py gen_code()).
if [[ ! -f "include/bitnet-lut-kernels.h" ]]; then
    echo "Generating include/bitnet-lut-kernels.h (required before CMake) ..."
    if ! command -v python3 &>/dev/null && ! command -v python &>/dev/null; then
        echo "Error: Python is required to generate bitnet-lut-kernels.h. Run: python setup_env.py -md models/BitNet-b1.58-2B-4T -q i2_s" >&2
        exit 1
    fi
    PYTHON=$(command -v python3 2>/dev/null || command -v python)
    ARCH=$(uname -m)
    if [[ "$ARCH" == "aarch64" || "$ARCH" == "arm64" ]]; then
        "$PYTHON" utils/codegen_tl1.py --model bitnet_b1_58-3B --BM 160,320,320 --BK 64,128,64 --bm 32,64,32
    else
        "$PYTHON" utils/codegen_tl2.py --model bitnet_b1_58-3B --BM 160,320,320 --BK 96,96,96 --bm 32,32,32
    fi
    if [[ ! -f "include/bitnet-lut-kernels.h" ]]; then
        echo "Error: Codegen did not create include/bitnet-lut-kernels.h. Try: python setup_env.py -md models/BitNet-b1.58-2B-4T -q i2_s" >&2
        exit 1
    fi
fi

echo "Configuring and building (Release) ..."
cmake -S . -B "$BUILD_DIR" -DCMAKE_BUILD_TYPE=Release
cmake --build "$BUILD_DIR" -j

EXE="$BUILD_DIR/bin/llama-server"
if [[ -x "$EXE" ]]; then
    echo "Build OK. Executable: $BITNET_DIR/$EXE"
    echo ""
    echo "Next steps:"
    echo "  1) Download a model, e.g.:"
    echo "     huggingface-cli download microsoft/BitNet-b1.58-2B-4T-gguf --local-dir $BITNET_DIR/models/BitNet-b1.58-2B-4T"
    echo "  2) Start the server:"
    echo "     $BITNET_DIR/$EXE -m $BITNET_DIR/models/BitNet-b1.58-2B-4T/ggml-model-i2_s.gguf --host 127.0.0.1 --port 8080 -c 2048 -t 2 -cb"
    echo "  3) In llm_router.yaml set provider bitnet with base_url http://127.0.0.1:8080 and use task_types.conversation.primary.provider: bitnet"
    echo ""
    echo "See spec/runbooks/03_bitnet_server_setup.md for full runbook."
else
    echo "Error: executable not found at $EXE" >&2
    exit 1
fi
