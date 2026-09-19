#!/usr/bin/env python3
"""Entry Markout Report [BOD 2 — 4.9.] — kvalita vstupů z tick history.

Měří pohyb ceny +30/+60/+300 s po každém reálném BUY fillu:
  markout = (cena_v_T − fill) / fill × 10⁴ bps

Negativní markouty = kupujeme drahá (adverse selection).
Pozitivní = vstup má edge.

Data: /var/lib/pirana/tick_history.jsonl (od 4.9. 06:51) +
journal fills (Authoritative fill).

Usage: sudo python3 scripts/entry_markout.py
"""

import json
import re
import subprocess
import sys
from bisect import bisect_left


def load_ticks():
    ticks = []
    with open("/var/lib/pirana/tick_history.jsonl") as f:
        for line in f:
            try:
                t = json.loads(line)
                ticks.append((t["ms"], t["p"]))
            except Exception:
                pass
    ticks.sort()
    return ticks


def load_fills():
    out = subprocess.run(
        ["sudo", "-n", "journalctl", "-u", "pirana.service", "--no-pager"],
        capture_output=True, text=True,
    ).stdout
    # pouze naše BUY filly: Authoritative fill (BUY cesta loguje slippage)
    pat = re.compile(r"^(\w+ \d+ \d\d:\d\d:\d\d).*Asynchronous BUY order executed! Authoritative fill: ([\d.]+) USD")
    fills = []
    for line in out.splitlines():
        m = pat.match(line)
        if m:
            import time as _t
            ts = _t.mktime(_t.strptime(f"2026 {m.group(1)}", "%Y %b %d %H:%M:%S"))
            fills.append((int(ts * 1000), float(m.group(2))))
    # dedupe: paralelní signály ve stejné sekundě → jeden
    dedup = []
    for ms, p in fills:
        if dedup and ms - dedup[-1][0] < 2000 and abs(p - dedup[-1][1]) < 1.0:
            continue
        dedup.append((ms, p))
    return dedup


def price_at(ticks_ms, ticks_p, at_ms):
    """Cena nejblíže času at_ms (interp. ne — nejbližší tick)."""
    i = bisect_left(ticks_ms, at_ms)
    if i >= len(ticks_ms):
        i = len(ticks_ms) - 1
    return ticks_p[i]


def main():
    ticks = load_ticks()
    fills = load_fills()
    if not ticks:
        print("Žádná tick data — čekáme na sběr.")
        return
    tick_ms = [t[0] for t in ticks]
    tick_p = [t[1] for t in ticks]
    # jen filly PO startu tick recorderu
    fills = [(ms, p) for ms, p in fills if ms >= tick_ms[0]]
    print(f"Ticků: {len(ticks)} | Reálných BUY fillů s tick daty: {len(fills)}")
    if not fills:
        print("Žádné filly v rozsahu tick dat.")
        return

    windows = [30_000, 60_000, 300_000]
    results = {w: [] for w in windows}
    for ms, fill_price in fills:
        for w in windows:
            if ms + w > tick_ms[-1]:
                continue  # okno přesahuje data
            p = price_at(tick_ms, tick_p, ms + w)
            markout_bps = (p - fill_price) / fill_price * 10_000
            results[w].append(markout_bps)

    print()
    print("ENTRY MARKOUT (bps — kladný = cena po vstupu rostla):")
    for w in windows:
        rs = results[w]
        if not rs:
            print(f"  +{w//1000:3d}s: (málo dat)")
            continue
        avg = sum(rs) / len(rs)
        pos = sum(1 for r in rs if r > 0)
        print(f"  +{w//1000:3d}s: n={len(rs):3d} | avg {avg:+6.2f} bps | "
              f"kladných {pos}/{len(rs)} ({pos/len(rs)*100:.0f} %)")

    print()
    avg60 = sum(results[60_000]) / len(results[60_000]) if results[60_000] else 0
    if avg60 < -0.5:
        print("🔴 Vstupy mají NEGATIVNÍ markout — kupujeme drahá (adverse selection).")
        print("   → Bod 1 (pullback filtr) by měl zlepšit; re-test po 100+ vstupech.")
    elif avg60 > 0.5:
        print("🟢 Vstupy mají kladný markout — edge na vstupu přítomen.")
    else:
        print("🟡 Markout neutrální — sbíráme další data.")


if __name__ == "__main__":
    main()
