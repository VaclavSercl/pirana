#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UNIT_DIR=/etc/systemd/system

sudo install -m 0644 "${ROOT}/deploy/systemd/pirana-exporter.service" "${UNIT_DIR}/pirana-exporter.service"
sudo mkdir -p "${UNIT_DIR}/pirana-exporter.service.d"
for conf in "${ROOT}"/deploy/systemd/pirana-exporter.service.d/*.conf; do
    sudo install -m 0644 "${conf}" "${UNIT_DIR}/pirana-exporter.service.d/$(basename "${conf}")"
done

sudo systemctl daemon-reload
sudo systemctl enable pirana-exporter.service
sudo systemctl restart pirana-exporter.service
sudo systemctl --no-pager --full status pirana-exporter.service
