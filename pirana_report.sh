#!/bin/bash

TELEGRAM_TOKEN="${TELEGRAM_BOT_TOKEN:?chybi promenna TELEGRAM_BOT_TOKEN}"
CHAT_ID="${TELEGRAM_CHAT_ID:?chybi promenna TELEGRAM_CHAT_ID}"
# Use the same validated canonical projection as the CLI; unknown remains unknown.
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
MSG=$(python3 "$SCRIPT_DIR/scripts/pirana_report.py" --no-api --html) || exit 1

# Odeslani na Telegram
curl -s -X POST "https://api.telegram.org/bot${TELEGRAM_TOKEN}/sendMessage" \
    -d "chat_id=${CHAT_ID}" \
    -d "parse_mode=HTML" \
    --data-urlencode "text=${MSG}" > /dev/null

echo "Telegram report odeslan: $(date)"
