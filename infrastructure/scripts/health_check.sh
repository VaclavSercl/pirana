#!/bin/bash
# PIRANA — Health Check Script
set -euo pipefail

HEALTH_URL="${1:-http://127.0.0.1:8080/api/health}"
METRICS_URL="${2:-http://127.0.0.1:9100/metrics}"
COMPOSE_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../docker/docker-compose.yml"

fail=0
echo "PIRANA Health Check"
echo "==================="

echo -n "Health endpoint: "
if curl -fsS "${HEALTH_URL}" >/dev/null; then
    echo "✓ OK"
else
    echo "✗ FAIL"
    fail=1
fi

echo -n "Rust metrics endpoint: "
if curl -fsS "${METRICS_URL}" >/dev/null; then
    echo "✓ OK"
else
    echo "✗ FAIL"
    fail=1
fi

echo
echo "Container Status:"
docker compose -f "${COMPOSE_FILE}" ps || fail=1

exit "${fail}"
