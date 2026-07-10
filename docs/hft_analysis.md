# PIRANA HFT Analysis Report

## Bitfinex Fee Structure (Updated July 2026)

### ⚠️ IMPORTANT: Zero Fees Since December 17, 2025

**As of December 17, 2025, Bitfinex charges ZERO fees for ALL trading activity.**

Source: https://www.bitfinex.com/fees (verified July 10, 2026)

#### Order Execution Fees

| Activity | Maker Fee | Taker Fee |
|---|---|---|
| **Spot and Margin trades** | **Zero** | **Zero** |
| **Derivatives trades** | **Zero** | **Zero** |
| OTC Trades | Zero | Zero |

The previous tiered fee structure (0.10% maker / 0.20% taker at Tier 0) is **no longer in effect**.

#### Other Fees (still apply)

| Service | Fee |
|---|---|
| Bank wire deposit | 0.100% (min $60) |
| Bank wire withdrawal | 0.100% (min $100) |
| Express bank wire withdrawal | 1.000% (min $125) |
| Crypto deposit | FREE |
| Stablecoin deposit | FREE |
| Crypto withdrawal | FREE |
| Internal transfer (Bitfinex to Bitfinex) | FREE |
| Bitfinex Borrow (funding fees) | Zero for borrower |
| Margin Funding provider fee | 15% of fees generated |

#### Impact on PIRANA Strategy

- **No trading fees** = round-trip cost is **$0.00** regardless of order type
- Both LIMIT and MARKET orders are free
- The previous concern about MARKET vs LIMIT fees is **moot** — no fee difference
- Spread capture profitability is now purely about price movement, not fee overhead
- HFT frequency is no longer constrained by fee accumulation

#### Minimum Order Sizes

| Symbol | Min Order Size |
|---|---|
| tBTCUSD | 0.00001 BTC (~$0.64 at $64,000) |
| tETHUSD | 0.0001 ETH (~$0.25) |

#### Rate Limits

- **REST API**: 90 requests per minute
- **WebSocket**: 30 subscriptions per connection
- **Order submission**: 10 orders per second (per symbol)
- **Order cancellation**: 10 cancellations per second

### HFT Strategy Implications (Zero-Fee Era)

With zero fees:

1. **Spread capture**: Any price improvement, even $0.01, is pure profit
2. **Order type**: LIMIT or MARKET — no fee difference (use LIMIT for price control)
3. **Frequency**: Trade as often as rate limits allow — no fee drain
4. **Small trades**: Even minimum-size trades ($0.64) are viable
5. **Round-trip**: Buy + Sell = $0.00 in fees (was $0.024 at old taker rates)

### Account Status (as of July 10, 2026)

| Asset | Balance |
|---|---|
| USD | ~$408 |
| BTC | ~0.0014 BTC (~$90) |
| 30d Volume | ~$232 |
| Fee Level | N/A (Zero fees) |
