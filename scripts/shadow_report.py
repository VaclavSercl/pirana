#!/usr/bin/env python3
"""Shadow A/B report — srovnání živé (pirana_) vs stínové (shadow_) strategie.

[FÁZE B/2 KANBAN T1] Živá strategie: SCALP (TP 0.3×ATR/SL 0.4×ATR).
Stínová: TIGHT (TP 0.5×ATR / SL 0.5×ATR) — paper pozice na stejných
vstupech. Po 200+ RT je srovnání statisticky smysluplné.

Usage: python3 scripts/shadow_report.py
"""

import json
from collections import defaultdict


def load(prefix):
    trades = []
    with open("/var/lib/pirana/trade_ledger.jsonl") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            parts = [line]
            while True:
                idx = parts[-1][1:].find('{"pnl_sats"')
                if idx > 0:
                    head, tail = parts[-1][:idx + 1], parts[-1][idx + 1:]
                    parts[-1] = head
                    parts.append(tail)
                else:
                    break
            for p in parts:
                try:
                    t = json.loads(p)
                    if t.get("cid", "").startswith(prefix):
                        trades.append(t)
                except Exception:
                    pass
    trades.sort(key=lambda t: t["ts"])
    return trades


def stats(trades):
    if not trades:
        return None
    n = len(trades)
    wins = [t for t in trades if t["pnl_sats"] > 0]
    pnl_sats = sum(t["pnl_sats"] for t in trades)
    pnl_usd = pnl_sats / 1e8 * (trades[-1].get("fill_price", 78000))
    wr = len(wins) / n * 100
    avg_w = sum(t["pnl_sats"] for t in wins) / len(wins) if wins else 0
    losses = [t for t in trades if t["pnl_sats"] <= 0]
    avg_l = sum(t["pnl_sats"] for t in losses) / len(losses) if losses else 0
    payoff = abs(avg_w / avg_l) if avg_l else float("inf")
    ev = pnl_sats / n
    return {
        "n": n, "wr": wr, "pnl_sats": pnl_sats, "pnl_usd": pnl_usd,
        "payoff": payoff, "ev_sats": ev,
    }


def main():
    live = load("pirana")
    shadow = load("shadow")

    print("════════ SHADOW A/B REPORT ════════")
    print("(živá SCALP vs stínová TIGHT, stejné vstupy)\n")

    for label, trades in [("ŽIVÁ (SCALP)", live), ("STÍN (TIGHT)", shadow)]:
        st = stats(trades)
        if not st:
            print(f"{label}: zatím žádná data")
            continue
        print(f"{label}:")
        print(f"  RT: {st['n']} | WR {st['wr']:.1f} % | payoff {st['payoff']:.2f}")
        print(f"  PnL: {st['pnl_sats']:+.1f} sats ({st['pnl_usd']:+.4f} USD)")
        print(f"  EV/RT: {st['ev_sats']:+.2f} sats\n")

    # Verdikt
    sl_ = stats(live)
    sh = stats(shadow)
    if sl_ and sh:
        if sl_["n"] < 50 or sh["n"] < 50:
            print(f"⚠️ Málo dat (živá {sl_['n']}, stín {sh['n']}) — verdikt až po 50+ RT každá.")
        diff = sl_["ev_sats"] - sh["ev_sats"]
        print(f"EV rozdíl (živá − stín): {diff:+.2f} sats/RT")
        if diff > 0:
            print("→ ŽIVÁ SCALP konfigurace vede. Neměnit.")
        else:
            print("→ STÍN TIGHT vede — po 200+ RT zvážit přepnutí (rozhoduje operátor).")
    elif sl_ and not sh:
        print("Stín zatím nemá uzavřené RT — čekáme na TP/SL zásahy.")
    else:
        print("Zatím žádná data — shadow sbírá od nasazení.")


if __name__ == "__main__":
    main()
