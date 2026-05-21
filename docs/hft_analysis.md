# PIRANA HFT Analysis Report

## Bitfinex API Analysis

### Account Balances
| Asset | Free | Locked |
|-------|------|--------|
| UST | 0.05033621 | 0 |
| ETH | 0.00000035 | 0 |
| USD | 167.97 | 0 |
| BTC | 0.00334339 | 0 |

### Trading Volume (30 days)
| Asset | Volume | Maker Volume |
|-------|--------|--------------|
| BTC | 0.00295743 | - |
| Total (USD) | 231.79 | 206.78 |

### Bitfinex Fee Structure

#### Trading Fees (Maker/Taker)
| Level | Maker Fee | Taker Fee | 30d Volume (USD) |
|-------|-----------|-----------|------------------|
| 0 | 0.10% | 0.20% | < 500K |
| 1 | 0.08% | 0.20% | 500K - 1M |
| 2 | 0.06% | 0.18% | 1M - 2.5M |
| 3 | 0.04% | 0.16% | 2.5M - 5M |
| 4 | 0.02% | 0.14% | 5M - 10M |
| 5 | 0.00% | 0.12% | 10M - 25M |
| 6 | 0.00% | 0.10% | 25M+ |

**Current level: 0** (volume < 500K USD)
- **Maker fee: 0.10%**
- **Taker fee: 0.20%**

#### Minimum Order Sizes
| Symbol | Min Order Size |
|--------|----------------|
| tBTCUSD | 0.00001 BTC (~$0.81) |
| tETHUSD | 0.0001 ETH (~$0.25) |

#### Rate Limits
- **REST API**: 90 requests per minute
- **WebSocket**: 30 subscriptions per connection
- **Order submission**: 10 orders per second (per symbol)
- **Order cancellation**: 10 cancellations per second

### HFT Strategy Considerations

#### Spread Capture Profitability
With current fees (0.10% maker, 0.20% taker):
- **Round-trip cost**: 0.30% (buy maker + sell taker)
- **Minimum spread needed**: 0.30% to break even
- **At BTC $81,356**: minimum spread = $244.07

#### Optimal Order Size
- **Minimum**: 0.00001 BTC ($0.81)
- **Recommended**: 0.001 BTC ($81.36) — balances fee impact
- **Maximum per trade**: 0.01 BTC ($813.56) — risk management

#### Frequency Limits
- **Max 10 orders/second** per symbol
- **Max 10 cancellations/second**
- **Recommended**: 1-2 orders/second to avoid rate limits

### Implementation Plan

1. **Spread Capture Strategy**
   - Place buy order at bid - $1
   - Place sell order at ask + $1
   - Cancel and replace on price change > $0.50
   - Order size: 0.001 BTC per side

2. **Fee Optimization**
   - Use LIMIT orders (maker) when possible
   - Avoid MARKET orders (taker fee 0.20%)
   - Target 0.10% maker fee by providing liquidity

3. **Risk Management**
   - Max exposure: 0.01 BTC per trade
   - Stop-loss: 0.5% price drop
   - Take-profit: 0.3% price rise
   - Max daily loss: 3%

4. **Rate Limit Compliance**
   - Max 1 order per second
   - Batch cancellations
   - Use WebSocket for real-time data
