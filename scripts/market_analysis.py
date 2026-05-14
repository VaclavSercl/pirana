#!/usr/bin/env python3
"""
PIRANA Market Analysis Script
Analyzes Bitfinex BTC/USD market data for trading signals
"""

import urllib.request
import json
import time
from collections import deque

def fetch_ticker():
    url = "https://api.bitfinex.com/v2/ticker/tBTCUSD"
    req = urllib.request.Request(url, headers={"User-Agent": "PIRANA/1.0"})
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())

def fetch_order_book(depth=25):
    url = f"https://api.bitfinex.com/v2/book/tBTCUSD/P0?len={depth}"
    req = urllib.request.Request(url, headers={"User-Agent": "PIRANA/1.0"})
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())

def fetch_trades(limit=100):
    url = f"https://api.bitfinex.com/v2/trades/tBTCUSD/hist?limit={limit}"
    req = urllib.request.Request(url, headers={"User-Agent": "PIRANA/1.0"})
    with urllib.request.urlopen(req, timeout=10) as resp:
        return json.loads(resp.read())

def analyze_order_book(data):
    bids = [x for x in data if x[2] > 0]
    asks = [x for x in data if x[2] < 0]
    
    if not bids or not asks:
        return None
    
    best_bid = max(bids, key=lambda x: x[0])
    best_ask = min(asks, key=lambda x: x[0])
    spread = best_ask[0] - best_bid[0]
    mid_price = (best_ask[0] + best_bid[0]) / 2
    
    bid_vol = sum(x[2] for x in bids)
    ask_vol = sum(abs(x[2]) for x in asks)
    imbalance = (bid_vol - ask_vol) / (bid_vol + ask_vol) if (bid_vol + ask_vol) > 0 else 0
    
    # VWAP for bids and asks
    bid_vwap = sum(x[0] * x[2] for x in bids) / bid_vol if bid_vol > 0 else 0
    ask_vwap = sum(x[0] * abs(x[2]) for x in asks) / ask_vol if ask_vol > 0 else 0
    
    return {
        'best_bid': best_bid[0],
        'best_ask': best_ask[0],
        'spread': spread,
        'mid_price': mid_price,
        'bid_vol': bid_vol,
        'ask_vol': ask_vol,
        'imbalance': imbalance,
        'bid_vwap': bid_vwap,
        'ask_vwap': ask_vwap,
        'bid_levels': len(bids),
        'ask_levels': len(asks),
    }

def analyze_trades(data):
    if not data:
        return None
    
    buys = [t for t in data if t[2] > 0]
    sells = [t for t in data if t[2] < 0]
    
    buy_vol = sum(t[2] for t in buys)
    sell_vol = sum(abs(t[2]) for t in sells)
    net_flow = buy_vol - sell_vol
    
    prices = [t[3] for t in data]
    timestamps = [t[1] for t in data]
    
    # Price change
    price_change = prices[0] - prices[-1] if len(prices) > 1 else 0
    price_change_pct = (price_change / prices[-1] * 100) if prices[-1] > 0 else 0
    
    # Average trade size
    avg_size = sum(abs(t[2]) for t in data) / len(data)
    
    # Large trades (>0.1 BTC)
    large_trades = [t for t in data if abs(t[2]) > 0.1]
    large_buy_vol = sum(t[2] for t in large_trades if t[2] > 0)
    large_sell_vol = sum(abs(t[2]) for t in large_trades if t[2] < 0)
    
    return {
        'total_trades': len(data),
        'buys': len(buys),
        'sells': len(sells),
        'buy_vol': buy_vol,
        'sell_vol': sell_vol,
        'net_flow': net_flow,
        'price_change': price_change,
        'price_change_pct': price_change_pct,
        'avg_size': avg_size,
        'large_trades': len(large_trades),
        'large_buy_vol': large_buy_vol,
        'large_sell_vol': large_sell_vol,
        'high': max(prices),
        'low': min(prices),
        'latest': prices[0],
    }

def compute_ofi(trades_data):
    """Compute Order Flow Imbalance from recent trades"""
    if len(trades_data) < 2:
        return 0.0
    
    ofi_values = []
    for i in range(1, len(trades_data)):
        prev_price = trades_data[i-1][3]
        curr_price = trades_data[i][3]
        curr_qty = abs(trades_data[i][2])
        
        if curr_price > prev_price:
            indicator = 1.0
        elif curr_price < prev_price:
            indicator = -1.0
        else:
            indicator = 0.0
        
        ofi_values.append(indicator * curr_qty)
    
    if not ofi_values:
        return 0.0
    
    total = sum(ofi_values)
    abs_total = sum(abs(v) for v in ofi_values)
    
    return total / abs_total if abs_total > 0 else 0.0

def generate_signal(ob, trades, ofi):
    """Generate trading signal based on microstructure analysis"""
    signals = []
    confidence = 0.5
    
    # OFI signal
    if ofi > 0.3:
        signals.append("OFI: Strong buying pressure")
        confidence += 0.15
    elif ofi < -0.3:
        signals.append("OFI: Strong selling pressure")
        confidence -= 0.15
    
    # Order book imbalance
    if ob['imbalance'] > 0.2:
        signals.append("Book: Bid-heavy (support)")
        confidence += 0.1
    elif ob['imbalance'] < -0.2:
        signals.append("Book: Ask-heavy (resistance)")
        confidence -= 0.1
    
    # Trade flow
    if trades['net_flow'] > 0.5:
        signals.append("Flow: Net buying")
        confidence += 0.1
    elif trades['net_flow'] < -0.5:
        signals.append("Flow: Net selling")
        confidence -= 0.1
    
    # Large trades
    if trades['large_buy_vol'] > trades['large_sell_vol'] * 2:
        signals.append("Large: Whales buying")
        confidence += 0.1
    elif trades['large_sell_vol'] > trades['large_buy_vol'] * 2:
        signals.append("Large: Whales selling")
        confidence -= 0.1
    
    # Spread analysis
    spread_pct = ob['spread'] / ob['mid_price'] * 10000  # in bps
    if spread_pct < 2:
        signals.append(f"Spread: Tight ({spread_pct:.1f} bps) - good liquidity")
    elif spread_pct > 10:
        signals.append(f"Spread: Wide ({spread_pct:.1f} bps) - low liquidity")
    
    confidence = max(0.0, min(1.0, confidence))
    
    if confidence > 0.7:
        signal_type = "ACCUMULATION_ENTRY"
    elif confidence < 0.3:
        signal_type = "DISTRIBUTION_EXIT"
    else:
        signal_type = "HOLD"
    
    return signal_type, confidence, signals

def main():
    print("=" * 60)
    print("  PIRANA Market Analysis — Bitfinex BTC/USD")
    print(f"  {time.strftime('%Y-%m-%d %H:%M:%S')}")
    print("=" * 60)
    
    # Fetch data
    print("\n[1/4] Fetching ticker...")
    ticker = fetch_ticker()
    print(f"  Last: ${ticker[0]:,.0f}  |  Bid: ${ticker[1]:,.0f}  |  Ask: ${ticker[3]:,.0f}")
    print(f"  24h Change: {ticker[4]:+.2f}%  |  24h High: ${ticker[8]:,.0f}  |  Low: ${ticker[9]:,.0f}")
    print(f"  24h Volume: {ticker[7]:,.2f} BTC")
    
    print("\n[2/4] Fetching order book...")
    ob_data = fetch_order_book(25)
    ob = analyze_order_book(ob_data)
    print(f"  Mid Price: ${ob['mid_price']:,.2f}")
    print(f"  Spread: ${ob['spread']:.2f} ({ob['spread']/ob['mid_price']*10000:.1f} bps)")
    print(f"  Bid Vol: {ob['bid_vol']:.4f} BTC  |  Ask Vol: {ob['ask_vol']:.4f} BTC")
    print(f"  Imbalance: {ob['imbalance']:+.4f} ({'Bid-heavy' if ob['imbalance'] > 0 else 'Ask-heavy'})")
    print(f"  Bid VWAP: ${ob['bid_vwap']:,.2f}  |  Ask VWAP: ${ob['ask_vwap']:,.2f}")
    
    print("\n[3/4] Fetching recent trades...")
    trades_data = fetch_trades(100)
    trades = analyze_trades(trades_data)
    print(f"  Trades: {trades['total_trades']}  |  Buys: {trades['buys']}  |  Sells: {trades['sells']}")
    print(f"  Buy Vol: {trades['buy_vol']:.4f}  |  Sell Vol: {trades['sell_vol']:.4f}")
    print(f"  Net Flow: {trades['net_flow']:+.4f} BTC")
    print(f"  Price Change: {trades['price_change']:+.0f} ({trades['price_change_pct']:+.3f}%)")
    print(f"  Large Trades: {trades['large_trades']}  |  Large Buy: {trades['large_buy_vol']:.4f}  |  Large Sell: {trades['large_sell_vol']:.4f}")
    
    print("\n[4/4] Computing OFI...")
    ofi = compute_ofi(trades_data)
    print(f"  OFI: {ofi:+.4f} ({'Buying pressure' if ofi > 0 else 'Selling pressure'})")
    
    # Generate signal
    signal_type, confidence, signals = generate_signal(ob, trades, ofi)
    
    print("\n" + "=" * 60)
    print(f"  SIGNAL: {signal_type}")
    print(f"  CONFIDENCE: {confidence:.0%}")
    print("=" * 60)
    
    if signals:
        print("\n  Factors:")
        for s in signals:
            print(f"    • {s}")
    
    # Output JSON for PIRANA
    result = {
        "signal_type": signal_type,
        "confidence": confidence,
        "ofi": ofi,
        "spread_bps": ob['spread'] / ob['mid_price'] * 10000,
        "imbalance": ob['imbalance'],
        "net_flow": trades['net_flow'],
        "price": ticker[0],
        "volume_24h": ticker[7],
        "change_24h_pct": ticker[4],
    }
    
    print(f"\n  JSON: {json.dumps(result)}")
    return result

if __name__ == "__main__":
    main()
