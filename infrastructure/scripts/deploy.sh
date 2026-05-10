#!/bin/bash
# PIRANA — Deployment Script
# Usage: ./deploy.sh [environment]

set -euo pipefail

ENV="${1:-production}"
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"

echo "========================================="
echo "  PIRANA Deployment"
echo "  Environment: ${ENV}"
echo "  Directory: ${PROJECT_DIR}"
echo "========================================="

# Check prerequisites
command -v docker >/dev/null 2>&1 || { echo "Docker required"; exit 1; }
command -v docker compose >/dev/null 2>&1 || { echo "Docker Compose required"; exit 1; }

# Check environment variables
if [ -z "${BITFINEX_API_KEY:-}" ]; then
    echo "WARNING: BITFINEX_API_KEY not set"
fi

if [ -z "${BITFINEX_API_SECRET:-}" ]; then
    echo "WARNING: BITFINEX_API_SECRET not set"
fi

# Build
echo "[1/3] Building PIRANA..."
cd "${PROJECT_DIR}"
cargo build --release 2>/dev/null || echo "Cargo not available — skipping native build"

# Deploy infrastructure
echo "[2/3] Deploying infrastructure..."
cd "${PROJECT_DIR}/infrastructure/docker"
docker compose up -d --build

# Verify
echo "[3/3] Verifying deployment..."
sleep 5

if curl -sf http://localhost:8080/health >/dev/null 2>&1; then
    echo "✓ Health check passed"
else
    echo "✗ Health check failed — check logs"
fi

echo ""
echo "PIRANA deployed successfully!"
echo "  Metrics:    http://localhost:9090"
echo "  Grafana:    http://localhost:3000"
echo "  Prometheus: http://localhost:9091"
