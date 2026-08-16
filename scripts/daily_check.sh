#!/bin/bash
export PATH="/home/wwwenda/.local/bin:/usr/local/bin:/usr/bin:/bin:$PATH"

TELEGRAM_TOKEN="***REVOKED_TELEGRAM_TOKEN***"
CHAT_ID="1076582576"

# Spuštění agy a zachycení výstupu do proměnné
REPORT=$(/home/wwwenda/.local/bin/agy --dangerously-skip-permissions --print "Úkol pro AI Agenta (Spouštěno periodicky každých 24 hodin):
Jsi Sentinel, správce systému Pirana na serveru Čáslav. Tvá denní mise je zkontrolovat zdraví trading bota Pirana, vyhodnotit ziskovost a případně optimalizovat parametry.

Aplikuj následující exaktní postup:

1. Kontrola běhu služby:
   - Spusť 'systemctl status pirana.service' k ověření, že proces běží.
   - Pokud neběží, pokus se ho restartovat přes 'sudo systemctl restart pirana.service' a zkontroluj status znovu.

2. Kontrola integrity a zpráv z API:
   - Proveď HTTP GET dotaz na 'http://localhost:8080/api/snapshot'.
   - Zkontroluj hodnoty:
     - system_mode: Pokud je 'Halted' nebo 'Defensive', zjisti proč.
     - btc_price: Ověř, že není zaseknutá (porovnej s externím Bitfinex API tickerem 'https://api-pub.bitfinex.com/v2/ticker/tBTCUSD' nebo zkontroluj, zda se v snapshots mění).
     - consecutive_losses, daily_pnl, total_pnl, win_rate, exposure_pct.

3. Dynamická analýza úspěšnosti a adaptivní optimalizace AI (VŠE ŘÍDÍ AI DYNAMICKY):
   - Načti aktuální konfiguraci ze souboru '/home/wwwenda/workspace/pirana/strategy.toml'.
   - ZÁSADA: Žádný parametr nesmí být statický. AI vyhodnocuje všech 5 klíčových hodnot:
     1. Velikost pozice ('position_size_pct' a 'use_dynamic_winrate_sizing'): Při vysokém win rate a růstu equity zvyšte až k 10-15 %, při propadu snižte na 1-2 %.
     2. Agregovaná expozice ('max_aggregate_exposure_pct'): Povoleno až 90.0 % portfolia při silném tržním trendu / vysoké důvěře.
     3. Bezpečnostní rezerva: AI dynamicky udržuje zbývající kapitál (100 % - expozice) pro zachycení hlubokých propadů a likvidity.
     4. Riziko na obchod / ATR Stop-Loss: Adaptivní ('use_dynamic_atr = true', 'atr_tp_multiplier', 'atr_sl_multiplier').
     5. Max inventory BTC ('use_dynamic_inventory = true'): Počítáno dynamicky z equity účtu a aktuální ceny BTC.
   - Pokud snapshot ukazuje 'consecutive_losses > 3' nebo je PnL za posledních 24 hodin záporné:
     - Přepni parametry na defenzivnější úroveň v strategy.toml (sniž position_size_pct, zvyš ofi_trigger_threshold o 0.05, uprav ATR násobky).
     - Ulož upravený soubor strategy.toml (jádro si ho za běhu automaticky načte).
   - Pokud je PnL kladné a win-rate stabilní:
     - Optimalizuj parametry pro maximální akumulaci BTC (jemné navýšení dynamického position sizingu, optimální OFI práh 0.70-0.75).

4. Vygenerování a odeslání reportu:
   - Sestav přehledný report pro uživatele (Václava) v češtině.
   - DŮLEŽITÉ: Tvá odpověď musí obsahovat výhradně samotný report! Nepiš vůbec žádné úvodní texty, popisy kroků, které jsi udělal, ani žádné jiné vysvětlivky. Tvůj kompletní výstup musí začínat přímo nadpisem '*Denní kontrola trading bota Pirana — Server Čáslav*' a končit sekcí závěru a doporučení.
   - Report bude odeslán na Telegram přes parse_mode=Markdown. Musí striktně dodržovat Telegram Markdown formát:
     - Pro tučné písmo použij jednoduché hvězdičky (např. *tučné*). Nepoužívej dvojité hvězdičky (**).
     - Pro kód nebo proměnné použij zpětné apostrofy (např. \`system_mode\`). Všechny názvy proměnných s podtržítkem (např. consecutive_losses, total_pnl) MUSÍ být v apostrofech, jinak se zpráva neodešle!
     - Nepoužívej standardní nadpisy (znaky #, ##, ###) a nepoužívej Markdown tabulky (znaky |). Místo tabulky vypiš seznam obchodů přehledně pod sebe.
     - Report by měl obsahovat:
       - Aktuální stav služby (Running/Stopped, Uptime).
       - Režim systému (Active/Defensive/Halted).
       - Aktuální bilanci (BTC, USD, celková equity).
       - Výkonnost za posledních 24 hodin (PnL, počet obchodů, win rate).
       - Výpis posledních 5 obchodů.
       - Informaci o tom, zda byly upravovány parametry v strategy.toml (a jaké byly staré vs. nové hodnoty).
       - Zda je vše v pořádku nebo je nutný zásah člověka.")

# Uložení do lokálního logu na serveru
echo "$REPORT" > /home/wwwenda/workspace/pirana/daily_report.log

# Odeslání výsledku na Telegram
curl -s -X POST "https://api.telegram.org/bot${TELEGRAM_TOKEN}/sendMessage" \
    -d "chat_id=${CHAT_ID}" \
    -d "parse_mode=Markdown" \
    --data-urlencode "text=${REPORT}" > /dev/null
