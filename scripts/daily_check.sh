#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# ČÁSLAV :: AUTONOMOUS MORNING AUDIT & REPORT DISPATCHER
# ==============================================================================

export PATH="/home/wwwenda/.local/bin:/usr/local/bin:/usr/bin:/bin:$PATH"
WORKSPACE_DIR="/home/wwwenda/workspace/pirana"
LOG_FILE="${WORKSPACE_DIR}/logs/daily_report.log"
mkdir -p "${WORKSPACE_DIR}/logs"

# Telegram-only configuration comes from /etc/pirana/telegram.env via systemd.
# Exchange/custody credentials must never be loaded by this service.
TELEGRAM_TOKEN="${TELEGRAM_BOT_TOKEN:?chybi TELEGRAM_BOT_TOKEN}"
CHAT_ID="${TELEGRAM_CHAT_ID:?chybi TELEGRAM_CHAT_ID}"

# 2. Definice promptu pro Agenta Čáslav
PROMPT_CONTENT=$(cat << 'EOF'
Úkol pro AI Agenta (Autonomní denní audit a optimalizace systému Pirana):
Jsi ČÁSLAV – svrchovaný správce serveru, kvantitativní architekt a institucionální exekutor. Tvou primární misí je systematická akumulace fyzického Bitcoinu (The Bitcoin Accumulation Mandate) a každodenní hloubkový audit v 07:00.

Postupuj podle následujícího protokolu:

1. KONTROLA BĚHU A TELEMETRIE:
   - Ověř běh služby přes 'systemctl is-active pirana.service'. Pokud neběží, proveď 'sudo -n systemctl restart pirana.service'.
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

3. STRUKTURA VÝSTUPNÍ ZPRÁVY:
   - Vrať stručný kvalitativní komentář auditu jako prostý text, nejvýše 600 znaků.
   - Finanční část doplní automaticky kanonický účetní report. Nevymýšlej ani
     neopisuj PnL, equity, win rate, zůstatky nebo počty obchodů z runtime telemetrie.
   - Uveď pouze doložený stav, provedené zásahy a nutné kontroly; chybějící důkaz
     označ NEOVĚŘENO. Active není důkaz obchodování, stabilita není důkaz zisku.
   - Komentář AI bude oddělen od finanční části jako neověřené kvalitativní hodnocení.

EOF
)

echo "[$(date -Iseconds)] Spouštím ranní audit agenta Čáslav (hermes)... " >> "$LOG_FILE"

# [ROZHODNUTÍ OPERÁTORA 26.8.]: Ranní audit provádí HERMES (instance Čáslava),
# nikoli agy. agy zůstává pouze jako oponent/verifikátor na vyžádání.
# Timeout 5 minut (hermes -z oneshot). -k 30s: SIGKILL po 30s po ignorování SIGTERM.
AGENT_TIMEOUT=300
if REPORT_OUTPUT=$(
    env -u BITFINEX_API_KEY -u BITFINEX_API_SECRET \
        -u BITFINEX_API_KEY_FILE -u BITFINEX_API_SECRET_FILE \
        -u TELEGRAM_BOT_TOKEN -u TELEGRAM_CHAT_ID \
        -u CASLAV_TELEGRAM_TOKEN -u CASLAV_ALLOWED_USER_ID \
        timeout -k 30s "$AGENT_TIMEOUT" hermes -z "$PROMPT_CONTENT" --yolo 2>&1
); then
    AGENT_EXIT=0
else
    AGENT_EXIT=$?
fi

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

# Finanční čísla sestavuje výhradně kanonický reporter, nikoli AI komentář.
FINANCIAL_REPORT=$(python3 "${WORKSPACE_DIR}/scripts/pirana_report.py")
export CASLAV_TELEGRAM_TOKEN="$TELEGRAM_TOKEN"
export CASLAV_ALLOWED_USER_ID="$CHAT_ID"
export PYTHONPATH="${WORKSPACE_DIR}/scripts${PYTHONPATH:+:$PYTHONPATH}"
# Escapování probíhá až po omezení prostého textu, nikdy uvnitř HTML entity.
printf '%s\n\nAI AUDIT — NEOVĚŘENÉ KVALITATIVNÍ HODNOCENÍ\n%s' "$FINANCIAL_REPORT" "$REPORT_OUTPUT" |
    python3 -c 'import html, sys; from send_scheduled_report import send_telegram; text = sys.stdin.read(); head, marker, audit = text.partition("AI AUDIT — NEOVĚŘENÉ KVALITATIVNÍ HODNOCENÍ\n"); text = head + marker + audit[:600]; sys.exit(0 if send_telegram(html.escape(text[:3900])) else 1)'
echo "[$(date -Iseconds)] Ranní report byl odeslán do Telegramu." >> "$LOG_FILE"
