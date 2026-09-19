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

The canonical host gate is:

```sh
python3 scripts/postdeploy_gate.py --require-flat
```

It fails closed when any of these conditions is true:

- the service account can execute a generic command through passwordless sudo
  (for example an old `NOPASSWD:ALL` rule);
- Pirana endpoints 8080/9091/9100 are absent or any of 3000/8080/9090/9091/9100
  is bound outside loopback;
- operational accounting is incomplete/stale/invalid;
- operational epoch identity/reserve is invalid;
- authenticated/runtime BTC differs from quarantined reserve + operational lots;
- an execution block exists or the runtime is Halted;
- with `--require-flat`, an open order/position/pending intent remains.

Additional evidence:

```sh
systemctl is-active pirana.service
curl -fsS http://127.0.0.1:8080/api/health
curl -fsS http://127.0.0.1:9100/metrics >/dev/null
journalctl -u pirana.service -n 200 --no-pager
```

A restart, `Active`, or a complete reporting period is not proof of recovery.
Historical account accounting may legitimately remain `incomplete` when old
cost basis is unknown; trading safety is determined by the separate fresh
operational projection and exact wallet reconciliation.

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

All unattended privileged helpers use `sudo -n` so a missing/incorrect sudo
rule fails immediately instead of hanging on a password prompt. The Hermes
daily-audit unit itself is explicitly non-privileged
(`NoNewPrivileges=true`, `RestrictSUIDSGID=true`) and its prompt forbids
sudo/restart actions.

Installing `pirana-ops` does not neutralize an older broad sudo rule. A host
with `wwwenda ... NOPASSWD:ALL` or another generic passwordless-root path MUST
NOT be declared security-GREEN. `scripts/postdeploy_gate.py` verifies this by
requiring `sudo -n /usr/bin/true` to fail. Audit and remove/migrate the broad
host rule before final approval.

## Credential isolation

Production must not keep Bitfinex keys in the project `.env`.

Create root-only credential sources:

```sh
sudo install -d -m 0700 /etc/pirana/credentials
sudo install -m 0600 /dev/null /etc/pirana/credentials/bitfinex_api_key
sudo install -m 0600 /dev/null /etc/pirana/credentials/bitfinex_api_secret
sudoedit /etc/pirana/credentials/bitfinex_api_key
sudoedit /etc/pirana/credentials/bitfinex_api_secret

sudo install -d -m 0755 /etc/pirana
sudo install -m 0600 /dev/null /etc/pirana/telegram.env
sudoedit /etc/pirana/telegram.env
```

`/etc/pirana/telegram.env` contains only:

```text
TELEGRAM_BOT_TOKEN=...
TELEGRAM_CHAT_ID=...
```

`pirana.service` receives the exchange keys through systemd `LoadCredential=`
(unit-private credential mount). The Hermes daily-audit unit never loads those
credentials, cannot open the legacy project `.env` in its mount namespace, and
launches `hermes` with exchange and Telegram variables removed from its child
environment.

For local development only, `.env` loading is opt-in with
`PIRANA_LOAD_DOTENV=1`.

Before enabling the new units, remove Bitfinex keys from the project `.env`
after verifying the credential files. Do not delete the only working copy until
the new service starts and authenticated reconciliation succeeds.

## Network and secrets

Dashboard/API, Rust metrics, accounting exporter, Prometheus and Grafana are
expected to be loopback-only on the production host unless a separately audited
private/reverse-proxy boundary is documented. The post-deploy gate rejects
non-loopback listeners on ports 3000, 8080, 9090, 9091 and 9100.

Keep `.env` untracked and mode 0600. The public Git history historically
contained a Telegram bot token; any credential that appeared in history must be
revoked/rotated at its provider.

## Backups

Keep an off-host set containing the SQLite backup, `positions.json`, current
risk state, and exact Git/build hash. Periodically test restore in an isolated
directory.
