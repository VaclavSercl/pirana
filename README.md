# PIRANA
## Institutional Hybrid AI-Orchestrated Quantitative Trading System
### Hermes Agent + Deterministic Ultra-Low-Latency Execution Architecture

---

## CRITICAL ARCHITECTURAL PRINCIPLE

Large language models are probabilistic inference engines.
Financial execution systems require deterministic behavior.
These two domains MUST be separated.

Therefore:
- Hermes performs reasoning
- deterministic systems perform execution
- risk engines enforce governance
- infrastructure layers preserve integrity
- custody remains isolated from AI

This separation is mandatory.

---

## WHAT PIRANA IS

PIRANA is:
- a quantitative orchestration engine
- a hybrid AI-assisted trading architecture
- a market microstructure intelligence system
- a volatility-aware execution framework
- a deterministic low-latency trading infrastructure
- an adaptive BTC accumulation platform

PIRANA is NOT:
- a magic profit generator
- a social-media sentiment trader
- a retail RSI bot
- a fully autonomous hedge fund
- a direct-LLM execution system

---

## THE HYBRID MODEL

### HFT Layer
- order routing, spread capture, market making
- queue positioning, latency arbitration, execution timing
- Implemented in **Rust**

### AI Layer
- regime classification, structural analysis, probabilistic reasoning
- volatility adaptation, signal weighting, parameter optimization
- Implemented using **Hermes Agent**

The AI layer NEVER participates directly inside the microsecond execution loop.

---

## EXCHANGE

**Bitfinex** — all trading operations execute exclusively on Bitfinex.

---

## TRADING STRATEGY

### Order Flow Imbalance (OFI)

The primary signal engine. OFI measures directional order flow pressure:

```
OFI_t = I(P_t > P_{t-1}) * V_t - I(P_t < P_{t-1}) * V_t
```

Normalized to [-1, +1] over a rolling window.

- **BUY trigger**: OFI > `ofi_trigger_threshold` (default: 0.85)
- **SELL trigger**: OFI < -`ofi_trigger_threshold`

The threshold is **configurable** via `strategy.toml` and is properly passed to the OfiCalculator at runtime.

### Order Execution

- **LIMIT orders** (EXCHANGE LIMIT) — qualifies for **maker fee** (0.10% on Bitfinex Tier 0)
- Orders are submitted asynchronously via `tokio::spawn` for non-blocking execution
- Position tracking, balance updates, and risk engine updates happen **synchronously before** the async order submission to prevent race conditions
- Rollback logic on order failure: position, balance, and exposure are reverted

### Position Management

- BUY positions are tracked in-memory **before** the async order is submitted
- SELL orders require an existing BUY position — **naked shorts are prevented**
- TP/SL monitoring runs on every ticker update
- If no BUY position exists when SELL signal fires, the trade is **skipped** (not executed)

### Risk Management

- Maximum Aggregate Exposure: **20%**
- Maximum Single Trade Risk: **0.50%**
- Maximum Daily Drawdown: **3%**
- Maximum Weekly Drawdown: **7%**
- Position size is dynamically scaled to fit within exposure budget
- 5 consecutive losses → **Defensive Mode** (50% position size)
- 10 consecutive losses → **Halted** (paper trading until 5 consecutive paper wins)

### Defensive Protocol
If 5 consecutive losses occur, abnormal volatility appears, exchange instability detected, or API degradation emerges:
- aggressive strategies halt
- exposure reduces automatically
- AI enters DEFENSIVE MODE
- human review required

---

## CONFIGURATION

### strategy.toml

```toml
[strategy]
take_profit_distance_usd = 15.0      # TP distance from entry
stop_loss_distance_usd = 25.0        # SL distance from entry
ofi_trigger_threshold = 0.85         # OFI threshold (higher = fewer trades)
ofi_window_size = 100                # Rolling window for OFI calculation
trade_cooldown_ms = 10000            # Min time between trades (10s)
min_confidence_score = 0.95          # Minimum signal confidence

[risk_management]
position_size_pct = 2.0              # % of portfolio per trade
daily_loss_limit_usd = 1000.0
max_slippage_bps = 5
```

The `strategy.toml` is **hot-reloadable** — changes take effect within `reload_interval_seconds` without restart.

### Environment Variables (.env)

```
BITFINEX_API_KEY=xxx                 # API key (withdrawals DISABLED)
BITFINEX_API_SECRET=xxx              # API secret
PIRANA_ENV=production                # production | staging | development
LOG_LEVEL=info                       # trace | debug | info | warn | error
```

---

## DASHBOARD

Real-time web dashboard with:

- Live BTC price chart
- P&L history chart
- Order book visualization (top 25 levels, live from Bitfinex WebSocket)
- Recent trades list (last 100)
- Recent signals list (last 50)
- System mode indicator (Active / Defensive / Halted)
- Risk metrics (exposure, drawdown, consecutive losses, win rate)

### Endpoints

| Endpoint | URL |
|---|---|
| Dashboard | `http://localhost:8080` |
| Trading UI | `http://localhost:8080/trading` |
| API Snapshot | `http://localhost:8080/api/snapshot` |
| WebSocket | `ws://localhost:8080/ws` |
| Health Check | `http://localhost:8080/api/health` |
| Prometheus Metrics | `http://localhost:9091/metrics` |

---

## SECURITY

- Exchange keys: withdrawals DISABLED, IP whitelisting, periodic rotation
- Keys remain inaccessible to Hermes
- API secrets use `zeroize` for memory safety
- `#[serde(skip_serializing)]` prevents key leakage in logs
- Isolated infrastructure, immutable logs, read-only containers
- Outbound firewall restrictions

---

## DEPLOYMENT

### systemd Services

```bash
# Build
cd /home/wwwenda/workspace/pirana
cargo build --release

# Services (auto-start on boot, auto-restart on crash)
sudo systemctl restart pirana.service
sudo systemctl restart pirana-exporter.service

# Check status
systemctl status pirana.service
journalctl -u pirana.service -f
```

### Service Configuration

```ini
# /etc/systemd/system/pirana.service
[Unit]
Description=Pirana HFT Bot
After=network-online.target

[Service]
Type=simple
User=wwwenda
WorkingDirectory=/home/wwwenda/workspace/pirana
EnvironmentFile=/home/wwwenda/workspace/pirana/.env
ExecStart=/home/wwwenda/workspace/pirana/target/release/pirana
Restart=always
RestartSec=3
LimitNOFILE=65535
```

---

## PROJECT STRUCTURE

```
pirana/
├── src/main.rs                         # Main loop — WS feed, OFI, execution, TP/SL
├── crates/
│   ├── pirana-core/                    # Types, constants, errors, order book
│   ├── pirana-config/                  # .env loading, PiranaConfig, validation
│   ├── pirana-market-data/             # Bitfinex WebSocket, REST, OrderBookManager
│   ├── pirana-features/                # OFI, volatility, liquidity, volume profile
│   ├── pirana-signal-validator/        # Signal validation & governance
│   ├── pirana-risk-engine/             # Risk limits, exposure, drawdown controls
│   ├── pirana-execution/               # BitfinexClient (HMAC-SHA384), OrderRouter
│   ├── pirana-dashboard/               # Web UI, API, WebSocket, static HTML
│   └── pirana-telemetry/               # Prometheus metrics, tracing
├── ai-orchestration/                   # Hermes AI layer
│   ├── prompts/                        # System prompts
│   ├── skills/                         # Hermes skills
│   └── config/                         # AI configuration
├── infrastructure/
│   ├── docker/                         # Docker Compose (engine, prometheus, grafana, loki)
│   └── monitoring/                     # Prometheus config
├── strategy.toml                       # Active strategy configuration
├── pirana_exporter.py                  # Prometheus exporter
├── docs/                               # Documentation
└── tests/                              # Integration & unit tests
```

---

## MONITORING

- **Prometheus**: metrics collection (port 9091)
- **Grafana**: dashboards (port 3000)
- **Loki**: log aggregation (port 3100)
- **Nginx**: reverse proxy on port 80

---

## CHANGELOG

### 2026-07-10: Critical Fixes

- **OFI threshold fix**: `ofi_trigger_threshold` from `strategy.toml` is now properly passed to `OfiCalculator` instead of using hardcoded `OFI_THRESHOLD = 0.6` constant
- **Position tracking fix**: BUY positions, balance updates, and exposure updates now happen **synchronously before** `tokio::spawn` to eliminate race conditions where SELL arrived before BUY was registered
- **Naked short prevention**: SELL orders are **skipped** if no open BUY position exists, instead of executing and logging a warning
- **LIMIT orders**: Switched from `EXCHANGE MARKET` (taker fee 0.20%) to `EXCHANGE LIMIT` (maker fee 0.10%) — saves 50% on fees
- **Win rate calculation**: Now properly updated on every SELL trade (was hardcoded 0.0)
- **Order book processing**: Bitfinex book channel data is now parsed and stored in `DashboardState.order_book` (was empty)
- **TP/SL realism**: Adjusted from $350/$150 to $15/$25 — achievable within the 10s trade interval
- **Cooldown increased**: From 5s to 10s to reduce trade frequency and fee impact
- **Position size reduced**: From 5% to 2% to prevent exposure exhaustion
- **Rollback on failure**: If Bitfinex rejects an order, position/balance/exposure are reverted
- **Safe BTC check**: SELL size is capped to 99% of available BTC balance to prevent "not enough balance" errors

---

## LICENSE

Proprietary — All rights reserved.
