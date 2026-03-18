# BitNet (Microsoft 1-bit LLM) — build from source and run llama-server on port 8080.
# Build is long (~20–40 min). Model can be downloaded at build time or mounted at run time.
# See https://github.com/microsoft/BitNet
FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    cmake \
    clang-18 \
    git \
    python3 \
    python3-pip \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

ENV CC=clang-18
ENV CXX=clang++-18

WORKDIR /BitNet
# Clone with submodules (required for llama.cpp)
RUN git clone --recursive --depth 1 https://github.com/microsoft/BitNet.git . \
    || (git clone https://github.com/microsoft/BitNet.git . && git submodule update --init --recursive)

RUN pip install --no-cache-dir -r requirements.txt

# Generate bitnet-lut-kernels.h then build (no model download; mount model at run time).
RUN python3 utils/codegen_tl2.py --model bitnet_b1_58-3B --BM 160,320,320 --BK 96,96,96 --bm 32,32,32
RUN cmake -S . -B build -DCMAKE_BUILD_TYPE=Release && cmake --build build -j$(nproc)

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

EXPOSE 8080
ENV BITNET_MODEL_PATH=/models
ENTRYPOINT ["/entrypoint.sh"]
