#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UNIT_DIR=/etc/systemd/system

for credential in /etc/pirana/credentials/bitfinex_api_key /etc/pirana/credentials/bitfinex_api_secret; do
    if ! sudo test -s "$credential"; then
        echo "ERROR: required systemd credential is missing/empty: $credential" >&2
        echo "See docs/OPERATIONS.md -> Credential isolation. Service was NOT restarted." >&2
        exit 2
    fi
done

python3 "${ROOT}/scripts/strategy_versioning.py" validate
cargo build --release --locked --manifest-path "${ROOT}/Cargo.toml"

sudo install -m 0644 "${ROOT}/deploy/systemd/pirana.service" "${UNIT_DIR}/pirana.service"
sudo mkdir -p "${UNIT_DIR}/pirana.service.d"
for conf in "${ROOT}"/deploy/systemd/pirana.service.d/*.conf; do
    sudo install -m 0644 "${conf}" "${UNIT_DIR}/pirana.service.d/$(basename "${conf}")"
done

sudo systemctl daemon-reload
sudo systemctl enable pirana.service
sudo systemctl restart pirana.service
sudo systemctl --no-pager --full status pirana.service
