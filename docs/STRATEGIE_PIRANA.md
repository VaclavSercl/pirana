# ⚔️ PIRANA — Doktrína jediné strategie: Pullback Flow + Avellaneda-Stoikov

**Verze:** 3.0.0  
**Datum:** 20. září 2026  
**Účetní standard:** Bitcoin Standard (satoshi, $P(\text{ruin}) \to 0$)  
**Strategická směrnice operátora:** *V ostrém provozu běží výhradně jedna kmenová strategie (Pullback Flow) doplněná o vyvažování inventáře (Avellaneda-Stoikov). Veškeré stínové testování a výzkum se soustředí výhradně na iterace a varianty této základní strategie směrem k dokonalosti.*

---

## 1. Základní architektura: Jediná produkční osa

PIRANA neprovozuje portfolio nesouvisejících modelů. Celý systém je zúžen na jednu robustní, matematicky ověřenou osu mikrostruktury:

```
    ┌─────────────────────────────────────────────────────────────┐
    │          1. VSTUPNÍ STRATEGIE: PULLBACK FLOW                │
    │  - Čeká na mikro-korekci (dip >= 10 bps pod lokální HWM)    │
    │  - Vyžaduje potvrzenou nákupní převahu (current_flow > 0.05)│
    │  - Alokace: až 10 pozic (slotů) po 10 % equity              │
    └──────────────────────────────┬──────────────────────────────┘
                                   │
                                   ▼
    ┌─────────────────────────────────────────────────────────────┐
    │       2. VYVAŽOVÁNÍ INVENTÁŘE: AVELLANEDA-STOIKOV           │
    │  - Cílový inventář: 30 % volné equity v BTC                │
    │  - Dynamický skew nákupních/prodejních kotací dle odchylky  │
    │  - Rezervační cena: r(s, q, t) = s - q·γ·σ²·(T - t)         │
    └──────────────────────────────┬──────────────────────────────┘
                                   │
                                   ▼
    ┌─────────────────────────────────────────────────────────────┐
    │          3. ADAPTIVNÍ VÝSTUPY: DYNAMIC ATR TP               │
    │  - Dynamický ziskový cíl: 0.5 * ATR (clamp $10 - $120)      │
    │  - Bitcoin Standard Invariant: ZÁKAZ PRODEJE SE ZTRÁTOU     │
    │  - Žádná pozice se nikdy neprodá pod nákupní cenou          │
    └─────────────────────────────────────────────────────────────┘
```

---

## 2. Detailní specifikace ostrého jádra

### A. Vstupní model: Pullback Flow
- **Zdrojový kód:** `src/entry_policy.rs`, `strategy.toml`
- **Úloha:** Otevírání nových pozic.
- **Logika:**
  Trh osciluje i v silném trendu. Pullback Flow odmítá nakupovat na lokálních maximech. Čeká, až cena klesne pod lokální High-Water Mark ($\text{HWM}$), ale vstoupí pouze tehdy, když agresivní nákupní objednávky na pásce prokazují absorpci a převahu kupců.
- **Vstupní kritéria:**
  1. $\text{current\_flow} > 0.05$ (kladná balance taker objemů)
  2. $P_{\text{current}} < \text{HWM} \times 0.9990$ (sleva min. 10 bps)
  3. $\text{ActivePositions} < 10$ (volný slot z max 10)
  4. Velikost pozice = 10 % equity (`MAX_LIVE_BUY_EQUITY_FRACTION = 0.10`)
  5. Stop-loss: **Vypnut** (`stop_loss_enabled = false`)

### B. Vyvažování inventáře: Avellaneda-Stoikov (AS)
- **Zdrojový kód:** `crates/pirana-execution/src/avellaneda_stoikov.rs`, `strategy.toml`
- **Úloha:** Neustálá regulace expozice a optimální asymetrie spreadu.
- **Logika:**
  Držení satoshi má svůj cíl ($I_{\text{target}} = 30\,\%$ equity). Odchylka $q = I_{\text{real}} - I_{\text{target}}$ určuje asymetrii:
  - **Při přebytku BTC ($q > 0$):** Rezervační cena klesá pod tržní střed $\to$ systém tlačí prodejní limitní příkazy agresivněji k midu a nákupní bidy stahuje níže.
  - **Při nedostatku BTC ($q < 0$):** Rezervační cena roste $\to$ systém agresivněji poptává nákupy a brzdí prodeje.

### C. Výstupní brána: ATR Dynamic Exits & Bitcoin Standard
- **Zdrojový kód:** `strategy.toml`, `src/main.rs`
- **Úloha:** Realizace zisku a akumulace satoshi.
- **Logika:**
  - $\text{TakeProfit} = \text{EntryPrice} + \text{clamp}(0.5 \times \text{ATR},\; \$10.00,\; \$120.00)$
  - **Nepřekročitelný invariant:** $\text{ExitPrice} \ge \text{EntryPrice} + \text{Fees} + \text{MinTick}$.
  - Žádný stop-loss nesmí zlikvidovat satoshi do fiatu se ztrátou.

---

## 3. Výzkumná a stínová laboratoř: Výhradně varianty základní strategie

Všechny testovací a stínové experimenty v `shadow_experiments.jsonl` a R&D smyčce (§10) jsou **výhradně variantami a kalibracemi Pullback Flow + Stoikov**. Nesouvisející modely jsou vyřazeny.

### Matice testovaných variant Pullback Flow:

| Varianta | Název | Podmínka toku | Hloubka dipu (HWM) | Cíl a hypotéza |
|---|---|---|---|---|
| **V0 (Live)** | **Baseline Production** | `flow > 0.05` | 10 bps (`0.9990`) | Současný ověřený produkční standard. |
| **V1 (Shadow)** | **Deep Pullback** | `flow > 0.05` | 25 bps (`0.9975`) | Vstup pouze ve větších slevách. Snižuje frekvenci, zvyšuje průměrný zisk na obchod. |
| **V2 (Shadow)** | **Strong Momentum Flow** | `flow > 0.25` | 10 bps (`0.9990`) | Vyšší jistota nákupní dominance před vstupem. |
| **V3 (Shadow)** | **Volatility-Adaptive Dip** | `flow > 0.05` | $\text{dip} = f(\text{ATR})$ | Hloubka slevy se přizpůsobuje okamžitému šumu (v klidu 5 bps, ve vlnách 30 bps). |
| **V4 (Shadow)** | **Asymmetric Stoikov Skew** | `flow > 0.05` | 10 bps (`0.9990`) | Agresivnější $\gamma$ (averze k riziku) pro rychlejší návrat k 30% inventáři. |

### Pravidlo pro nasazení jakékoliv varianty do ostrého provozu:
1. **Minimálně 50 uzavřených round-tripů** ve stínovém záznamu.
2. **Kladná čistá expektace ($\text{Net EV} > 0$)** po započtení plných poplatků (min. 2 bps).
3. **Lepší Profit Factor nebo Win Rate** než má aktuální ostrá varianta V0 při $\Delta P(\text{ruin}) \le 0$.
4. Pokud varianta podmínky nesplní (jako např. dřívější pokus s extrémním `flow > 0.40`), je zamítnuta a systém zůstává na stabilní bázi.

---

## 4. Systémové brzdy (Pasivní ochrana, nikoli strategie)

Následující komponenty **nejsou samostatné strategie**, ale pasivní nouzové brzdy (circuit breakers) chránící kapitál:
- **VPIN Guard ($\text{VPIN} > 0.82$):** Detekce toxického toku informovaných institucí $\to$ stažení kotací do DEFENSIVE módu.
- **Hawkes Cascade Guard ($Z \ge 2.5$):** Detekce lavinových likvidačních kaskád $\to$ zákaz otevírání nových pozic během laviny.
- **Adaptive Cooldown:** Ochrana před přeobchodováním (churn) v bočním trhu.

---
*Dokument je závaznou směrnicí pro další rozvoj systému PIRANA na serveru caslav.*
