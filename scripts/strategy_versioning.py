#!/usr/bin/env python3
"""
Čáslav :: Strategy Git Versioning & Automated Rollback Guard
Validates strategy.toml syntax, commits changes to Git, and safely rolls back if anomalies occur.
"""

import os
import sys
import shutil
import tomllib
import subprocess
from datetime import datetime

STRATEGY_FILE = "/home/wwwenda/workspace/pirana/strategy.toml"
BACKUP_FILE = "/home/wwwenda/workspace/pirana/strategy.toml.bak"
REPO_DIR = "/home/wwwenda/workspace/pirana"

def validate_strategy_file(file_path=STRATEGY_FILE):
    """Validates TOML syntax and required trading parameters."""
    if not os.path.exists(file_path):
        print(f"[ERROR] Strategy file {file_path} does not exist.", file=sys.stderr)
        return False
    try:
        with open(file_path, "rb") as f:
            data = tomllib.load(f)
        
        required_keys = ["system", "trading", "strategy", "inventory", "risk_management", "trailing_stop", "profit_skimmer", "adaptive_cooldown"]
        for key in required_keys:
            if key not in data:
                print(f"[ERROR] Missing required section: [{key}]", file=sys.stderr)
                return False
        
        # Validate critical numeric limits
        risk = data.get("risk_management", {})
        max_exp = risk.get("max_aggregate_exposure_pct", 0)
        pos_size = risk.get("position_size_pct", 0)
        if not (0.01 <= max_exp <= 95.0):
            print(f"[ERROR] Invalid max_aggregate_exposure_pct: {max_exp}", file=sys.stderr)
            return False
        if not (0.1 <= pos_size <= 25.0):
            print(f"[ERROR] Invalid position_size_pct: {pos_size}", file=sys.stderr)
            return False
            
        print("[OK] Strategy file syntax and parameter bounds are valid.")
        return True
    except Exception as e:
        print(f"[ERROR] TOML validation failed: {e}", file=sys.stderr)
        return False

def commit_strategy(reason="Autonomous AI tuning by Caslav"):
    """Validates and commits strategy.toml to Git."""
    if not validate_strategy_file(STRATEGY_FILE):
        print("[ABORT] Cannot commit invalid strategy file.", file=sys.stderr)
        return False
    
    # Save local backup
    shutil.copy2(STRATEGY_FILE, BACKUP_FILE)
    
    timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    commit_msg = f"chore(strategy): {reason} [{timestamp}]"
    
    try:
        subprocess.run(["git", "add", "strategy.toml"], cwd=REPO_DIR, check=True)
        # Check if there are diffs
        res = subprocess.run(["git", "diff", "--staged", "--quiet"], cwd=REPO_DIR)
        if res.returncode != 0:
            subprocess.run(["git", "commit", "-m", commit_msg], cwd=REPO_DIR, check=True)
            print(f"[OK] Strategy committed to Git: '{commit_msg}'")
            # Push automatically to GitHub remote
            try:
                subprocess.run(["git", "push", "origin", "main"], cwd=REPO_DIR, timeout=15, check=True)
                print("[OK] Pushed commit to GitHub (origin/main).")
            except Exception as pe:
                print(f"[WARN] Remote GitHub push deferred: {pe}")
        else:
            print("[INFO] No changes detected in strategy.toml to commit.")
        return True
    except Exception as e:
        print(f"[ERROR] Git commit failed: {e}", file=sys.stderr)
        return False

def rollback_strategy():
    """Rolls back strategy.toml to previous commit or backup."""
    print("⚠️ Initiating strategy rollback...")
    try:
        res = subprocess.run(["git", "checkout", "HEAD~1", "--", "strategy.toml"], cwd=REPO_DIR)
        if res.returncode == 0 and validate_strategy_file(STRATEGY_FILE):
            print("[OK] Successfully reverted strategy.toml to previous Git commit (HEAD~1).")
            return True
    except Exception as e:
        print(f"[WARN] Git checkout rollback failed ({e}), attempting backup file restore...")

    if os.path.exists(BACKUP_FILE):
        shutil.copy2(BACKUP_FILE, STRATEGY_FILE)
        print("[OK] Successfully restored strategy.toml from strategy.toml.bak.")
        return True

    print("[CRITICAL] Rollback failed! No valid backup found.", file=sys.stderr)
    return False

if __name__ == "__main__":
    if len(sys.argv) > 1:
        cmd = sys.argv[1].lower()
        if cmd == "validate":
            sys.exit(0 if validate_strategy_file() else 1)
        elif cmd == "commit":
            reason = sys.argv[2] if len(sys.argv) > 2 else "Manual strategy update"
            sys.exit(0 if commit_strategy(reason) else 1)
        elif cmd == "rollback":
            sys.exit(0 if rollback_strategy() else 1)
        else:
            print("Usage: strategy_versioning.py [validate | commit <reason> | rollback]")
            sys.exit(1)
    else:
        # Default: validate
        sys.exit(0 if validate_strategy_file() else 1)
