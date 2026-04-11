# Multi-stage build for shell-node with RocksDB + libp2p
FROM rust:1.93-bookworm AS builder

# Install RocksDB build dependencies
RUN apt-get update && apt-get install -y \
    clang libclang-dev llvm-dev \
    cmake pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .

RUN cargo build --release -j 1 -p shell-cli --features "rocksdb,libp2p"

# Runtime image
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates curl jq \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash shelluser

COPY --from=builder /build/target/release/shell-node /usr/local/bin/shell-node
COPY docker/entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENV DATADIR=/data
ENV SHARED=/shared

RUN mkdir -p /data /shared && chown shelluser:shelluser /data /shared

USER shelluser

EXPOSE 8545 30303 9090

HEALTHCHECK --interval=10s --timeout=3s --retries=3 \
    CMD curl -sf http://localhost:9090/health || exit 1

ENTRYPOINT ["/entrypoint.sh"]
