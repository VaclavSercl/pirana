#!/usr/bin/env bash
set -euo pipefail

# Deterministic read-only observation. The previous Hermes --yolo invocation
# could mutate strategy and publish the registry; neither belongs in reporting.
# Keep the installed service entrypoint and systemd Telegram environment.
WORKSPACE_DIR="/home/wwwenda/workspace/pirana"
exec python3 "${WORKSPACE_DIR}/scripts/send_recalibration_report.py" --daily-audit "$@"
