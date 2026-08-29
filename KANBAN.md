# 📋 KANBAN — Plán náročných úloh PIRANA

> Sdílený plán všech instancí Čáslava. Vícefázové úlohy sem — ne do hlavy.
> Pravidlo: každá fáze má Definition of Done + ověřitelný výsledek.
> Před prací si přečti AGENT_STATE.md, po práci zapiš PROTOCOL zápis.

## 🔥 IN PROGRESS

### T1 — Testovací pyramida (rozhodnutí operátora 29. 8.)

**Cíl:** Chyby odhalovat PŘED nasazením, ne po týdnu provozu.
Replay na historii místo „nasadíme a doufáme".

| Fáze | Úloha | Termín | Stav | Definition of Done |
|---|---|---|---|---|
| **A** | Replay/Backtest engine — `scripts/replay_tp_sl.py`: 5m candles (14 dní), ATR/TP/SL/trailing věrné, sweep 40 kombinací | **29. 8.** | ✅ HOTovo | Engine funguje; sweep: žádná TP/SL kombinace nemá kladné EV (−0.0008 až −0.0023/RT); TP 0.4/SL 0.5 nejlepší, ale rozdíl kosmetický — problém je VE VSTUPU, ne v exitech. Momentum vstup +0.19 %/RT vs dip −0.05 %. ⚠️ Simulovaný vstup ≠ reálné flow signály → Fáze B to opraví |
| **B** | Shadow/A-B mode + replay z REÁLNÝCH signálů — (1) replay: vezme zaznamenané signály z journalu (ts+cena každého vstupu) a na nich testuje TP/SL varianty = věrný A/B bez aproximačního vstupního signálu; (2) shadow: nová konfigurace paper-obchoduje na živých cenách WS, stará reálně | **30. 8. (zítra)** | ⏳ | Replay na reálných vstupech: verdikt A vs B; shadow paper RT zapisovány zvlášť do JSONL (cid prefix shadow_) |
| **C** | Chaos testy — restart mid-order, API outage simulace, poškozený JSONL → restart → robustnost | **31. 8. (pozítří)** | ⏳ | Skript/doctor mód, který každý chaos scénář provede a vyhodnotí PASS/FAIL |

**Ověřovací otázky pro Fázi A (od operátora):**
- Je nová TP/SL asymetrie lepší než stará na 14 dnech? (číslo, ne názor)
- Jaké WR potřebuje nová konfigurace k breakeven na každém ATR režimu?

---

## 📦 BACKLOG (návrhy čekající na rozhodnutí operátora)

- **[z Fáze A] Strategie mean-reverze vs momentum**: replay ukázal +0.19 %/RT
  pro momentum vstup vs −0.05 % dip-buying na 14 dnech trendového trhu.
  Naše flow signály (OFI/lead-lag) jsou hybrid. Zvážit režimově-podmíněný
  vstup: TREND-UP → momentum/pullback, RANGE → mean-reverze.
- **[z Fáze A] TP vzdálenost**: nasazená TP 1.8×ATR (= +170 USD reálně!)
  nedosahovatelná — vše končí trailing. Kandidát: TP 0.4–0.6×ATR.
  Čeká na Fázi B replay s reálnými vstupy pro rozhodnutí.

- **Latency izolace API klíčů** (oponentura 26. 8.): read-only klíč pro telemetrii,
  oddělený submit_mutex — čtení historie dnes blokuje exekuci na mutexu
- **REST vs WS auth fills** (P2 dluh): `resolve_fill` polling vs WS `te`/`tu` kanály
- **fee_sats** v ledgeru (P2): zero-fee burza, ale struktura připravit
- **Race BUG-4** (oponentura 29. 8.): pozice removovaná před BUY fill korekcí —
  in-flight registr / PendingFill stav
- **main.rs god-file** (~2 600 řádků): rozdělit na moduly

## ✅ DONE (poslední 3)

- **TP/SL asymetrie P0 fix** (29. 8.): oponentura odhalila obrácenou asymetrii
  (breakeven WR 88 %!) → TP 1.8×ATR / SL 0.7×ATR, payoff 1.5–2.6:1. `e84de86`
- **Body 1-2-3 „vydělávat v tomto trhu"** (29. 8.): JSONL SELL persistence +
  position_id robustní match; režimový inventářní strop 10/20/35 %; TP/SL asymetrie. `904ddd7`
- **Konzistenční rolling brake** (29. 8.): WR < 10 % na 30 RT → engage; oponentura
  2× P0 (self-release mid-streak, rehydrate řazení). `9b0de07`
