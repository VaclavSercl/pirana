# PIRANA

> ⚠️ **HISTORICKÝ DOKUMENT — SUPERSEDED**
>
> Tento dokument popisuje původní architektonický záměr (květen 2026).
> Závazným zdrojem je `ai-orchestration/prompts/CASLAV_MASTER_v5_1.md`.
> **Číselné risk limity uvedené níže NEPLATÍ** — jediným zdrojem pravdy je
> `/opt/caslav/risk/risk_state.toml` (viz §8.4 master promptu).
> Limity se odvozují ze vzorců, nejsou zapsané.


## Institutional Hybrid AI-Orchestrated Quantitative Trading System
### Hermes Agent + Deterministic Ultra-Low-Latency Execution Architecture

---

# EXECUTIVE SUMMARY

PIRANA is a hybrid institutional trading architecture designed to combine:

- deterministic ultra-low-latency execution
- market microstructure intelligence
- AI-assisted strategic orchestration
- quantitative signal generation
- institutional-grade risk governance
- adaptive market regime classification
- event-driven infrastructure

PIRANA is NOT a chatbot connected directly to an exchange.

PIRANA is NOT a retail trading bot.

PIRANA is NOT an unrestricted autonomous financial entity.

The system exists specifically to solve one of the largest architectural failures in modern “AI trading bots”:

granting stochastic language models direct control over deterministic financial execution.

This design eliminates that failure completely.

---

# CRITICAL ARCHITECTURAL PRINCIPLE

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

# WHAT PIRANA REALLY IS

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

# REALITY CHECK: HFT VS AI

Most systems marketed online as “AI HFT bots” are technically fraudulent.

True High-Frequency Trading requires:

- colocated infrastructure
- kernel bypass networking
- lock-free memory structures
- nanosecond timestamping
- deterministic runtimes
- FPGA acceleration
- exchange proximity hosting
- FIX connectivity
- hardware-level optimization
- C++ or Rust execution systems

Large language models cannot:

- trade in microseconds
- compete inside exchange matching engines
- maintain deterministic timing
- perform wire-speed order arbitration

Therefore PIRANA uses a hybrid model.

## THE HYBRID MODEL

### HFT Layer

Responsible for:

- order routing
- spread capture
- market making
- queue positioning
- latency arbitration
- execution timing
- slippage minimization

Implemented in:

- Rust
- optional C++ modules

### AI Layer

Responsible for:

- regime classification
- structural analysis
- probabilistic reasoning
- volatility adaptation
- signal weighting
- parameter optimization
- anomaly detection

Implemented using:

- Hermes Agent
- LangGraph
- MCP tools
- structured memory systems

The AI layer NEVER participates directly inside the microsecond execution loop.

The deterministic execution layer remains isolated.

---

# ENTERPRISE SYSTEM ARCHITECTURE

## SYSTEM TOPOLOGY

```text
                 ┌──────────────────────┐
                 │ Exchange Data Feeds  │
                 │ WebSocket / FIX      │
                 └──────────┬───────────┘
                            │
                            ▼
                 ┌──────────────────────┐
                 │ Market Data Engine   │
                 │ Tick Aggregation     │
                 │ Order Book Engine    │
                 └──────────┬───────────┘
                            │
                            ▼
                 ┌──────────────────────┐
                 │ Feature Pipeline     │
                 │ OFI / Volatility     │
                 │ Liquidity Metrics    │
                 └──────────┬───────────┘
                            │
            ┌───────────────┴──────────────┐
            ▼                               ▼
 ┌──────────────────────┐      ┌──────────────────────┐
 │ Deterministic HFT    │      │ Hermes AI Layer      │
 │ Execution Runtime    │      │ Strategic Reasoning  │
 └──────────┬───────────┘      └──────────┬───────────┘
            │                              │
            └───────────────┬──────────────┘
                            ▼
                 ┌──────────────────────┐
                 │ Signal Validator     │
                 │ Governance Layer     │
                 └──────────┬───────────┘
                            │
                            ▼
                 ┌──────────────────────┐
                 │ Risk Engine          │
                 │ Exposure Controls    │
                 └──────────┬───────────┘
                            │
                            ▼
                 ┌──────────────────────┐
                 │ Execution Gateway    │
                 │ Order Router         │
                 └──────────┬───────────┘
                            │
                            ▼
                 ┌──────────────────────┐
                 │ Exchange API / FIX   │
                 └──────────────────────┘
```

---

# TECHNOLOGY STACK

## AI ORCHESTRATION LAYER

Primary orchestration framework:

- Hermes Agent

Reasoning models:

- Claude Sonnet
- DeepSeek R1
- local reasoning fallback models

Cognitive infrastructure:

- LangGraph
- MCP Tools
- structured memory graphs
- event-driven tool routing

---

## EXECUTION LAYER

Primary languages:

- Rust
- optional C++ latency-critical modules

Responsibilities:

- order placement
- cancellation arbitration
- queue optimization
- spread capture
- slippage management
- inventory balancing
- exchange reconciliation

---

## MARKET DATA LAYER

Realtime feeds:

- Binance WebSocket
- Bybit streams
- Coinbase Advanced Trade
- Hyperliquid feeds
- FIX protocol optional

Storage systems:

- Redis
- TimescaleDB
- ClickHouse

---

## INFRASTRUCTURE

Deployment:

- isolated VPS cluster
- Docker containers
- optional Kubernetes
- Tailscale zero-trust mesh

Monitoring:

- Prometheus
- Grafana
- Loki
- OpenTelemetry

---

# EVENT-DRIVEN ARCHITECTURE

PIRANA is NOT cron-driven.

Polling every 15 minutes is architecturally unacceptable for quantitative systems.

PIRANA is fully event-driven.

## EVENT FLOW

```text
Market Tick Arrives
        ↓
Order Book Update
        ↓
Feature Extraction
        ↓
Volatility Classification
        ↓
Hermes Evaluation
        ↓
Signal Probability Score
        ↓
Validation Engine
        ↓
Risk Evaluation
        ↓
Execution Decision
        ↓
Order Routing
```

The Hermes layer activates ONLY when:

- volatility anomalies emerge
- OFI thresholds break
- liquidation cascades appear
- liquidity compression occurs
- structural inefficiencies are detected

This dramatically reduces noise and inference waste.

---

# QUANTITATIVE EDGE

Retail indicators alone do not create sustainable edge.

Indicators such as:

- RSI
- MACD
- simplistic VWAP usage

are insufficient.

PIRANA instead evaluates market microstructure.

---

# CORE FEATURE ENGINE

## ORDER FLOW IMBALANCE (OFI)

The primary OFI evaluation model:

genui{"math_block_widget_always_prefetch_v2":{"content":"OFI_t = I(P_t > P_{t-1})V_t^b - I(P_t < P_{t-1})V_t^a"}}

Where:

- I = indicator function
- V_t^b = bid-side volume
- V_t^a = ask-side volume

This enables detection of:

- aggressive liquidity pressure
- directional imbalance
- forced liquidation momentum
- hidden accumulation

---

## ADDITIONAL MICROSTRUCTURE FEATURES

### Liquidity Delta

Measures velocity of limit order insertion/removal.

### Realized Volatility Clustering

Detects transition into high-risk expansion regimes.

### Funding Rate Pressure

Identifies over-leveraged derivatives environments.

### Liquidation Heatmaps

Detects forced liquidation clusters.

### Queue Position Dynamics

Tracks order priority decay.

### Cross-Exchange Spread Analysis

Identifies temporary structural inefficiencies.

### Volume Profile Analysis

Detects high-liquidity support and resistance zones.

---

# ENTERPRISE RISK GOVERNANCE

## ABSOLUTE PRINCIPLE

Capital preservation overrides profit generation.

Always.

---

# HARD RISK LIMITS

The AI layer MAY NOT:

- withdraw funds
- transfer assets
- bypass exposure controls
- disable stop systems
- modify exchange permissions
- exceed position limits
- access infrastructure secrets
- directly execute exchange calls

---

# POSITION LIMITS

## Maximum Aggregate Exposure

- 20%

## Maximum Single Trade Risk

- 0.50%

## Maximum Daily Drawdown

- 3%

## Maximum Weekly Drawdown

- 7%

---

# DEFENSIVE PROTOCOL

If:

- 5 consecutive losses occur
- abnormal volatility appears
- exchange instability detected
- API degradation emerges

Then:

- aggressive strategies halt
- exposure reduces automatically
- AI enters DEFENSIVE MODE
- human review required

---

# LATENCY ARCHITECTURE

Microsecond execution exists ONLY inside deterministic infrastructure.

The Hermes layer is strategically asynchronous.

---

# LOW-LATENCY REQUIREMENTS

The deterministic runtime uses:

- Rust async runtime
- lock-free queues
- memory pooling
- binary websocket parsing
- NUMA-aware affinity
- realtime Linux kernel
- preallocated buffers
- optimized TCP networking

Optional advanced extensions:

- DPDK kernel bypass
- FPGA acceleration
- hardware timestamping
- colocated exchange infrastructure

---

# SECURITY ARCHITECTURE

## ZERO-TRUST PRINCIPLE

PIRANA MUST NEVER:

- run on personal desktop systems
- share infrastructure with personal services
- expose plaintext secrets
- allow unrestricted shell access

---

# REQUIRED SECURITY

- isolated VMs
- immutable logs
- read-only containers
- hardware-encrypted secrets
- outbound firewall restrictions
- signed deployments
- infrastructure segmentation

---

# EXCHANGE KEY SECURITY

Exchange keys MUST:

- disable withdrawals
- disable internal transfers
- use IP whitelisting
- rotate periodically
- remain inaccessible to Hermes

---

# MASTER SYSTEM PROMPT

## PIRANA HERMES ORCHESTRATION PROMPT

```text
# CORE DIRECTIVE & GOVERNANCE

You are PIRANA.

You are the Hermes Quantitative Orchestration Engine operating inside a hybrid institutional trading architecture.

You are NOT:
- a retail trading chatbot
- a discretionary gambler
- a sentiment trader
- an unrestricted autonomous executor
- a direct exchange execution system

Your purpose is:
- market microstructure analysis
- volatility regime classification
- liquidity detection
- probabilistic signal generation
- adaptive strategy optimization
- drawdown minimization
- long-term BTC-denominated capital growth

You function as a probabilistic market intelligence layer.

You do NOT:
- directly execute trades
- hold custody of assets
- access exchange secrets
- override deterministic safeguards
- bypass risk engines
- disable stop systems
- escalate privileges
- perform unrestricted autonomous actions

## PERMITTED COGNITIVE OPERATIONS

1. Regime Classification
Analyze macro and microstructural conditions to classify the active market regime.

2. Signal Generation
Generate high-probability directional or accumulation signals.

3. Parameter Optimization
Adjust stop-loss, take-profit, inventory weighting, and volatility thresholds dynamically.

4. Structural Analysis
Identify liquidity vacuums, liquidation cascades, spread anomalies, and asymmetric opportunity zones.

## REQUIRED REASONING INPUTS

You reason using:
- order flow imbalance
- liquidity delta
- realized volatility
- spread compression
- liquidation pressure
- volume profile
- funding rate structure
- queue dynamics
- cross-exchange inefficiencies

## OUTPUT REQUIREMENTS

All outputs MUST:
- remain emotionally neutral
- remain mathematically justified
- use structured reasoning
- produce strict JSON output
- include confidence scoring
- include invalidation logic
- remain volatility-aware

## HARD CONSTRAINTS

You cannot:
- directly place orders
- directly modify infrastructure
- directly change production systems
- bypass governance
- exceed risk thresholds
- access custody systems

All generated outputs MUST pass through:
1. Validation Engine
2. Governance Engine
3. Risk Engine
4. Deterministic Execution Layer

Your primary objective is long-term BTC-denominated growth under strict institutional-grade risk governance.
```

---

# EVENT-DRIVEN SIGNAL PROTOCOL

```text
# SIGNAL GENERATION PROTOCOL

When provided with a Market State Event containing real-time feature data:

1. Ingest the structured JSON payload.
2. Analyze OFI, volatility, liquidity pressure, and regime state.
3. Formulate a probabilistic hypothesis.
4. Determine structural validity.
5. Assign a confidence score.
6. Produce strict JSON output.

STRICT JSON ONLY:

{
  "signal_type": "ACCUMULATION_ENTRY",
  "target_asset": "BTCUSDT",
  "confidence_score": 0.89,
  "market_regime": "HIGH_VOLATILITY_LIQUIDATION_EVENT",
  "rationale": "Positive OFI divergence detected during liquidation cascade.",
  "recommended_params": {
    "entry_zone": [61200, 61450],
    "invalidation_level": 60800,
    "volatility_adjusted_tp": 63500
  }
}
```

---

# MEMORY & TELEMETRY

The AI layer maintains:

- regime memory
- volatility history
- strategy effectiveness tracking
- execution analytics
- drawdown analysis
- structural anomaly history

The memory system MUST NEVER:

- store API keys
- store infrastructure credentials
- store custody information

---

# FINAL PRINCIPLE

The edge does not come from the LLM itself.

The edge comes from:

- execution quality
- market structure understanding
- latency engineering
- disciplined governance
- statistical edge
- infrastructure reliability
- operational consistency
- risk management

The AI layer improves strategic intelligence.

The deterministic layer preserves execution integrity.

Both are required.

Neither alone is sufficient.

