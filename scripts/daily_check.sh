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
    TELEGRAM_TOKEN=$(grep -E '^TELEGRAM_BOT_TOKEN=' "$ENV_FILE" | cut -d '=' -f2- | tr -d '"'"' | tr -d '[:space:]')
    CHAT_ID=$(grep -E '^TELEGRAM_CHAT_ID=' "$ENV_FILE" | cut -d '=' -f2- | tr -d '"'"' | tr -d '[:space:]')
fi

# Fallback tokeny
TELEGRAM_TOKEN="${TELEGRAM_TOKEN:-***REVOKED_TELEGRAM_TOKEN***}"
CHAT_ID="${CHAT_ID:-1076582576}"

# 2. Definice promptu pro Agenta Čáslav
PROMPT_CONTENT=$(cat << 'EOF'
Úkol pro AI Agenta (Autonomní denní audit a optimalizace systému Pirana):
Jsi ČÁSLAV – svrchovaný správce serveru a kvantitativní architekt. Tvou denní misí je provést v 07:00 hloubkový audit trading bota Pirana, vyhodnotit 24h PnL a bezpečně optimalizovat parametry.

Postupuj podle následujícího protokolu:

1. KONTROLA BĚHU A TELEMETRIE:
   - Ověř běh služby přes 'systemctl is-active pirana.service'. Pokud neběží, proveď 'sudo systemctl restart pirana.service'.
   - Stáhni telemetrii z 'http://localhost:80/api/snapshot' (fallback na port 8080).
   - Zkontroluj: system_mode, btc_price, consecutive_losses, daily_pnl, total_pnl, win_rate, current_equity, starting_equity.

2. SOULAD S PRAVIDLY AGENTS.md A BEZPEČNÁ OPTIMALIZACE:
   - Pokud 'consecutive_losses >= 3' nebo je denní PnL v hlubokém propadu:
     * Přepni systém do Defenzivního režimu: sniž position_size_pct na minimální mez (0.00004 BTC ekvivalent), zvyš ofi_trigger_threshold o +0.05.
   - Pokud je denní PnL kladné a win-rate stabilní:
     * Povol jemné navýšení parametrů pro akumulaci BTC při zachování striktní rezervy kapitálu.
   - FSM VALIDACE: Pokud provádíš úpravu '/home/wwwenda/workspace/pirana/strategy.toml', VŽDY před uložením ověř platnost syntaxe. Nikdy neukládej poškozený soubor.

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

📊 <b>TRŽNÍ METRIKY:</b>
• BTC Cena: <code>$[BTC_PRICE]</code>
• OFI Composite: <code>[OFI]</code>
• Spread: <code>$[SPREAD]</code>

🛠 <b>ADAPTIVNÍ ZÁSAH:</b>
[Popis změn v strategy.toml (Stará hodnota ➔ Nová hodnota) NEBO "Parametry ponechány beze změny — systém je optimální."]

🚦 <b>VERDIKT:</b>
[🟢 Systém je 100% stabilní a ziskový / 🟡 Vyžaduje zvýšený dohled / 🔴 Nutný manuální zásah]
EOF
)

echo "[$(date -Iseconds)] Spouštím ranní audit agenta Čáslav..." >> "$LOG_FILE"

REPORT_OUTPUT=$(/home/wwwenda/.local/bin/agy --dangerously-skip-permissions --print "$PROMPT_CONTENT" 2>&1)

# Uložení výstupu do logu
echo "$REPORT_OUTPUT" >> "$LOG_FILE"

# 3. Bezpečné odeslání na Telegram (HTML parse mode)
HTTP_RESPONSE=$(curl -s -w "\n%{http_code}" -X POST "https://api.telegram.org/bot${TELEGRAM_TOKEN}/sendMessage" \
    -d "chat_id=${CHAT_ID}" \
    -d "parse_mode=HTML" \
    --data-urlencode "text=${REPORT_OUTPUT}")

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
