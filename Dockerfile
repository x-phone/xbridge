# ── Build stage ──
FROM rust:1.87-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release

# ── Runtime stage ──
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/xbridge /usr/local/bin/xbridge

EXPOSE 8080
EXPOSE 10000-20000/udp

HEALTHCHECK --interval=10s --timeout=3s --start-period=5s \
    CMD curl -sf http://localhost:8080/health || exit 1

ENTRYPOINT ["xbridge"]
CMD ["--config", "/etc/xbridge/config.yaml"]
