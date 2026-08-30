#!/usr/bin/env python3
"""Fáze B — replay z REÁLNÝCH signálů (journal BUY fills) [KANBAN T1/B].

Rozdíl oproti Fázi A: vstupní body jsou NAŠE SKUTEČNÉ obchody (ts + fill
cena z journalu), ne aproximační signál. Testujeme jen exit strategie
na identických vstupech — čistý A/B test TP/SL konfigurací.

Kromě TP/SL testuje i kontext vstupu: momentum (cena před vstupem rostla)
vs dip (klesala) → odpoví na otázku z backlogu (mean-reverze vs momentum).

Usage:
  sudo python3 replay_real_signals.py [--days 14]
"""

import json
import re
import subprocess
import sys
import time
import urllib.request
from collections import defaultdict


def fetch_candles(days: int):
    """5m candles (14 dní zpět)."""
    end = int(time.time() * 1000)
    start = end - days * 86400 * 1000
    url = (
        f"https://api-pub.bitfinex.com/v2/candles/trade:5m:tBTCUSD/hist"
        f"?limit=5000&sort=1&start={start}&end={end}"
    )
    req = urllib.request.Request(url, headers={"User-Agent": "pirana-backtest"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.load(r)


def fetch_real_entries(hours_back: int = 40):
    """BUY fills z journalu (ts unix, cena)."""
    out = subprocess.run(
        ["sudo", "-n", "journalctl", "-u", "pirana.service",
         f"--since=-{hours_back}h", "--no-pager"],
        capture_output=True, text=True,
    ).stdout
    pat = re.compile(r"^(\w+ \d+ \d\d:\d\d:\d\d).*Asynchronous BUY order executed! Authoritative fill: ([\d.]+) USD")
    entries = []
    for line in out.splitlines():
        m = pat.match(line)
        if m:
            ts = time.mktime(time.strptime(f"2026 {m.group(1)}", "%Y %b %d %H:%M:%S"))
            entries.append((int(ts), float(m.group(2))))
    # dedupe (paralelní signály v tutéž sekundu = jeden vstup pro replay)
    dedup = []
    for ts, p in entries:
        if dedup and ts - dedup[-1][0] < 5 and abs(p - dedup[-1][1]) < 1.0:
            continue
        dedup.append((ts, p))
    return dedup


def candle_at(candles, ts_ms):
    """Nejbližší candle pokrývající čas ts (binární hledání liniárně stačí)."""
    for c in candles:  # candles jsou sortované
        if c[0] > ts_ms:
            return c
    return None


def replay_real(entries, candles, conf):
    """Replay TP/SL strategie na reálných vstupech.

    Pozice se simují NEZÁVISLE (paralelně) — v realitě jich může být
    víc najednou; každá má vlastní TP/SL/trailing.
    Vrací stats + seznam (vstup_ts, pnl, kontext_momentum).
    """
    # index candles podle času pro rychlé hledání
    c_by_ts = {c[0]: c for c in candles}
    c_list = candles

    positions = []  # dict per pozice
    results = []  # (ts, pnl, was_momentum)
    c_idx = 0

    for ts, price in entries:
        # kontext: ceny 30 min před vstupem (6 candles)
        # momentum = cena vstupu > cena před 30 min
        pre_price = None
        for c in c_list:
            if c[0] > (ts - 1800) * 1000:
                pre_price = c[1]  # open té candle
                break
        was_momentum = pre_price is not None and price > pre_price

        # otevřít pozici (ATR vezmeme z candles — jednoduchý True Range průměr 14)
        atr = compute_atr(c_list, ts)
        if atr is None or atr <= 0:
            continue
        tp_d = max(atr * conf["tp_mult"], conf["min_tp"])
        tp_d = min(tp_d, conf["max_tp"])
        sl_d = max(atr * conf["sl_mult"], conf["min_sl"])
        sl_d = min(sl_d, conf["max_sl"])
        positions.append({
            "entry": price, "entry_ts": ts, "tp": price + tp_d, "sl": price - sl_d,
            "peak": price, "breakeven": False, "trailing": False,
            "momentum": was_momentum, "atr": atr, "usd": SIZE_USD,
        })

    # projít candles chronologicky a řešit exity
    # (jednoduchý event loop: pro každou candle zkontrolovat pozice otevřené před ní)
    for c in c_list:
        c_ts = c[0] // 1000
        still = []
        for pos in positions:
            if pos.get("closed"):
                continue
            if c_ts < pos["entry_ts"]:
                still.append(pos)  # candle před vstupem
                continue
            hi, lo, close = c[3], c[4], c[2]
            # breakeven/trailing update
            if close > pos["peak"]:
                pos["peak"] = close
            if not pos["breakeven"] and hi >= pos["entry"] + conf["min_trigger"]:
                pos["breakeven"] = True
                pos["trailing"] = True
                new_sl = pos["entry"] + conf["be_offset"]
                if new_sl > pos["sl"]:
                    pos["sl"] = new_sl
            if pos["trailing"]:
                # ATR v okamžiku — aproximace: fixní vstupní ATR
                trail = max(pos["atr"] * conf["trail_mult"], conf["min_trigger"])
                t_sl = pos["peak"] - trail
                if t_sl > pos["sl"]:
                    pos["sl"] = t_sl
            # exit: SL nejdřív (konzervativně)
            if lo <= pos["sl"]:
                pnl = (pos["sl"] - pos["entry"]) / pos["entry"]
                results.append((pos["entry_ts"], pnl * pos.get("usd", 4.0), pos["momentum"], "SL/trail"))
                pos["closed"] = True
            elif hi >= pos["tp"]:
                pnl = (pos["tp"] - pos["entry"]) / pos["entry"]
                results.append((pos["entry_ts"], pnl * pos.get("usd", 4.0), pos["momentum"], "TP"))
                pos["closed"] = True
            else:
                still.append(pos)
        positions = [p for p in positions if not p.get("closed")] + \
                    [p for p in still if not p.get("closed")]
        # dedupe guard
        seen = set()
        uniq = []
        for p in positions:
            k = id(p)
            if k not in seen:
                seen.add(k)
                uniq.append(p)
        positions = uniq
    return results


def compute_atr(candles, ts, period=14):
    """ATR (Wilder) v časovém okamžiku ts — z candles do ts."""
    past = [c for c in candles if c[0] <= ts * 1000]
    if len(past) < period + 1:
        return None
    trs = []
    for i in range(1, len(past)):
        h, l, pc = past[i][3], past[i][4], past[i - 1][2]
        trs.append(max(h - l, abs(h - pc), abs(l - pc)))
    trs = trs[-period * 3:]  # omezit
    if not trs:
        return None
    # Wilder smoothing
    atr = sum(trs[:period]) / period
    for tr in trs[period:]:
        atr = (atr * (period - 1) + tr) / period
    return atr


CONF_OLD = {"tp_mult": 0.4, "sl_mult": 2.5, "min_tp": 10.0, "max_tp": 300.0,
            "min_sl": 350.0, "max_sl": 1200.0, "trail_mult": 0.5, "min_trigger": 10.0, "be_offset": 2.0}
CONF_MID = {"tp_mult": 0.8, "sl_mult": 1.5, "min_tp": 10.0, "max_tp": 300.0,
            "min_sl": 150.0, "max_sl": 800.0, "trail_mult": 0.5, "min_trigger": 10.0, "be_offset": 2.0}
CONF_NEW = {"tp_mult": 1.8, "sl_mult": 0.7, "min_tp": 10.0, "max_tp": 300.0,
            "min_sl": 30.0, "max_sl": 400.0, "trail_mult": 0.5, "min_trigger": 10.0, "be_offset": 2.0}
CONF_TIGHT = {"tp_mult": 0.5, "sl_mult": 0.5, "min_tp": 10.0, "max_tp": 300.0,
              "min_sl": 30.0, "max_sl": 400.0, "trail_mult": 0.5, "min_trigger": 10.0, "be_offset": 2.0}
CONF_SCALP = {"tp_mult": 0.3, "sl_mult": 0.4, "min_tp": 8.0, "max_tp": 60.0,
              "min_sl": 25.0, "max_sl": 100.0, "trail_mult": 0.3, "min_trigger": 6.0, "be_offset": 2.0}

SIZE_USD = 4.0  # 1 % z 400


def report(name, results):
    n = len(results)
    if not n:
        print(f"{name}: žádné výsledky")
        return
    wins = [r for r in results if r[1] > 0]
    losses = [r for r in results if r[1] <= 0]
    pnl = sum(r[1] for r in results)
    wr = len(wins) / n * 100
    avg_w = sum(r[1] for r in wins) / len(wins) if wins else 0
    avg_l = sum(r[1] for r in losses) / len(losses) if losses else 0
    payoff = abs(avg_w / avg_l) if avg_l else float("inf")
    ev = pnl / n
    tp_hits = sum(1 for r in results if r[3] == "TP")
    print(f"\n══ {name} ══")
    print(f"  RT {n} | W {len(wins)} / L {len(losses)} | WR {wr:.1f} % | payoff {payoff:.2f} | EV/RT {ev:+.5f} USD")
    print(f"  PnL {pnl:+.4f} USD | TP hits {tp_hits} | SL/trail {n - tp_hits}")
    # momentum vs dip rozpad
    for label, flag in [("momentum", True), ("dip", False)]:
        sel = [r for r in results if r[2] == flag]
        if sel:
            p = sum(r[1] for r in sel)
            w = sum(1 for r in sel if r[1] > 0)
            print(f"  vstupy {label:8s}: {len(sel):3d} | WR {w/len(sel)*100:5.1f} % | PnL {p:+.4f}")
    return {"pnl": pnl, "ev": ev, "wr": wr, "payoff": payoff, "n": n}


def main():
    print("Fáze B — replay z REÁLNÝCH signálů")
    entries = fetch_real_entries(40)
    print(f"Reálných vstupů (posledních 40 h): {len(entries)}")
    candles = fetch_candles(14)
    print(f"Candles: {len(candles)} (5m, 14 dní)")

    # nastavit usd a atr per pozice v replay_real (mutace conf)
    out = {}
    for conf_name, conf in [("STARÁ (TP 0.4/SL 2.5)", CONF_OLD),
                             ("PŮLNOČNÍ (0.8/1.5)", CONF_MID),
                             ("NOVÁ (1.8/0.7)", CONF_NEW),
                             ("TIGHT (0.5/0.5)", CONF_TIGHT),
                             ("SCALP (0.3/0.4, těsné)", CONF_SCALP)]:
        results = replay_real(entries, candles, conf)
        out[conf_name] = report(conf_name, results)

    print("\n══ VERDIKT (řazeno dle PnL) ══")
    for name, r in sorted(out.items(), key=lambda kv: kv[1]["pnl"], reverse=True):
        print(f"  {name}: PnL {r['pnl']:+.4f} | EV/RT {r['ev']:+.5f} | WR {r['wr']:.0f} % | payoff {r['payoff']:.2f}")


if __name__ == "__main__":
    main()
