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
| 26.8.2026 | **Ranní audit provádí HERMES (instance Čáslava), nikoli agy.** agy zůstává jen jako oponent/verifikátor na vyžádání. |

---

## 📋 AKTIVNÍ ROZHODNUTÍ A STAV

| Položka | Hodnota | Kdo rozhodl | Poznámka |
|---|---|---|---|
| position_size_pct | **1.0 %** (dočasně) | ranní audit 26.8. | ⚠️ DOČASNÉ — čeká na rozhodnutí operátora (návrat na 20 % vs. ponechat) |
| min_position_size_pct | 1.0 % | ranní audit 26.8. | srženo z 5 % — podezřelé, mělo zůstat 5 % |
| Řešení sizingu | čeká na operátora | — | 3 varianty: 20 % / 1 % / 5 % kompromis |

---

## 🔄 PROTOCOL (co která instance dělala — číst PŘED prací, psát PO práci)

### 26.8.2026

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
