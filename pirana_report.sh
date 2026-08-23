#!/bin/bash

TELEGRAM_TOKEN="${TELEGRAM_BOT_TOKEN:?chybi promenna TELEGRAM_BOT_TOKEN}"
CHAT_ID="${TELEGRAM_CHAT_ID:?chybi promenna TELEGRAM_CHAT_ID}"
API_URL="http://localhost:80/api/snapshot"

# Nacteni dat z bota
SNAPSHOT=$(curl -s --max-time 10 "$API_URL")

if [ -z "$SNAPSHOT" ] || [ "$SNAPSHOT" = "null" ]; then
    MSG="Pirana ALERT: Bot neodpovida na API! Zkontroluj server Caslav (systemctl status pirana)."
    curl -s -X POST "https://api.telegram.org/bot${TELEGRAM_TOKEN}/sendMessage" \
        -d "chat_id=${CHAT_ID}" \
        -d "parse_mode=Markdown" \
        --data-urlencode "text=${MSG}" > /dev/null
    exit 1
fi

# Parsovani dat pres jq
MODE=$(echo "$SNAPSHOT"          | jq -r '.system_mode // "Unknown"')
UPTIME=$(echo "$SNAPSHOT"        | jq -r '.uptime_seconds // 0')
BTC_PRICE=$(echo "$SNAPSHOT"     | jq -r '.btc_price // 0')
BTC_BAL=$(echo "$SNAPSHOT"       | jq -r '.btc_balance // 0')
USD_BAL=$(echo "$SNAPSHOT"       | jq -r '.usd_balance // 0')
DAILY_PNL=$(echo "$SNAPSHOT"     | jq -r '.daily_pnl // 0')
DAILY_PNL_PCT=$(echo "$SNAPSHOT" | jq -r '.daily_pnl_pct // 0')
TOTAL_PNL=$(echo "$SNAPSHOT"     | jq -r '.total_pnl // 0')
TRADES=$(echo "$SNAPSHOT"        | jq -r '.trades_today // 0')
WIN_RATE=$(echo "$SNAPSHOT"      | jq -r '.win_rate // 0')
CONS_LOSS=$(echo "$SNAPSHOT"     | jq -r '.consecutive_losses // 0')
BEST=$(echo "$SNAPSHOT"          | jq -r '.best_trade // 0')
WORST=$(echo "$SNAPSHOT"         | jq -r '.worst_trade // 0')
AVG_SIZE=$(echo "$SNAPSHOT"      | jq -r '.avg_trade_size // 0')
EXPOSURE=$(echo "$SNAPSHOT"      | jq -r '.exposure_pct // 0')
DRAWDOWN=$(echo "$SNAPSHOT"      | jq -r '.daily_drawdown_pct // 0')

# Uptime formatovani
UPTIME_H=$((UPTIME / 3600))
UPTIME_M=$(( (UPTIME % 3600) / 60 ))

# Ikony stavu
if [ "$MODE" = "Active" ]; then
    MODE_ICON="✅"
elif [ "$MODE" = "Halted" ]; then
    MODE_ICON="🛑"
else
    MODE_ICON="⚠️"
fi

PNL_ICON=$(echo "$DAILY_PNL" | awk '{if ($1 >= 0) print "📈"; else print "📉"}')

CONS_LOSS_WARN=""
if [ "$CONS_LOSS" -ge 3 ] 2>/dev/null; then
    CONS_LOSS_WARN=" 🚨 VAROVÁNÍ"
fi

TIMESTAMP=$(date '+%Y-%m-%d %H:%M %Z')

# Sestaveni zpravy
MSG="🦈 *PIRANA HFT Report*
📅 ${TIMESTAMP}

*Stav systemu:* ${MODE_ICON} ${MODE}
⏱ *Uptime:* ${UPTIME_H}h ${UPTIME_M}m

💰 *Zustatky na burze:*
  BTC: \`${BTC_BAL}\` BTC
  USD: \`$(printf '%.2f' $USD_BAL)\` USD
  Cena BTC: \`$(printf '%.0f' $BTC_PRICE)\` USD

${PNL_ICON} *Denni vysledky:*
  Obchodu dnes: \`${TRADES}\`
  Denni PnL: \`$(printf '%+.4f' $DAILY_PNL)\` USD (\`$(printf '%+.2f' $DAILY_PNL_PCT)\`%)
  Celkovy PnL: \`$(printf '%+.4f' $TOTAL_PNL)\` USD

📊 *Statistiky:*
  Uspesnost: \`$(printf '%.1f' $WIN_RATE)\`%
  Nejlepsi obchod: \`$(printf '%+.4f' $BEST)\` USD
  Nejhorsi obchod: \`$(printf '%+.4f' $WORST)\` USD
  Prumerna velikost: \`$(printf '%.4f' $AVG_SIZE)\` BTC

⚠️ *Risk Management:*
  Expozice: \`$(printf '%.1f' $EXPOSURE)\`%
  Denni drawdown: \`$(printf '%.2f' $DRAWDOWN)\`%
  Ztraty v rade: \`${CONS_LOSS}\`${CONS_LOSS_WARN}"

# Odeslani na Telegram
curl -s -X POST "https://api.telegram.org/bot${TELEGRAM_TOKEN}/sendMessage" \
    -d "chat_id=${CHAT_ID}" \
    -d "parse_mode=Markdown" \
    --data-urlencode "text=${MSG}" > /dev/null

echo "Telegram report odeslan: $(date)"
