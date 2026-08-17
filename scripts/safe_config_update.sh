#!/bin/bash
# Čáslav :: Safe Strategy Config Update & Rollback Helper
set -euo pipefail

STRATEGY_FILE="/home/wwwenda/workspace/pirana/strategy.toml"
VERSIONER="/home/wwwenda/workspace/pirana/scripts/strategy_versioning.py"

ACTION="${1:-validate}"
REASON="${2:-Manual update via safe_config_update.sh}"

case "$ACTION" in
    validate)
        python3 "$VERSIONER" validate
        ;;
    commit)
        python3 "$VERSIONER" commit "$REASON"
        ;;
    rollback)
        python3 "$VERSIONER" rollback
        sudo systemctl restart pirana.service
        ;;
    auto-guard)
        # Check consecutive losses from API snapshot
        LOSSES=$(curl -s http://localhost:80/api/snapshot 2>/dev/null | grep -o '"consecutive_losses":[0-9]*' | cut -d':' -f2 || echo "0")
        if [ "$LOSSES" -ge 3 ]; then
            echo "⚠️ [GUARD TRIGGERED] Consecutive losses reached $LOSSES! Initiating automated strategy rollback..."
            python3 "$VERSIONER" rollback
            sudo systemctl restart pirana.service
        else
            echo "✓ System healthy. Consecutive losses: $LOSSES/3."
        fi
        ;;
    *)
        echo "Usage: $0 [validate | commit <reason> | rollback | auto-guard]"
        exit 1
        ;;
esac
