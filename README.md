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

## ENTERPRISE RISK GOVERNANCE

- Maximum Aggregate Exposure: **20%**
- Maximum Single Trade Risk: **0.50%**
- Maximum Daily Drawdown: **3%**
- Maximum Weekly Drawdown: **7%**

### DEFENSIVE PROTOCOL
If 5 consecutive losses occur, abnormal volatility appears, exchange instability detected, or API degradation emerges:
- aggressive strategies halt
- exposure reduces automatically
- AI enters DEFENSIVE MODE
- human review required

---

## SECURITY

- Exchange keys: withdrawals DISABLED, IP whitelisting, periodic rotation
- Keys remain inaccessible to Hermes
- Isolated infrastructure, immutable logs, read-only containers
- Outbound firewall restrictions

---

## PROJECT STRUCTURE

```
pirana/
├── crates/                  # Rust workspace
│   ├── pirana-core/         # Shared types, traits, constants
│   ├── pirana-market-data/  # Bitfinex WebSocket feed, order book
│   ├── pirana-features/     # OFI, volatility, liquidity metrics
│   ├── pirana-signal-validator/ # Signal validation & governance
│   ├── pirana-risk-engine/  # Risk limits, exposure controls
│   ├── pirana-execution/    # Order router, Bitfinex API
│   ├── pirana-config/       # Configuration management
│   └── pirana-telemetry/    # Metrics, logging, tracing
├── ai-orchestration/        # Hermes AI layer
│   ├── prompts/             # System prompts
│   ├── memory/              # Regime & volatility memory
│   ├── skills/              # Hermes skills
│   └── config/              # AI configuration
├── infrastructure/
│   ├── docker/              # Container definitions
│   ├── monitoring/          # Prometheus, Grafana, Loki
│   └── scripts/             # Deployment & ops scripts
├── docs/                    # Documentation
└── tests/                   # Integration & unit tests
```

---

## LICENSE

Proprietary — All rights reserved.
