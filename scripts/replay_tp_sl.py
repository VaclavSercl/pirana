#!/usr/bin/env python3
"""Replay/Backtest engine — TP/SL asymetrie validace [KANBAN T1 Fáze A].

Stáhne 5m candles z Bitfinexu (veřejné API), replayne mean-reverzní
scalping strategii PIRANA s definovanými TP/SL vzdálenostmi a
porovná konfigurace na STEJNÝCH datech.

Strategie (věrná dle src/main.rs + strategy.toml):
- ATR z 1m candles (Wilder smoothing, 14 period — jako AtrCalculator)
- Vstup: mean-reverzní — po N minut konsekutivního pohybu jedna strana
  (naše OFI signály jsou momentum-odražené; aproximace: pullback entry)
- LONG jen (spot systém, BUY/SELL close)
- Výstup: TP / SL / trailing (breakeven po min_trigger, trail ATR×mult)
- Velikost: 1 % equity (jako baseline sizing dnes)

Usage:
  python3 replay_tp_sl.py [--days 14] [--verbose]
"""

import json
import math
import subprocess
import sys
import time
import urllib.request
from collections import namedtuple

Candle = namedtuple("Candle", "ts open close high low volume")

# ── Konfigurace strategie (ze strategy.toml + config.rs defaultů) ──

CONF_OLD = {  # do 29. 8. 11:06
    "name": "STARÁ (do 29.8. ráno)",
    "tp_mult": 0.4, "sl_mult": 2.5,
    "min_tp": 10.0, "max_tp": 300.0,
    "min_sl": 350.0, "max_sl": 1200.0,
    "trail_mult": 0.5, "min_trigger": 10.0, "be_offset": 2.0,
}

CONF_MID = {  # 29. 8. 11:06 (běžela ~1 h, chybná asymetrie — pro dokumentaci)
    "name": "PŮLNOČNÍ OMYL (29.8. 11:06)",
    "tp_mult": 0.8, "sl_mult": 1.5,
    "min_tp": 10.0, "max_tp": 300.0,
    "min_sl": 150.0, "max_sl": 800.0,
    "trail_mult": 0.5, "min_trigger": 10.0, "be_offset": 2.0,
}

CONF_NEW = {  # 29. 8. 11:06 po oponentuře — aktuální
    "name": "NOVÁ (aktuální)",
    "tp_mult": 1.8, "sl_mult": 0.7,
    "min_tp": 10.0, "max_tp": 300.0,
    "min_sl": 30.0, "max_sl": 400.0,
    "trail_mult": 0.5, "min_trigger": 10.0, "be_offset": 2.0,
}

EQUITY_USD = 400.0
SIZE_PCT = 0.01  # 1 % pozice
ENTRY_LOOKBACK = 3  # 3×5m = 15 min proti pohybu  # po 5 minut proti pohybu = mean-reverzní vstup


def fetch_candles(days: int) -> list:
    """Stáhne 5m candles za N dní (Bitfinex: jeden request, start+end)."""
    end = int(time.time() * 1000)
    start = end - days * 86400 * 1000
    url = (
        f"https://api-pub.bitfinex.com/v2/candles/trade:5m:tBTCUSD/hist"
        f"?limit=5000&sort=1&start={start}&end={end}"
    )
    req = urllib.request.Request(url, headers={"User-Agent": "pirana-backtest"})
    with urllib.request.urlopen(req, timeout=60) as r:
        data = json.load(r)
    return [Candle(c[0], c[1], c[2], c[3], c[4], c[5]) for c in data]


class AtrCalculator:
    """Wilder ATR — věrný našemu Rust AtrCalculator."""

    def __init__(self, period: int = 14):
        self.period = period
        self.value = 0.0
        self.count = 0
        self.prev_close = None

    def update(self, candle: Candle) -> float:
        if self.prev_close is None:
            tr = candle.high - candle.low
        else:
            tr = max(
                candle.high - candle.low,
                abs(candle.high - self.prev_close),
                abs(candle.low - self.prev_close),
            )
        self.prev_close = candle.close
        self.count += 1
        if self.count < self.period:
            self.value = tr  # seed
            return self.value
        self.value = (self.value * (self.period - 1) + tr) / self.period
        return self.value


def replay(candles: list, conf: dict, verbose: bool = False) -> dict:
    """Replay strategie na candles. Vrací statistiky."""
    atr = AtrCalculator(14)
    position = None  # dict: entry, tp, sl, peak, breakeven, trailing
    stats = {
        "rt": 0, "wins": 0, "pnl": 0.0, "max_dd": 0.0, "peak_pnl": 0.0,
        "losses_list": [], "wins_list": [], "sl_hits": 0, "tp_hits": 0,
        "trail_hits": 0, "cooldown_until": -1,
    }
    last_moves = []  # znaménka pohybů pro mean-reverzní vstup

    for c in candles:
        a = atr.update(c)
        price = c.close

        # ── řízení pozice ──
        if position is not None:
            pos = position
            if price > pos["peak"]:
                pos["peak"] = price
            # breakeven
            if not pos["breakeven"] and price >= pos["entry"] + conf["min_trigger"]:
                pos["breakeven"] = True
                pos["trailing"] = True
                new_sl = pos["entry"] + conf["be_offset"]
                if new_sl > pos["sl"]:
                    pos["sl"] = new_sl
            # trailing
            if pos["trailing"]:
                trail = max(a * conf["trail_mult"], conf["min_trigger"])
                t_sl = pos["peak"] - trail
                if t_sl > pos["sl"]:
                    pos["sl"] = t_sl
            # exit — intrabar: SL dle low, TP dle high (konzervativně SL nejdřív)
            if c.low <= pos["sl"]:
                pnl = (pos["sl"] - pos["entry"]) / pos["entry"] * pos["usd"]
                stats["pnl"] += pnl
                stats["sl_hits"] += 1
                _close(stats, pos, pnl)
                position = None
                stats["cooldown_until"] = c.ts  # loss-cooldown 60 s = 1 candle
            elif c.high >= pos["tp"]:
                pnl = (pos["tp"] - pos["entry"]) / pos["entry"] * pos["usd"]
                stats["pnl"] += pnl
                stats["tp_hits"] += 1
                _close(stats, pos, pnl)
                position = None
        # ── vstup ──
        else:
            if c.ts < stats["cooldown_until"]:
                continue
            if len(last_moves) >= ENTRY_LOOKBACK and a > 0:
                # 5 minut po sobě klesalo → mean-reverzní LONG
                if all(m < 0 for m in last_moves[-ENTRY_LOOKBACK:]):
                    tp_d = (a * conf["tp_mult"]).__max__(conf["min_tp"]) if hasattr(a * conf["tp_mult"], "__max__") else max(a * conf["tp_mult"], conf["min_tp"])
                    tp_d = min(tp_d, conf["max_tp"])
                    sl_d = max(a * conf["sl_mult"], conf["min_sl"])
                    sl_d = min(sl_d, conf["max_sl"])
                    entry = price
                    usd = EQUITY_USD * SIZE_PCT
                    position = {
                        "entry": entry, "tp": entry + tp_d, "sl": entry - sl_d,
                        "peak": entry, "breakeven": False, "trailing": False,
                        "usd": usd, "atr": a,
                    }
        # sledování pohybů
        if len(candles) > 0:
            last_moves.append(c.close - c.open)
        # drawdown
        if stats["pnl"] > stats["peak_pnl"]:
            stats["peak_pnl"] = stats["pnl"]
        dd = stats["peak_pnl"] - stats["pnl"]
        if dd > stats["max_dd"]:
            stats["max_dd"] = dd

    return stats


def _close(stats, pos, pnl):
    stats["rt"] += 1
    if pnl > 0:
        stats["wins"] += 1
        stats["wins_list"].append(pnl)
    else:
        stats["losses_list"].append(pnl)


def report(conf, stats):
    n = stats["rt"]
    wr = stats["wins"] / n * 100 if n else 0
    avg_w = sum(stats["wins_list"]) / len(stats["wins_list"]) if stats["wins_list"] else 0
    avg_l = sum(stats["losses_list"]) / len(stats["losses_list"]) if stats["losses_list"] else 0
    payoff = abs(avg_w / avg_l) if avg_l else float("inf")
    ev = (wr / 100 * avg_w) + ((1 - wr / 100) * avg_l) if n else 0
    print(f"\n══ {conf['name']} ══")
    print(f"  RT: {n} | W {stats['wins']} / L {n - stats['wins']} | WR {wr:.1f} %")
    print(f"  avg win {avg_w:+.4f} | avg loss {avg_l:+.4f} | payoff {payoff:.2f}:1")
    print(f"  TP hits {stats['tp_hits']} | SL hits {stats['sl_hits']}")
    print(f"  PnL: {stats['pnl']:+.4f} USD | EV/RT: {ev:+.5f} USD")
    print(f"  Max DD: {stats['max_dd']:.4f} USD ({stats['max_dd'] / EQUITY_USD * 100:.2f} % equity)")
    return {"pnl": stats["pnl"], "wr": wr, "payoff": payoff, "ev": ev, "dd": stats["max_dd"], "rt": n}


def main():
    days = 14
    if "--days" in sys.argv:
        days = int(sys.argv[sys.argv.index("--days") + 1])
    verbose = "--verbose" in sys.argv

    print(f"Stahuji {days} dní 1m candles z Bitfinexu…")
    candles = fetch_candles(days)
    print(f"Staženo: {len(candles)} candles "
          f"({candles[0].ts and time.strftime('%d.%m %H:%M', time.localtime(candles[0].ts/1000))} → "
          f"{time.strftime('%d.%m %H:%M', time.localtime(candles[-1].ts/1000))})")

    results = {}
    for conf in [CONF_OLD, CONF_MID, CONF_NEW]:
        stats = replay(candles, conf, verbose)
        results[conf["name"]] = report(conf, stats)

    # ── Verdikt ──
    print("\n══ VERDIKT ══")
    order = sorted(results.items(), key=lambda kv: kv[1]["pnl"], reverse=True)
    for i, (name, r) in enumerate(order):
        medal = ["🥇", "🥈", "🥉"][i] if i < 3 else "  "
        print(f"{medal} {name}: PnL {r['pnl']:+.4f} USD | WR {r['wr']:.1f} % | payoff {r['payoff']:.2f} | EV/RT {r['ev']:+.5f}")


if __name__ == "__main__":
    main()
