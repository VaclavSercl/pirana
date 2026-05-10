#!/bin/bash
# PIRANA — Health Check Script

set -euo pipefail

HEALTH_URL="${1:-http://localhost:8080/health}"
METRICS_URL="${2:-http://localhost:9090/metrics}"

echo "PIRANA Health Check"
echo "==================="

# Health endpoint
echo -n "Health endpoint: "
if curl -sf "${HEALTH_URL}" 2>/dev/null; then
    echo "✓ OK"
else
    echo "✗ FAIL"
fi

# Metrics endpoint
echo -n "Metrics endpoint: "
if curl -sf "${METRICS_URL}" >/dev/null 2>&1; then
    echo "✓ OK"
else
    echo "✗ FAIL"
fi

# Docker containers
echo ""
echo "Container Status:"
docker compose -f "$(dirname "$0")/../docker/docker-compose.yml" ps 2>/dev/null || echo "Docker Compose not available"
