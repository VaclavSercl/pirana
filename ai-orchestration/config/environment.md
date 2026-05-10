# PIRANA Configuration

## Environment Variables

### Exchange (Bitfinex)
- `BITFINEX_API_KEY` — API key (WITHDRAWALS DISABLED)
- `BITFINEX_API_SECRET` — API secret
- `PIRANA_TESTNET` — Set to "true" for testnet mode

### Risk Overrides (optional)
- `MAX_AGGREGATE_EXPOSURE` — Default: 0.20
- `MAX_SINGLE_TRADE_RISK` — Default: 0.005
- `MAX_DAILY_DRAWDOWN` — Default: 0.03
- `MAX_WEEKLY_DRAWDOWN` — Default: 0.07
- `CONSECUTIVE_LOSS_THRESHOLD` — Default: 5

### Infrastructure
- `METRICS_PORT` — Default: 9090
- `HEALTH_CHECK_PORT` — Default: 8080
- `LOG_LEVEL` — Default: info
- `PIRANA_ENV` — Default: production

## Security Requirements
1. API keys MUST have withdrawals DISABLED
2. IP whitelisting MUST be configured
3. Keys MUST be rotated periodically
4. Hermes NEVER has direct access to keys
5. Keys stored only in environment variables, never in code
