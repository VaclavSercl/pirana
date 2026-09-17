#!/usr/bin/env python3
"""Backtest framework pro T2 entry experiment.

Testuje různé vstupní signály na research_ticks.jsonl datech:
1. OFI buying pressure (současná strategie)
2. Flow divergence (cena vs flow)
3. Pullback + flow confirmation
4. Mean reversion po selling pressure

Výstup: statistická significance, EV/RT, win rate, max DD.
"""
import json
import sys
from collections import defaultdict


def load_research_ticks():
    ticks = []
    with open('/var/lib/pirana/research_ticks.jsonl') as f:
        for line in f:
            try:
                ticks.append(json.loads(line))
            except:
                pass
    ticks.sort(key=lambda t: t['ms'])
    return ticks


def compute_flow(window):
    """OFI-like flow imbalance z tick okna."""
    buy_vol = sum(t['q'] for t in window if t['s'] == 1)
    sell_vol = sum(t['q'] for t in window if t['s'] == -1)
    total = buy_vol + sell_vol
    if total == 0:
        return 0.0
    return (buy_vol - sell_vol) / total


def simulate_entry(ticks, entry_idx, tp_bps=5.0, sl_bps=10.0, max_hold=60):
    """Simuluj exit s TP/SL/trailing na tick datech."""
    entry_price = ticks[entry_idx]['p']
    tp_price = entry_price * (1 + tp_bps / 10000)
    sl_price = entry_price * (1 - sl_bps / 10000)
    
    peak = entry_price
    for j in range(entry_idx + 1, min(entry_idx + max_hold + 1, len(ticks))):
        p = ticks[j]['p']
        if p > peak:
            peak = p
        if p >= tp_price:
            return (p - entry_price) / entry_price * 10000, 'TP', j - entry_idx
        if p <= sl_price:
            return (p - entry_price) / entry_price * 10000, 'SL', j - entry_idx
        # Trailing stop: 30% od peak
        if peak > entry_price * 1.001:
            trail_sl = peak * 0.997
            if p <= trail_sl:
                return (p - entry_price) / entry_price * 10000, 'TRAIL', j - entry_idx
    
    # Max hold exit
    exit_price = ticks[min(entry_idx + max_hold, len(ticks) - 1)]['p']
    return (exit_price - entry_price) / entry_price * 10000, 'HOLD', max_hold


def backtest_strategy(ticks, signal_fn, name, min_gap=30):
    """Backtestuje strategii na tick datech."""
    results = []
    last_entry = -min_gap
    
    for i in range(200, len(ticks) - 60):
        if i - last_entry < min_gap:
            continue
        
        if signal_fn(ticks, i):
            pnl, exit_type, hold = simulate_entry(ticks, i)
            results.append({'pnl': pnl, 'exit': exit_type, 'hold': hold})
            last_entry = i
    
    if not results:
        return {'name': name, 'n': 0}
    
    pnls = [r['pnl'] for r in results]
    wins = [p for p in pnls if p > 0]
    losses = [p for p in pnls if p <= 0]
    
    exits = defaultdict(int)
    for r in results:
        exits[r['exit']] += 1
    
    return {
        'name': name,
        'n': len(results),
        'ev': sum(pnls) / len(pnls),
        'wr': len(wins) / len(pnls) * 100 if pnls else 0,
        'avg_win': sum(wins) / len(wins) if wins else 0,
        'avg_loss': sum(losses) / len(losses) if losses else 0,
        'max_dd': min(pnls) if pnls else 0,
        'exits': dict(exits),
    }


def main():
    ticks = load_research_ticks()
    print(f"Loaded {len(ticks)} research ticks")
    
    if len(ticks) < 500:
        print("Málo dat — potřebuje alespoň 500 ticků pro smysluplný backtest.")
        print("Nech tick_research.py běžet déle.")
        return
    
    # Strategie 1: OFI buying pressure (současná)
    def ofi_buy(ticks, i):
        window = ticks[max(0, i-20):i]
        flow = compute_flow(window)
        return flow > 0.1
    
    # Strategie 2: Pullback + positive flow
    def pullback_flow(ticks, i):
        window = ticks[max(0, i-20):i]
        flow = compute_flow(window)
        hwm_window = ticks[max(0, i-100):i]
        hwm = max(t['p'] for t in hwm_window)
        price = ticks[i]['p']
        return flow > 0.05 and price < hwm * 0.999
    
    # Strategie 3: Mean reversion po selling pressure
    def mean_rev(ticks, i):
        window = ticks[max(0, i-10):i]
        flow = compute_flow(window)
        prev_window = ticks[max(0, i-30):max(0, i-10)]
        prev_flow = compute_flow(prev_window)
        return prev_flow < -0.3 and flow > -0.05  # selling pressure ustala
    
    # Strategie 4: Flow divergence (cena down, flow turning positive)
    def flow_divergence(ticks, i):
        if i < 30:
            return False
        price_change = (ticks[i]['p'] - ticks[i-30]['p']) / ticks[i-30]['p'] * 10000
        window = ticks[max(0, i-10):i]
        flow = compute_flow(window)
        return price_change < -3 and flow > 0.1  # cena -3bps, flow kladný
    
    strategies = [
        (ofi_buy, "OFI Buy Pressure (současná)"),
        (pullback_flow, "Pullback + Flow"),
        (mean_rev, "Mean Reversion (post-selling)"),
        (flow_divergence, "Flow Divergence"),
    ]
    
    print(f"\n{'='*60}")
    print(f"{'STRATEGIE':<30} | {'n':>4} | {'EV bps':>7} | {'WR%':>5} | {'MaxDD':>6}")
    print(f"{'='*60}")
    
    for fn, name in strategies:
        r = backtest_strategy(ticks, fn, name)
        if r['n'] > 0:
            print(f"{name:<30} | {r['n']:4d} | {r['ev']:+7.2f} | {r['wr']:5.1f} | {r['max_dd']:6.1f}")
            if r.get('exits'):
                exits_str = ', '.join(f"{k}:{v}" for k, v in r['exits'].items())
                print(f"  └─ exity: {exits_str}")
        else:
            print(f"{name:<30} | {'---':>4} | {'---':>7} | {'---':>5} | {'---':>6}")
    
    print(f"{'='*60}")


if __name__ == '__main__':
    main()
