#!/usr/bin/env bash
set -eu
root=$(git rev-parse --show-toplevel)
exec python3 "$root/.synthbit/gate.py" "$@"
