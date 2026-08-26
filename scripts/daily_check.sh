#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# ČÁSLAV :: AUTONOMOUS MORNING AUDIT & REPORT DISPATCHER
# ==============================================================================

export PATH="/home/wwwenda/.local/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
WORKSPACE_DIR="/home/wwwenda/workspace/pirana"
ENV_FILE="${WORKSPACE_DIR}/.env"
LOG_FILE="${WORKSPACE_DIR}/logs/daily_report.log"
mkdir -p "${WORKSPACE_DIR}/logs"

# 1. Načtení proměnných prostředí
if [ -f "$ENV_FILE" ]; then
    TELEGRAM_TOKEN=$(grep -E '^TELEGRAM_BOT_TOKEN=' "$ENV_FILE" | cut -d '=' -f2- | tr -d '[:space:]"' | tr -d '\047')
    CHAT_ID=$(grep -E '^TELEGRAM_CHAT_ID=' "$ENV_FILE" | cut -d '=' -f2- | tr -d '[:space:]"' | tr -d '\047')
fi

# Fallback tokeny
TELEGRAM_TOKEN="${TELEGRAM_TOKEN:?chybi promenna TELEGRAM_TOKEN}"
CHAT_ID="${CHAT_ID:-1076582576}"

# 2. Definice promptu pro Agenta Čáslav
PROMPT_CONTENT=$(cat << 'EOF'
Úkol pro AI Agenta (Autonomní denní audit a optimalizace systému Pirana):
Jsi ČÁSLAV – svrchovaný správce serveru, kvantitativní architekt a institucionální exekutor. Tvou primární misí je systematická akumulace fyzického Bitcoinu (The Bitcoin Accumulation Mandate) a každodenní hloubkový audit v 07:00.

Postupuj podle následujícího protokolu:

1. KONTROLA BĚHU A TELEMETRIE:
   - Ověř běh služby přes 'systemctl is-active pirana.service'. Pokud neběží, proveď 'sudo systemctl restart pirana.service'.
   - Stáhni telemetrii z 'http://localhost:80/api/snapshot' (fallback na port 8080).
   - Zkontroluj: system_mode, btc_price, consecutive_losses, daily_pnl, total_pnl, win_rate, current_equity, starting_equity, locked_btc_reserve, vpin_score, lead_lag_status.

2. DVOUVRSTVÁ ARCHITEKTURA A RISK GOVERNANCE:
   - ⚠️ SDÍLENÁ PAMĚŤ: PŘED prací si přečti /home/wwwenda/workspace/pirana/AGENT_STATE.md
     — rozhodnutí operátora jsou ZÁVAZNÁ, sekce PROTOCOL obsahuje co dělaly jiné
     instance (WebUI session, Telegram bot). PO dokončení auditu zapiš svůj souhrn
     do sekce PROTOCOL v AGENT_STATE.md (datum, co jsi změnil a proč).
   - VRSTVA 1 (Operační HFT Motor): Obchoduje s USD marží, zachycuje spread a Lead-Lag arbitráže. Řídí se dynamickým ATR Stop-Lossem (nikdy ne pevným šumovým SL).
   - VRSTVA 2 (Strategický Trezor): 10 % z každého zisku ze spreadu se natrvalo zamyká do nedotknutelné BTC rezervy (Profit Skimmer). Na tuto rezervu se NIKDY nevztahuje prodej ani Stop-Loss (1 BTC = 1 BTC).
   - ⚠️ SIZING (§8.3 + rozhodnutí operátora — viz AGENT_STATE.md!):
     * position_size_pct je ROZHODNUTÍ OPERÁTORA. Před jakoukoliv změnou si ověř
       aktuální rozhodnutí v AGENT_STATE.md — pokud tam není novější pokyn,
       platí poslední rozhodnutí operátora.
     * Ranní audit NESMÍ sizing trvale srážet na 1 % — podlaha min_position_size_pct
       existuje právě proto, aby se bot neza sebeumrtvil (§8.3: „autonomie ano —
       sebeumrtvení ne").
     * Defenzivní reakce na ztrátovou sérii: maximálně DOČASNÉ snížení
       position_size_pct na polovinu (nikdy pod min_position_size_pct),
       ofi_trigger_threshold +0.05. Po 24 h se sizing vrací na původní hodnotu.
     * Změna baseline sizingu = vždy [NEOVĚŘENO] v reportu + žádost operátorovi
       o potvrzení. Operátor rozhoduje, agent navrhuje.
   - FSM VALIDACE: Pokud upravuješ '/home/wwwenda/workspace/pirana/strategy.toml', VŽDY před uložením ověř platnost syntaxe pomocí 'python3 scripts/strategy_versioning.py validate'.

3. STRUKTURA VÝSTUPNÍ ZPRÁVY PRO TELEGRAM:
   - Odpověď musí obsahovat VÝHRADNĚ samotnou zprávu připravenou pro Telegram v HTML formátu.
   - Žádný úvodní ani závěrečný meta-text. Začni přímo hlavičkou.
   - Povolené HTML tagy: <b>tučné</b>, <code>kód/hodnota</code>, <i>kurzíva</i>.

Šablona zprávy:
👑 <b>ČÁSLAV :: RANNÍ AUDIT SYSTÉMU PIRANA</b>
📅 <code>[DATUM A ČAS]</code>
──────────────────────────
🤖 <b>Stav jádra:</b> <code>[Running / Stopped]</code> | Uptime: <code>[UPTIME]</code>
⚙️ <b>Režim:</b> <code>[Active / Defensive / Halted]</code>
💵 <b>Equity:</b> <code>[CURRENT] USD</code> (Start: <code>[START] USD</code>)
📈 <b>Denní PnL:</b> <code>[+ / - PnL USD] ([+ / - %])</code>
🎯 <b>Win Rate:</b> <code>[WIN_RATE]%</code> | Obchodů dnes: <code>[TRADES_COUNT]</code>
⚠️ <b>Série ztrát:</b> <code>[CONSECUTIVE_LOSSES] / 3</code>
🔒 <b>Trezor BTC:</b> <code>[LOCKED_BTC] BTC</code>

📊 <b>TRŽNÍ METRIKY:</b>
• BTC Cena: <code>$[BTC_PRICE]</code>
• OFI Composite: <code>[OFI]</code>
• VPIN Toxicita: <code>[VPIN]%</code>
• Spread: <code>$[SPREAD]</code>

🛠 <b>ADAPTIVNÍ ZÁSAH:</b>
[Popis změn v strategy.toml (Stará hodnota ➔ Nová hodnota) NEBO "Parametry ponechány beze změny — systém je optimální."]

🚦 <b>VERDIKT:</b>
[🟢 Systém je 100% stabilní a ziskový / 🟡 Vyžaduje zvýšený dohled / 🔴 Nutný manuální zásah]
EOF
)

echo "[$(date -Iseconds)] Spouštím ranní audit agenta Čáslav (hermes)... " >> "$LOG_FILE"

# [ROZHODNUTÍ OPERÁTORA 26.8.]: Ranní audit provádí HERMES (instance Čáslava),
# nikoli agy. agy zůstává pouze jako oponent/verifikátor na vyžádání.
# Timeout 5 minut (hermes -z oneshot). -k 30s: SIGKILL po 30s po ignorování SIGTERM.
AGENT_TIMEOUT=300
REPORT_OUTPUT=$(timeout -k 30s "$AGENT_TIMEOUT" hermes -z "$PROMPT_CONTENT" --yolo 2>&1)
AGENT_EXIT=$?

# Timeout nebo chyba → fallback report, ne ticho
if [ $AGENT_EXIT -ne 0 ]; then
    if [ $AGENT_EXIT -eq 124 ]; then
        REPORT_OUTPUT="⚠️ <b>ČÁSLAV :: RANNÍ AUDIT — TIMEOUT</b>\n\nAgent hermes nestihl odpovědět do 5 minut (timeout -k 30s). Zkontroluj logy: journalctl -u pirana-daily-check.service"
    else
        REPORT_OUTPUT="⚠️ <b>ČÁSLAV :: RANNÍ AUDIT — CHYBA AGENTA</b>\n\nAgent hermes skončil s exit kódem $AGENT_EXIT. Zkontroluj logy: journalctl -u pirana-daily-check.service"
    fi
fi

# Uložení výstupu do logu
echo "$REPORT_OUTPUT" >> "$LOG_FILE"

# 3. Bezpečné odeslání na Telegram (HTML parse mode)
# Sanitizace: Telegram HTML nesnasi bare '<' '>' (napr. "win rate <50 %" ->
# "Unsupported start tag"). Povolene tagy b/i/code/pre zachovame, vsechen jiny
# obsah s < > & escapujeme.
sanitize_for_telegram() {
    printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' \
        -e 's/&lt;b&gt;/<b>/g' -e 's/&lt;\/b&gt;/<\/b>/g' \
        -e 's/&lt;i&gt;/<i>/g' -e 's/&lt;\/i&gt;/<\/i>/g' \
        -e 's/&lt;code&gt;/<code>/g' -e 's/&lt;\/code&gt;/<\/code>/g' \
        -e 's/&lt;pre&gt;/<pre>/g' -e 's/&lt;\/pre&gt;/<\/pre>/g'
}

REPORT_SANITIZED=$(sanitize_for_telegram "$REPORT_OUTPUT")

HTTP_RESPONSE=$(curl -s -w "\n%{http_code}" -X POST "https://api.telegram.org/bot${TELEGRAM_TOKEN}/sendMessage" \
    -d "chat_id=${CHAT_ID}" \
    -d "parse_mode=HTML" \
    --data-urlencode "text=${REPORT_SANITIZED}")

HTTP_STATUS=$(echo "$HTTP_RESPONSE" | tail -n1)

if [ "$HTTP_STATUS" -eq 200 ]; then
    echo "[$(date -Iseconds)] Ranní report byl úspěšně odeslán do Telegramu." >> "$LOG_FILE"
else
    echo "[$(date -Iseconds)] CHYBA při odesílání na Telegram (HTTP $HTTP_STATUS)! Zkouším fallback..." >> "$LOG_FILE"
    # Fallback odeslání čistého textu bez formátování při syntaktické chybě HTML
    curl -s -X POST "https://api.telegram.org/bot${TELEGRAM_TOKEN}/sendMessage" \
        -d "chat_id=${CHAT_ID}" \
        --data-urlencode "text=⚠️ Čáslav: Ranní report (čistý text): ${REPORT_OUTPUT}" > /dev/null || true
fi
