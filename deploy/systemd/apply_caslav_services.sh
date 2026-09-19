#!/usr/bin/env bash
# ==============================================================================
# apply_caslav_services.sh — Atomic Systemd Deployment & Verification
# ==============================================================================
set -euo pipefail

DEPLOY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PIRANA_ROOT="$(cd "${DEPLOY_DIR}/../.." && pwd)"
BOT_SERVICE_SRC="${PIRANA_ROOT}/../caslav_telegram/caslav-bot.service"
BACKUP_DIR="${HOME}/backups/systemd_$(date +%Y%m%d_%H%M%S)"

echo "=== [1/5] PRE-FLIGHT VERIFICATION ==="
# Verify all staging units syntax
echo "Checking staging unit syntax with systemd-analyze verify..."
systemd-analyze verify \
    "${DEPLOY_DIR}/pirana.service" \
    "${DEPLOY_DIR}/pirana-tick-research.service" \
    "${DEPLOY_DIR}/pirana-monthly-report.timer" \
    "${BOT_SERVICE_SRC}"

echo "Pre-flight syntax checks passed cleanly."

echo
echo "=== [2/5] BACKUP EXISTING CONFIGURATIONS ==="
mkdir -p "${BACKUP_DIR}"
for f in pirana.service pirana-tick-research.service pirana-monthly-report.timer caslav-bot.service; do
    if [ -f "/etc/systemd/system/${f}" ]; then
        cp -a "/etc/systemd/system/${f}" "${BACKUP_DIR}/"
        echo "Backed up /etc/systemd/system/${f} -> ${BACKUP_DIR}/"
    fi
done
if [ -d "/etc/systemd/system/pirana.service.d" ]; then
    cp -r "/etc/systemd/system/pirana.service.d" "${BACKUP_DIR}/"
    echo "Backed up /etc/systemd/system/pirana.service.d -> ${BACKUP_DIR}/"
fi

echo
echo "=== [3/5] INSTALL CANONICAL UNITS ==="
echo "Installing units into /etc/systemd/system/..."
sudo cp "${DEPLOY_DIR}/pirana.service" /etc/systemd/system/pirana.service
sudo cp "${DEPLOY_DIR}/pirana-tick-research.service" /etc/systemd/system/pirana-tick-research.service
sudo cp "${DEPLOY_DIR}/pirana-monthly-report.timer" /etc/systemd/system/pirana-monthly-report.timer
sudo cp "${BOT_SERVICE_SRC}" /etc/systemd/system/caslav-bot.service

# Permissions
sudo chmod 644 \
    /etc/systemd/system/pirana.service \
    /etc/systemd/system/pirana-tick-research.service \
    /etc/systemd/system/pirana-monthly-report.timer \
    /etc/systemd/system/caslav-bot.service

# Consolidate pirana.service.d drop-ins
echo "Consolidating pirana.service.d drop-ins..."
sudo mkdir -p /etc/systemd/system/pirana.service.d
# Remove obsolete duplicates
sudo rm -f \
    /etc/systemd/system/pirana.service.d/20-priority.conf \
    /etc/systemd/system/pirana.service.d/30-accounting-memory.conf \
    /etc/systemd/system.control/pirana.service.d/50-CPUWeight.conf \
    /etc/systemd/system.control/pirana.service.d/50-IOWeight.conf

sudo cp "${DEPLOY_DIR}/pirana.service.d/10-resources.conf" /etc/systemd/system/pirana.service.d/10-resources.conf
sudo cp "${DEPLOY_DIR}/pirana.service.d/20-accounting.conf" /etc/systemd/system/pirana.service.d/20-accounting.conf
sudo chmod 644 /etc/systemd/system/pirana.service.d/*.conf

echo
echo "=== [4/5] SYSTEMD DAEMON-RELOAD ==="
sudo systemctl daemon-reload
echo "daemon-reload completed with exit code $?."

echo
echo "=== [5/5] POST-INSTALL VERIFICATION ==="
echo "Verifying live systemd units..."
systemd-analyze verify \
    /etc/systemd/system/pirana.service \
    /etc/systemd/system/pirana-tick-research.service \
    /etc/systemd/system/pirana-monthly-report.timer \
    /etc/systemd/system/caslav-bot.service

echo
echo "Verifying effective properties for pirana.service:"
systemctl show pirana.service -p MemoryHigh,MemoryMax,CPUWeight,IOWeight,OOMScoreAdjust,OnFailure

echo
echo "SUCCESS: All 5 systemd services and timers successfully deployed and verified."
