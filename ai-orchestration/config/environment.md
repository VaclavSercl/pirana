# PIRANA Configuration

## Environment Variables

### Exchange (Bitfinex)
- `BITFINEX_API_KEY` — API key (WITHDRAWALS DISABLED)
- `BITFINEX_API_SECRET` — API secret
- `PIRANA_TESTNET` — Set to "true" for testnet mode

### Risk source of truth
Runtime risk is **not** configured by `MAX_*` environment variables.

- Hard ceilings: `crates/pirana-core/src/constants.rs`
- Active strategy constraints: `strategy.toml`
- Calibrated/persisted state: `/opt/caslav/risk/risk_state.json`

Historical `MAX_AGGREGATE_EXPOSURE`, `MAX_SINGLE_TRADE_RISK`,
`MAX_DAILY_DRAWDOWN`, `MAX_WEEKLY_DRAWDOWN`, and
`CONSECUTIVE_LOSS_THRESHOLD` environment variables are accepted only as
legacy leftovers and are explicitly ignored by the trading runtime.

### Infrastructure
- `PIRANA_RUST_METRICS_PORT` — Default: 9100
- `HEALTH_CHECK_PORT` — Default: 8080
- `PIRANA_DASHBOARD_BIND` — Default: `127.0.0.1`; set `0.0.0.0` only behind an explicit network boundary
- `PIRANA_METRICS_BIND` — Default: `127.0.0.1`
- `LOG_LEVEL` — Default: info
- `PIRANA_ENV` — Default: production

## Security Requirements
1. API keys MUST have withdrawals DISABLED
2. IP whitelisting MUST be configured
3. Keys MUST be rotated periodically
4. Hermes must not be granted direct secret-reading permissions; this is an OS/service-policy requirement, not guaranteed by repository structure alone
5. Keys stored only in environment variables, never in code
