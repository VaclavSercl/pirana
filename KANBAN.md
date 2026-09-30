# 📋 KANBAN — Plán náročných úloh PIRANA

> Sdílený plán všech instancí Čáslava. Vícefázové úlohy sem — ne do hlavy.
> Pravidlo: každá fáze má Definition of Done + ověřitelný výsledek.
> Před prací si přečti AGENT_STATE.md, po práci zapiš PROTOCOL zápis.

## 🔥 IN PROGRESS

### Telegram incidents and LightGBM pilot — 2026-09-30
| Phase | Goal | Status | Definition of Done |
|---|---|---|---|
| A | Diagnose calibration notification and post-update recovery halt | IN PROGRESS | Root cause, regression, independent Hermes/Agy review, verified repair |
| B | Validated public L2 dataset and chronological experiment | APPROVED NEXT | Sequence/checksum/gap audit; reproducible features and executable labels |
| C | Isolated LightGBM comparison and shadow observer | APPROVED NEXT | Dependencies explicitly authorized; no order access; measured results and limitations |


### T1 — Testovací pyramida ✅ KOMPLETNÍ (29.8.–3.9.)

**Cíl:** Chyby odhalovat PŘED nasazením, ne po týdnu provozu.
Replay na historii místo „nasadíme a doufáme".

| Fáze | Úloha | Termín | Stav | Definition of Done |
|---|---|---|---|---|
| **A** | Replay/Backtest engine — `scripts/replay_tp_sl.py`: 5m candles (14 dní), ATR/TP/SL/trailing věrné, sweep 40 kombinací | **29. 8.** | ✅ HOTovo | Engine funguje; sweep: žádná TP/SL kombinace nemá kladné EV (−0.0008 až −0.0023/RT); TP 0.4/SL 0.5 nejlepší, ale rozdíl kosmetický — problém je VE VSTUPU, ne v exitech. Momentum vstup +0.19 %/RT vs dip −0.05 %. ⚠️ Simulovaný vstup ≠ reálné flow signály → Fáze B to opraví |
| **B** | Replay z reálných signálů ✅ + shadow mode runtime ✅ | **30. 8.** | ✅ | **Replay hotov** (`scripts/replay_real_signals.py`, 447 reálných vstupů): 1) všechny TP/SL konfigurace záporné EV, ale SCALP (těsné 0.3/0.4, TP≤60, SL≤100) je 6× méně ztrátový než STARÁ (−0.13 vs −0.85); 2) momentum vstupy > dip na všech konfiguracích; 3) noční EV kladné ale statisticky neprokázané (CI zahrnuje 0) — sbíráme data. **Zbývá**: shadow mode v runtime (paper obchody s alternativní konfigurací, cid shadow_) |
| **C** | Chaos testy — `scripts/chaos_tests.sh` | **3. 9.** | ✅ | **6 PASS / 0 FAIL** (3.9.): restart → active + rehydratace (1 857 RT) + persistence; outage → obnova; poškozený JSONL → 1 859 valid, 4 skipped. Pozn.: doctor detekce neměřitelná v 70s okně (timer 10 min) — akceptováno. sample_size v API = 0 je kosmetické (view po 1. rekalibraci) |

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

## Codex audit continuation 2026-09-23

| Phase | State | Acceptance evidence |
|---|---|---|
| Audit repairs and IOC recovery root cause | VERIFIED_NATIVE | 455 Rust tests, 210 Python tests, check and strict root Clippy passed on Caslav |
| Container release and publication | IN_PROGRESS | Compose and exact-candidate Docker release passed; Git checkpoint/publication pending |
| Production reconciliation | PENDING | Backup, exact binary digest, fresh accounting, pending-intent recovery and healthy service required |
| Hourly oversight | CONFIGURED | Codex thread heartbeat; live authenticated baseline stored privately on server |

## LightGBM optional research — 2026-09-30
- Installedisolatedruntime and completedfrozenexperiment: noadvantage demonstrated; livepromotionREJECTED.
-31researchtestsPASS; bounded600sno-tradingforwardpilotPASS (97freshobservations, automaticstop,32independentlyreplayedexactfeatures/predictions). Exactcandidatecheckpointexpectedonlyafterfinalgate.
- Privateevidence: telegram-lightgbm-20260930/approved-pilot. No productionbinary/risk/Telegramchanges.

## Completion1–4 — 2026-09-30
|Phase|State|Acceptance|
|---|---|---|
|1daily08:00|DEPLOYED_OFFLINE_VERIFIED|next08 verified; one extra catchup and false alert documented; delivery-status fix installed, first natural08 run still pending|
|2performance|VERIFIED_WITH_LIMITATIONS|fresh canonical and continuous forward window; old gaps and external basis remain unknown|
|3audits|VERIFIED_WITH_LIMITATIONS|OSV four pinned versions, actual Agy and Hermes reviews; no unknown-vulnerability guarantee|
|4decisiondataset|IMPLEMENTED_RELEASE_BUILT|all evaluated decisions and rejected candidates recorded; native/Docker builds and preliminary499Rust/362Python/43ML passed; exact gate/deployment acceptance stored privately|

Completion transaction: current source/checkpoint/deployment evidence is under private completion-20260930 and common Git codex-audit-20260923. Older dated tables above are historical snapshots, not fresh verification. Final research status validates only a recorded prefix; future data and inventory/exit replay remain unqualified. No live model orders or new remote publication.
