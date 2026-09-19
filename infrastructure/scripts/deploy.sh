#!/bin/bash
# PIRANA — reproducible Docker deployment helper
set -euo pipefail

ENV="${1:-production}"
PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COMPOSE_FILE="${PROJECT_DIR}/infrastructure/docker/docker-compose.yml"

echo "========================================="
echo "  PIRANA Deployment"
echo "  Environment: ${ENV}"
echo "  Directory: ${PROJECT_DIR}"
echo "========================================="

command -v docker >/dev/null 2>&1 || { echo "Docker required"; exit 1; }
docker compose version >/dev/null 2>&1 || { echo "Docker Compose v2 required"; exit 1; }

test -f "${PROJECT_DIR}/Cargo.toml"
test -f "${PROJECT_DIR}/Cargo.lock"
test -f "${PROJECT_DIR}/strategy.toml"

if [ -z "${BITFINEX_API_KEY:-}" ] || [ -z "${BITFINEX_API_SECRET:-}" ]; then
    echo "ERROR: BITFINEX_API_KEY and BITFINEX_API_SECRET must be exported" >&2
    exit 2
fi

echo "[1/4] Validating strategy..."
python3 "${PROJECT_DIR}/scripts/strategy_versioning.py" validate

echo "[2/4] Building locked Rust workspace..."
cd "${PROJECT_DIR}"
cargo build --release --locked

echo "[3/4] Building and starting containers..."
docker compose -f "${COMPOSE_FILE}" up -d --build

echo "[4/4] Verifying health..."
for _ in $(seq 1 20); do
    if curl -fsS http://127.0.0.1:8080/api/health >/dev/null; then
        echo "✓ Health check passed"
        docker compose -f "${COMPOSE_FILE}" ps
        exit 0
    fi
    sleep 1
done

echo "✗ Health check failed" >&2
docker compose -f "${COMPOSE_FILE}" ps >&2 || true
docker compose -f "${COMPOSE_FILE}" logs --tail=100 pirana-engine >&2 || true
exit 1
