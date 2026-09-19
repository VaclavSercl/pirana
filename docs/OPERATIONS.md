# PIRANA production operations

## Source of truth

A production binary must map to a Git commit that exists on GitHub. A draft pull
request is a review/staging boundary, not the canonical mainline. If a draft
candidate is temporarily deployed, record the exact commit and binary hash.

Before and after deployment:

```sh
git status --short --branch
git rev-parse HEAD
sha256sum target/release/pirana
```

## Required validation

```sh
cargo check --locked --all-targets
cargo test --locked --workspace
cargo clippy --locked --all-targets -- -D warnings
python3 -m pytest -q
python3 scripts/strategy_versioning.py validate
cargo build --locked --release
```

Back up canonical accounting through the SQLite backup helper in
`docs/accounting-recovery.md`, and preserve `positions.json`. Never replace a
newer accounting DB with an older copy after new exchange executions.

## Tracked systemd schedules

The repository now contains the base units required by the existing resource
drop-ins. Canonical schedules that are explicitly documented in source/history:

- `pirana-recalib.timer`: daily 06:00 local time
- `pirana-daily-check.timer`: daily 07:00 local time
- `pirana-weekly-audit.timer`: Monday 06:00 local time
- `pirana-monthly-proposal.timer`: first day of month 10:00 local time
- `pirana-yearly-report.timer`: January 1 at 09:00 local time

`pirana-monthly-report.service` is tracked, but no timer is invented here:
the repository does not establish its historical clock time. Capture/verify the
live host timer before codifying or replacing it.

## Post-deploy gates

```sh
systemctl is-active pirana.service
curl -fsS http://127.0.0.1:8080/api/health
curl -fsS http://127.0.0.1:8080/api/accounting | jq
curl -fsS http://127.0.0.1:8080/api/snapshot | jq '.system_mode,.execution_block_reason'
curl -fsS http://127.0.0.1:9100/metrics >/dev/null
journalctl -u pirana.service -n 200 --no-pager
```

A restart is not proof of reconciliation. Do not declare recovery successful
while canonical accounting is incomplete, an execution block remains, or the
runtime is unexpectedly Halted.

## Least-privilege control plane

Automated components (Hermes daily audit, Caslav doctor, Telegram control bot)
must never receive blanket passwordless sudo. The repository contains
`deploy/sudoers/pirana-ops`, which permits only:

- `systemctl start pirana.service`
- `systemctl stop pirana.service`
- `systemctl restart pirana.service`

Validate and install it explicitly on the host:

```sh
sudo visudo -cf deploy/sudoers/pirana-ops
sudo install -m 0440 deploy/sudoers/pirana-ops /etc/sudoers.d/pirana-ops
```

All unattended callers use `sudo -n` so a missing/incorrect sudo rule fails
immediately instead of hanging on a password prompt. Installing this file does
not remove any older broad sudo rules; audit `/etc/sudoers` and
`/etc/sudoers.d/` separately on the live host.

## Network and secrets

Dashboard/API, Rust metrics and the Python exporter bind to loopback by default.
Any external exposure must be explicitly protected by the host firewall/private
network/reverse proxy.

Keep `.env` untracked and mode 0600. The public Git history historically
contained a Telegram bot token; any credential that appeared in history must be
revoked/rotated at its provider.

## Backups

Keep an off-host set containing the SQLite backup, `positions.json`, current
risk state, and exact Git/build hash. Periodically test restore in an isolated
directory.
