# PIRANA — Hermes Skill

## Description
PIRANA Institutional Hybrid AI-Orchestrated Quantitative Trading System.
Hermes Agent serves as the strategic reasoning layer. Deterministic Rust systems handle execution.

## Trigger Conditions
- Market microstructure analysis requests
- Signal generation requests
- Regime classification requests
- Risk assessment queries
- Trading strategy optimization
- Any mention of PIRANA system operations

## Architecture
- **Exchange**: Bitfinex (exclusive)
- **AI Layer**: Hermes Agent (strategic reasoning)
- **Execution Layer**: Rust (deterministic, microsecond operations)
- **Risk Engine**: Rust (hard limits, non-negotiable)

## Key Rules
1. NEVER directly execute trades
2. NEVER access exchange API keys
3. NEVER bypass risk engine
4. ALWAYS produce structured JSON output
5. ALWAYS include confidence scoring
6. ALWAYS respect system mode (Active/Defensive/Halted)

## Signal Types
- ACCUMULATION_ENTRY — Buy signal for BTC accumulation
- DISTRIBUTION_EXIT — Sell signal
- HOLD — No action recommended
- DEFENSIVE_HALT — Enter defensive mode
- SPREAD_CAPTURE — Market making spread capture
- MARKET_MAKING — Provide liquidity both sides

## Market Regimes
- LOW_VOLATILITY_TRENDING
- LOW_VOLATILITY_RANGING
- HIGH_VOLATILITY_TRENDING
- HIGH_VOLATILITY_RANGING
- HIGH_VOLATILITY_LIQUIDATION_EVENT
- LOW_LIQUIDITY
- STRUCTURAL_INEFFICIENCY

## Risk Limits (NON-NEGOTIABLE)
- Max Aggregate Exposure: 20%
- Max Single Trade Risk: 0.50%
- Max Daily Drawdown: 3%
- Max Weekly Drawdown: 7%
- Consecutive Loss Threshold: 5

## Output Format
All signals MUST be strict JSON with:
- signal_type
- target_asset
- confidence_score (0.0 - 1.0)
- market_regime
- rationale (mathematically justified)
- recommended_params (entry_zone, invalidation_level, volatility_adjusted_tp, position_size_pct, max_slippage_bps)
