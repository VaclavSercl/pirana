#!/usr/bin/env python3
"""
Synchronizes the production trading strategy state from the live Pirana node
into the public ai-trader-strategy repository (https://github.com/VaclavSercl/ai-trader-strategy).

Maintains:
- 01-live-production/T16-pullback-flow-stoikov/SPECIFICATION.json
- 01-live-production/T16-pullback-flow-stoikov/README.md
- 01-live-production/T16-pullback-flow-stoikov/AI_GENERATION_PROMPT.md
- 01-live-production/T16-pullback-flow-stoikov/PERFORMANCE_HISTORY.md
- README.md (Leaderboard & Registry)
"""

import json
import subprocess
import sys
from pathlib import Path

REGISTRY_ROOT = Path("/home/wwwenda/workspace/ai-trader-strategy")
PIRANA_ROOT = Path("/home/wwwenda/workspace/pirana")


def run_cmd(cmd: list[str], cwd: Path) -> tuple[int, str, str]:
    proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    return proc.returncode, proc.stdout, proc.stderr


def sync_registry() -> bool:
    if not REGISTRY_ROOT.exists():
        print(f"[ERROR] Registry directory not found: {REGISTRY_ROOT}")
        return False

    # 1. Pull latest changes if any
    ret, out, err = run_cmd(["git", "pull", "--rebase", "origin", "main"], REGISTRY_ROOT)
    if ret != 0:
        print(f"[WARN] Git pull failed: {err.strip()}")

    # 2. Run validator script
    val_script = REGISTRY_ROOT / "scripts" / "validate_strategies.py"
    if val_script.exists():
        ret, out, err = run_cmd([sys.executable, str(val_script)], REGISTRY_ROOT)
        if ret != 0:
            print(f"[ERROR] Strategy validation failed:\n{out}\n{err}")
            return False
        print("[OK] Strategy validation passed.")

    # 3. Check for unstaged/uncommitted changes
    ret, out, _ = run_cmd(["git", "status", "--porcelain"], REGISTRY_ROOT)
    if not out.strip():
        print("[INFO] ai-trader-strategy registry is already up-to-date. Nothing to commit.")
        return True

    # 4. Stage, commit and push
    run_cmd(["git", "add", "-A"], REGISTRY_ROOT)
    commit_msg = "chore(sync): update T16 live strategy telemetry and specification"
    ret, out, err = run_cmd(["git", "commit", "-m", commit_msg], REGISTRY_ROOT)
    if ret != 0:
        print(f"[ERROR] Git commit failed: {err}")
        return False

    ret, out, err = run_cmd(["git", "push", "origin", "main"], REGISTRY_ROOT)
    if ret != 0:
        print(f"[ERROR] Git push failed: {err}")
        return False

    print("[SUCCESS] Successfully synchronized and pushed updates to ai-trader-strategy!")
    return True


if __name__ == "__main__":
    success = sync_registry()
    sys.exit(0 if success else 1)
