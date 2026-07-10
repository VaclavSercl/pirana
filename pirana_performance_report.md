# Výkonnostní a obchodní analýza: HFT bot Pirana

Tento analytický report přináší detailní vyhodnocení výkonnosti, obchodních statistik a aktuálního stavu vysokofrekvenčního obchodního bota **Pirana** na serveru `caslav` za období od **21. května 2026 do 11. června 2026**.

Analýza byla sestavena na základě:
1. **Analýzy systémových logů** služby `pirana.service` (zpracováno celkem **1 185 162 logovacích řádků**).
2. **Přímého dotazu na API burzy Bitfinex** (zjištění reálného stavu peněženek, otevřených objednávek a pozic).
3. **Interního API snapshotu** běžícího bota (`http://localhost:80/api/snapshot`).

---

## 1. Aktuální stav bota a serveru

- **Status služby:** ✅ **Aktivní a běží (Active - running)** pod systemd jako `/home/wwwenda/workspace/pirana/target/release/pirana`.
- **Režim obchodování:** `Active` (detekuje signály a odesílá reálné objednávky na Bitfinex).
- **Uptime aktuální relace:** **2 hodiny 11 minut** (dnes kolem 12:00 CEST proběhl restart serveru `caslav`, bot se po startu automaticky spustil a okamžitě se připojil k trhu).
- **Síťové připojení:** Zabezpečeno přes Tailscale VPN (lokální IP serveru: `10.0.1.197`, veřejná VPN IP: `100.115.0.40`).

---

## 2. Výsledky reálného obchodování (Real Trading)
*Statistiky jsou spočteny ze všech 45 úspěšně uzavřených pozic v historii logu (od 21. 5. 2026).*

| Metrika | Hodnota | Poznámka |
| :--- | :--- | :--- |
| **Počet uzavřených obchodů** | **45** | Pouze kompletně uzavřené cykly (nákup + prodej). |
| **Celkový čistý zisk (Net PnL)** | **+0.55 USD** | Bot je v mírném zisku i přes nízkou úspěšnost. |
| **Hrubý zisk (Gross Profit)** | **+1.77 USD** | Suma zisků ze 13 úspěšných obchodů. |
| **Hrubá ztráta (Gross Loss)** | **-1.22 USD** | Suma ztrát ze 32 neúspěšných obchodů. |
| **Úspěšnost (Win Rate)** | **28.89 %** | 13 ziskových vs. 32 ztrátových obchodů. |
| **Profit Factor** | **1.46** | Velmi zdravý poměr ($1.46 vyděláno na každých $1.00 ztráty). |
| **Průměrný zisk na obchod** | **+0.0122 USD** | Průměrný výsledek na jeden uzavřený cyklus. |
| **Průměrný zisk (Win)** | **+0.1362 USD** | Průměrný ziskový obchod. |
| **Průměrná ztráta (Loss)** | **-0.0381 USD** | Průměrný ztrátový obchod. |
| **Risk/Reward Ratio (RRR)** | **3.58** | Průměrný zisk je 3,58× větší než průměrná ztráta. |
| **Nejlepší obchod** | **+0.1700 USD** | Maximální zisk na jeden obchod. |
| **Nejhorší obchod** | **-0.0600 USD** | Maximální realizovaná ztráta na jeden obchod. |

> [!TIP]
> **Klíč k profitabilitě bota:** Navzdory nízké úspěšnosti (Win Rate pod 30 %) je bot ziskový díky velmi asymetrickému poměru zisku a ztráty (RRR = 3.58). Risk Engine velmi rychle utíná ztráty na nízkých hodnotách (kolem 3-4 centů), zatímco ziskové obchody nechává běžet k cíli (kolem 12-17 centů).

---

## 3. Výsledky stínového obchodování (Paper Trading)
*Papírové obchody se spouštějí automaticky v režimu `Halted` (při zvýšeném riziku nebo po sérii ztrát).*

- **Počet uzavřených obchodů:** **10**
- **Celkový zisk na papíře:** **+0.84 USD** (8 ziskových / 2 ztrátové)
- **Úspěšnost (Win Rate):** **80.0 %**
- **Profit Factor:** **8.64** (gross profit 0.95 USD vs. gross loss 0.11 USD)
- **Nejlepší / Nejhorší obchod:** +0.12 USD / -0.06 USD
- **Analýza rozdílu (Real vs. Paper):** Výrazně lepší výsledky na papíře napovídají, že:
  1. V době, kdy je bot v defenzivním režimu (`Halted`), generuje strategie velmi přesné signály.
  2. Reálné obchody na burze mohou trpět na tržní skluz (slippage) a poplatky, které papírový režim simuluje jednodušeji.

---

## 4. Aktuální finanční stav na burze Bitfinex (k 11. 6. 2026)

Účet bota je veden jako **spotový (Exchange wallet)**. Bot nemá otevřené žádné pákové/margin pozice, veškeré obchody probíhají nákupem a prodejem fyzického BTC.

### Zůstatky na peněženkách (Wallets):
- **USD zůstatek (Celkový):** **379.88 USD**
- **USD zůstatek (Volný):** **222.09 USD** (zbytek je blokován v aktivní objednávce)
- **BTC zůstatek (Celkový & Volný):** **0.000372 BTC** (hodnota cca **23.47 USD** při ceně $63 088)
- **Ostatní zůstatky:** 0.05 UST, 0.00 ETH (zanedbatelné)
- **Aktuální celková hodnota (Equity):** **403.40 USD**

### Dnešní obchodní seance (od restartu ve 12:01 CEST):
- **Počáteční equity:** **403.62 USD**
- **Aktuální equity:** **403.40 USD**
- **Čistá dnešní změna:** **-0.22 USD (-0.05 %)**
- **Počet transakcí dnes ( trades_today ):** **236** (zahrnuje odeslané a vyplněné nákupní/prodejní market příkazy)
- **Aktuální otevřené objednávky (Open Orders):**
  Na burze je aktivní **jedna limitní nákupní objednávka**:
  - **Typ:** `EXCHANGE LIMIT` (Nákup)
  - **Objem:** `0.00250102 BTC` (odpovídá blokované částce **157.78 USD**)
  - **Cena:** `63 088.00 USD`
  - **Poznámka:** Tuto limitní objednávku musel zadat uživatel manuálně přes rozhraní burzy, protože bot Pirana je v kódu navržen výhradně pro zadávání `MARKET` objednávek.

---

## 5. Zjištěné technické a logické nedostatky

Během analýzy kódu a logů byly odhaleny tři důležité body k optimalizaci:

### A. Zamítání objednávek burzou (Chyba API 500)
V logu se opakovaně vyskytuje chyba:
`Bitfinex asynchronous BUY order execution failed: Exchange API error: 500 - ["error",10001,"Invalid order: minimum size for BTCUSD is 0.00004"]`
- **Příčina:** Risk Engine bota dynamicky upravuje velikost objednávek podle volatility a aktuálního kapitálu. Vypočtenou velikost clampuje na spodní hranici `0.00001` BTC. Bitfinex však pro pár BTC/USD vyžaduje minimální velikost **0.00004 BTC** (cca 2.5 USD). Pokud Risk Engine pošle menší velikost, burza ji odmítne.
- **Doporučení:** V `src/main.rs` upravit clampování minimální velikosti z `0.00001` na `0.00004` BTC.

### B. Extrémní spamování logů (Zahlcování disku)
Za 3 týdny provozu vygenerovala služba přes **1.18 milionu řádků** logu.
- **Příčina:** Téměř 99 % logů tvoří varování:
  `WARN ThreadId(05) pirana: src/main.rs:761: Min BTC inventory reached (0.000052), skipping SELL`
  Protože se jedná o vysokofrekvenční bot (HFT), při prodejním tlaku kontroluje signál na každém ticku (může jít o desítky ticků za sekundu). Pokud je stav BTC pod limitem `min_inventory_btc` (v TOML nastaveno na `0.0002`), bot zprávu vypíše při každém vyhodnocení.
- **Doporučení:** Implementovat logovací cooldown (throttling), aby se tato zpráva vypisovala maximálně jednou za 5-10 minut, nikoli při každém ticku.

### C. Prázdné statistiky na Dashboardu
Interní API snapshot z dashboardu obsahuje položky `"win_rate": 0.0`, `"best_trade": 0.0`, `"worst_trade": 0.0` a `"avg_trade_size": 0.0`.
- **Příčina:** Tyto hodnoty jsou v paměťové struktuře `DashboardState` definovány, ale v hlavním kódu exekučního vlákna v `src/main.rs` se po uzavření obchodu vůbec neaktualizují (kód zapisuje pouze `daily_pnl`, `total_pnl` a `trades_today`).
- **Doporučení:** Dopsat do exekuční logiky v Rustu výpočet a aktualizaci těchto hodnot do `DashboardState`, aby dashboard zobrazoval reálné statistiky v reálném čase.
