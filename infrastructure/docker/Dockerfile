# PIRANA — reproducible multi-stage image
FROM rust:1.85.1-slim-bookworm AS builder

WORKDIR /build
# Keep release builds within the ARM host memory budget.
ARG CARGO_BUILD_JOBS=1
RUN apt-get update && apt-get install -y --no-install-recommends \
    make pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
COPY src/ ./src/
COPY strategy.toml ./
COPY scripts/pirana_accounting.py ./scripts/pirana_accounting.py

RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl python3 gzip \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /usr/sbin/nologin pirana \
    && mkdir -p /var/lib/pirana /opt/caslav/risk /app/scripts \
    && chown -R pirana:pirana /var/lib/pirana /opt/caslav/risk /app

WORKDIR /app
COPY --from=builder /build/target/release/pirana /app/pirana
COPY --from=builder /build/strategy.toml /app/strategy.toml
COPY --from=builder /build/scripts/pirana_accounting.py /app/scripts/pirana_accounting.py
COPY --from=builder /build/crates/pirana-dashboard/static /app/crates/pirana-dashboard/static

USER pirana
EXPOSE 8080 9100

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8080/api/health || exit 1

ENTRYPOINT ["/app/pirana"]
