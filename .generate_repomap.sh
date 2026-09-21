#!/usr/bin/env bash
# ==============================================================================
# .generate_repomap.sh - Deterministic Repository Map generator launcher
# ==============================================================================
set -eu
root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
exec python3 "$root/.synthbit/repomap.py" "$root" "${1:-}"
