#!/usr/bin/env bash
# Čáslav :: Safe Strategy Config Update & Rollback Helper
#
# The trading runtime hot-reloads strategy.toml and owns its own durable
# risk/FSM brakes. This helper MUST NOT infer that a loss streak means the
# strategy file should be reverted.
set -euo pipefail

VERSIONER="/home/wwwenda/workspace/pirana/scripts/strategy_versioning.py"
SNAPSHOT_URL="http://127.0.0.1:8080/api/snapshot"

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
        # Explicit operator action only. The runtime hot-reloads the resulting
        # strategy, so a process restart is neither required nor desirable.
        python3 "$VERSIONER" rollback
        ;;
    auto-guard)
        # Observability-only compatibility command. RiskEngine/FSM is the
        # authoritative guard; never rewrite strategy.toml from a loss count.
        SNAPSHOT=$(curl -fsS --max-time 5 "$SNAPSHOT_URL") || {
            echo "CRITICAL: Pirana snapshot unavailable at $SNAPSHOT_URL" >&2
            exit 2
        }
        python3 -c '
import json, sys
s = json.load(sys.stdin)
mode = str(s.get("system_mode", "Unknown"))
losses = s.get("consecutive_losses")
block = s.get("execution_block_reason")
print(f"mode={mode} consecutive_losses={losses} execution_block={block or '''none'''}")
if block or mode == "Halted":
    raise SystemExit(2)
' <<<"$SNAPSHOT"
        ;;
    *)
        echo "Usage: $0 [validate | commit <reason> | rollback | auto-guard]" >&2
        exit 1
        ;;
esac
