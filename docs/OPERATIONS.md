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
