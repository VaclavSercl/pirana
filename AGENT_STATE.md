# 🧠 ČÁSLAV — SDÍLENÝ STAV AGENTA (jediná paměť všech instancí)

> **TOTO JE JEDINÁ PAMĚŤ VŠECH INSTANCÍ ČÁSLAVA.**
> Každá instance (WebUI session, ranní audit, Telegram bot) MUSÍ:
> 1. **PŘED prací** si tento soubor přečíst celý.
> 2. **PO práci** zapsat, co udělala, do sekce Protokol.
> 3. **Respektovat Rozhodnutí operátora** — ta jsou závazná pro všechny instance.

---

## ⚠️ ROZHODNUTÍ OPERÁTORA (závazná, mění jen operátor!)

| Datum | Rozhodnutí |
|---|---|
| 23.8.2026 | Sizing 20 % pozic = explicitní rozhodnutí. Nezpochybňovat bez nových důkazů o P(ruin). |
| 23.8.2026 | FSM smyčka (PLÁNOVAČ→ARCHITEKT→AUDITOR→TESTER→RECONCILER) je NEZKRACOVATELNÁ. |
| 26.8.2026 | Agent je JEDEN (Čáslav) ve více instancích. Všechny sdílejí tuto paměť. |
| 26.8.2026 | **AGY je POUZE kontrola / oponent / pomocný agent.** Nikdy vykonavatel! Veškerou práci (audity, reporty, nasazení, obchodní rozhodnutí) provádí Čáslav přes hermes. agy = jen druhý názor, verifikace závěrů, oponentura FSM. |

---

## 📋 AKTIVNÍ ROZHODNUTÍ A STAV

| Položka | Hodnota | Kdo rozhodl | Poznámka |
|---|---|---|---|
| position_size_pct | **1.0 %** (dočasně) | ranní audit 26.8. | ⚠️ DOČASNÉ — čeká na rozhodnutí operátora (návrat na 20 % vs. ponechat) |
| min_position_size_pct | 1.0 % | ranní audit 26.8. | srženo z 5 % — podezřelé, mělo zůstat 5 % |
| Řešení sizingu | čeká na operátora | — | 3 varianty: 20 % / 1 % / 5 % kompromis |

---

## 🔄 PROTOCOL (co která instance dělala — číst PŘED prací, psát PO práci)

### 30.8.2026

- **07:00 (ranní audit — Hermes CLI)**: Audit bez zásahu do TOML. Služba Active,
  uptime 3h49m (restart 03:11 po WebUI deployi z 10:30 včera). Stav: Active,
  consec_losses 1/5, WR 27,8 % (10/36 dnes), denní PnL −0,0139 USD (−0,0035 %),
  equity ~398,39 USD (start 398,20), BTC 0,00095 (inventář ~23 % equity),
  OFI snapshot 0,0 (signály 0,98), VPIN 33,4 % (15/50 košů, Low Toxicity),
  spread $12, markout_1s −5,1 bps / markout_30s −7,2 bps (negativní — SELL
  exekuce průměrně prodává pod signál, sledovat), slippage EWMA 1,09 bps
  (pod guard 5 bps). Lead-Lag disparity −$16,55 (BFX prémie, blízko prahu
  ±$15 → DistributionExit SELL signály DENIED od 04:58). Poslední trade
  04:58:48, režim RANGE. Sizing 1,0 % PONECHÁN — stále čeká na rozhodnutí
  operátora (3 varianty 20 %/1 %/5 %). Žádná změna TOML.

### 29.8.2026

- **17:28 (ranní/odpolední audit — Hermes CLI)**: Audit bez zásahu do TOML. Služba
  aktivní, ale uptime pouze 33 s (restart 17:27:50 — bot právě nastartoval, po
  WebUI deployi z 10:30). Stav: Active, consec_losses 0/5, denní PnL 0,0 (0 trades
  — den resetován restartem), equity ~398,04 USD (start 398,04), BTC 0,000886,
  OFI 0,0, VPIN 5,4 % (warming up, 0/10 košů — nespolehlivé), spread $7, markout
  0,0, slippage 0,0. Lead-Lag disparity +$4,86 (v prahu ±$15). Signál
  SpreadCapture conf 0,95 DENIED nespuštěn (nový běh). Kalibrace risk engine:
  SEED (n=0) — Treasury kalibrace zatím bez dat. Sizing 1,0 % PONECHÁN — stále
  čeká na rozhodnutí operátora (3 varianty 20 %/1 %/5 %). Žádná změna TOML.
- **10:30 (WebUI session — BODY 1-3 „vydělávat i v tomto trhu")**: Analýza ukázala,
  že hlavní krvácení nebylo trading (JSONL: +0.27/+0.15/+0.04/+0.02 USD denně!),
  ale INVENTÁŘ (49 % equity v BTC = −4.66 USD/noc při poklesu 2.4 %, 13× víc
  než trading) + špatné accountování. Implementace:
  BOD 1 (P0 accountování): signálová SELL cesta nezapisovala RT do JSONL
  (jen TP/SL cesta, ~38 % dat!) — nyní symetricky; position_id (AtomicU64
  counter) nahradil křehký match (entry_price, quantity) pro korekci
  entry_price na reálný fill (2 místa: korekce + rollback).
  BOD 2 (režimový inventářní strop): calculate_regime_inventory_btc —
  Range 20 %, TrendDown/rolling-PnL-záporný 10 %, TrendUp 35 % equity
  (dříve 90 % aggregate limit!). Wiring: BUY gate + REBALANCE deadlock gate.
  BOD 3 (TP/SL asymetrie): dříve TP 0.4×ATR vs SL 2.5×ATR (6:1 ve prospěch
  ztrát, SL floor 350 USD!) s payoff 0.18. Nově TP 0.8×ATR, SL 1.5×ATR,
  min_sl 150 USD — asymetrie 1.9:1 pro výhry, breakeven WR ~35 %.
  Živě po deployi: inventář 49 % → 10 % equity, režim TREND-DOWN detekován.
  Testy: 24 sad workspace 0 failed, +6 regime_inventory. Commit 904ddd7.
  Předtím: konzistenční rolling brake (WR<10 % na 30 RT, oponentura 2×P0:
  self-release mid-streak, rehydrate řazení) — commit 9b0de07.

- **07:00 (ranní audit — Hermes CLI)**: Audit bez zásahu do TOML. Stav: Active,
  consec_losses 0/5, WR 25,8 % (17/66 dnes), denní PnL −0,083 USD (−0,021 %),
  equity ~398,61 USD (start 398,70), BTC 0,002511, OFI=0.0, VPIN 62,3 % (moderátní,
  40/50 košů), spread $7, markout_1s +1,86 bps, markout_30s +2,15 bps, slippage EWMA
  0,85 bps. Uptime 4h41m. Lead-Lag disparity +$7,93 (v prahu ±$15). Sizing 1,0 %
  PONECHÁN — stále čeká na rozhodnutí operátora (3 varianty 20 %/1 %/5 %).
  Trezor: 3,8e-8 BTC (profit skimmer prakticky nulový — zisky zatím nepokrývají
  miner fee/zaokrouhlení). Žádná změna TOML.

### 28.8.2026

- **07:00 (ranní audit — Hermes CLI)**: Audit bez zásahu do TOML. Stav: Defensive,
  consec_losses 6/5 (seed práh), WR 0,0 % (0/6 dnes), denní PnL −0,0127 USD (−0,003 %),
  equity 406,05 USD (start 406,06), BTC 0,004509, OFI=0.0, VPIN 53,9 % (nedostatek dat
  1/10 košů), spread $16, markout_1s −3,5 bps, markout_30s +2,25 bps, slippage EWMA 0,89 bps.
  Uptime 17 min (restart 06:43:46). Poslední trade 04:48:27 — od té doby všechny signály
  DENIED (Defensive). Sizing 1,0 % PONECHÁN — čeká na rozhodnutí operátora (3 varianty
  20 %/1 %/5 %). Defenzivní půlení na 0,5 % nelze — podlaha min_position_size_pct=1,0.
  ⚠️ NÁLEZ (3. den v řadě): consec_losses=6 překračuje seed práh 5, ale FSM hlásí jen
  Defensive, ne Halted — logika přechodu Defensive→Halted v pirana-risk-engine stále
  neověřena. Doporučeno operátorovi: ověřit prahovou logiku FSM. Žádná změna TOML
  (validace strategy_versioning.py: OK).

### 27.8.2026

- **07:00 (ranní audit — Hermes WebUI)**: Audit bez zásahu do TOML. Stav: Defensive,
  consec_losses 7/5 (seed práh), WR 45,7 % (16/35), denní PnL +0,007 USD (+0,002 %),
  equity 402,57 USD (start 402,56), OFI=0.0, VPIN 34,3 % (nedostatek dat 3/10 košů),
  spread $11, markout_1s +2,81 bps, markout_30s -5,56 bps, slippage EWMA 0,77 bps.
  Bot v Defensive modu — všechny signály DENIED (SpreadCapture, DistributionExit).
  Poslední trade 04:50:35. Sizing 1,0 % PONECHÁN — čeká na rozhodnutí operátora
  (3 varianty 20 %/1 %/5 %). Defenzivní půlení na 0,5 % nelze — podlaha min_position_size_pct=1,0.
  ⚠️ NÁLEZ: consecutive_losses=7 překračuje seed práh 5, ale FSM hlásí jen Defensive, ne Halted —
  stejný problém jako 26.8. Žádná změna TOML.

### 26.8.2026

- **15:53 (Telegram bot)**: Ověřen TTS Piper — provider `piper`, hlas `cs_CZ-kasandra-medium`
  (soubory `/home/wwwenda/.local/share/piper/voices/cs_CZ-kasandra-medium.onnx` + `.json`).
  Test syntézy: exit 0, WAV 136 KB. Config `~/.hermes/config.yaml` již obsahuje `tts.provider: piper`
  a `tts.piper.voice: cs_CZ-kasandra-medium`. Vše funkční, žádná změna nebyla nutná.
- **11:57 (ranní audit — Hermes WebUI)**: Audit bez zásahu do TOML. Stav: Defensive,
  consec_losses 6/5(seed práh), WR 22,7 % (15/66), denní PnL -0,063 USD (-0,016 %),
  equity 402,07 USD (start 402,76), OFI=0.0, VPIN 41,0 %, markout_1s -7,8 bps (záporný
  trend exekuce). Sizing 1,0 % PONECHÁN — čeká na rozhodnutí operátora (3 varianty
  20 %/1 %/5 %). Defenzivní půlení na 0,5 % nelze — podlaha min_position_size_pct=1,0.
  ⚠️ POZOR: consec_losses=6 překračuje seed práh 5, ale FSM hlásí jen Defensive, ne
  Halted — ověřit logiku přechodu Defensive→Halted v pirana-risk-engine.
- **10:15 (WebUI session)**: Analýza sizingu — Kelly f* = −38 % (negativní edge na 28
  round-tripech, win rate 14 %). Doporučeno nechat 1 % dokud edge kladný. Vysvětleno
  operátorovi, že sizing srazil ranní audit (instance agy) — prompt ve skriptu to
  přikazoval. OPERÁTOR REAGOVAL: „Agent musí být jeden!" → zřízena tato sdílená paměť.
- **10:20 (WebUI session)**: Opraven prompt daily_check.sh — ranní audit NESMÍ trvale
  srážet sizing; max dočasné snížení na polovinu, vždy [NEOVĚŘENO] + žádost o potvrzení.
  Commit `e979c5b`.
- **09:36 (WebUI session)**: FILL TRUTH oprava — autoritativní fill z /trades/hist
  (ACK price_avg = limit cena!). Slippage EWMA 4.94 → 0.92 bps. Commit `1ac99f1`.
- **07:01 (ranní audit — instance agy)**: ⚠️ Srazil position_size_pct 20 % → 1 % a
  min_position_size_pct 5 % → 1 % bez vědomí operátora. Prompt to přikazoval (nyní opraven).

- **17:53 (WebUI session — večerní audit po rebootu)**: Audit po manuálním `sudo reboot`
  operátora v 17:40 (dnes 12. boot — kumulativní statistiky přežívají v
  `/var/lib/pirana/trade_ledger.jsonl`, 394 round-tripů). Stav: Active, consec_losses 0,
  equity 400,10 USD, denní PnL (ledger): -144,5 sats (-0,113 USD, 282 RT, WR 33 %),
  ale posledních 30 RT: WR 60 %, +33,2 sats — edge se po přepnutí na Lead-Lag
  front-run zlepšuje. ⚠️ NÁLEZ 1: trailing-stop exekuce používá `EXCHANGE MARKET`
  (main.rs:1034) — odchylka od konvence č.3 (vždy EXCHANGE IOC ± 5 bps); dopad malý
  (poplatky 0 %), ale zaznamenáno ke sjednocení. ⚠️ NÁLEZ 2: START RECONCILIATION po
  každém restartu vidí BTC na burze bez pozic — řeší REBALANCE SELL, známé.
  Sizing ponechán 1,0 % — čeká na operátora (3 varianty). Žádná změna TOML.

### 25.8.2026

- **(WebUI session)**: Slippage P0+P1+P2 — guard, IOC limit, telemetrie. Oponentura agy:
  vwap() prohozené strany (P0!), IOC parser falešný 100 % fill (P0). Commity `439c322`,
  `1bb1635`. Deadlock odblokován (REBALANCE SELL).

---

## 🔧 TECHNICKÉ KONVENTE (pro všechny instance)

1. **Fill cena**: ACK `on-req` price_avg = LIMIT cena, ne fill. Autoritativní fill =
   `resolve_fill()` z `/trades/hist` přes order_id. Nikdy neúčtovat PnL z ACK.
2. **SLIPPAGE GUARD**: max 5 bps (VWAP knihy vs signál), skip při překročení.
3. **IOC order**: vždy `EXCHANGE IOC` s limitem signál ± 5 bps, nikdy MARKET.
4. **FSM**: každá změna = PLÁNOVAČ → ARCHITEKT → AUDITOR → TESTER → RECONCILER.
5. **Oponentura agy**: povinná před commitem netriviálních změn.
6. **Sizing**: ROZHODNUTÍ OPERÁTORA (viz výše). Instance navrhují, operátor rozhoduje.
