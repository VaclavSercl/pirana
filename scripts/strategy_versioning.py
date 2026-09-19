#!/usr/bin/env python3
"""
Čáslav :: Strategy Git Versioning & Automated Rollback Guard.

Safety invariants:
- never publish an invalid strategy
- back up the previous committed strategy, not the already-edited candidate
- publish the exact commit to the branch that is actually checked out
- detached HEAD is fail-closed
- a failed commit/push restores the last committed strategy so hot reload
  cannot silently apply an unpublished configuration
"""

import os
import sys
import tomllib
import subprocess
from datetime import datetime
from pathlib import Path

_DEFAULT_REPO_DIR = Path(__file__).resolve().parents[1]
REPO_DIR = os.environ.get("PIRANA_REPO_DIR", str(_DEFAULT_REPO_DIR))
STRATEGY_FILE = os.environ.get("PIRANA_STRATEGY_FILE", os.path.join(REPO_DIR, "strategy.toml"))
BACKUP_FILE = os.environ.get("PIRANA_STRATEGY_BACKUP", os.path.join(REPO_DIR, "strategy.toml.bak"))
REMOTE = os.environ.get("PIRANA_GIT_REMOTE", "origin")


def _atomic_write(path, data):
    tmp = f"{path}.tmp"
    with open(tmp, "wb") as f:
        f.write(data)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def _git(args, *, check=True, capture=False, timeout=None):
    return subprocess.run(
        ["git", *args],
        cwd=REPO_DIR,
        check=check,
        timeout=timeout,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
    )


def _git_output(args):
    return _git(args, capture=True).stdout.decode("utf-8", errors="strict").strip()


def _committed_strategy(rev="HEAD"):
    return _git(["show", f"{rev}:strategy.toml"], capture=True).stdout


def _restore_last_committed(previous):
    try:
        _atomic_write(STRATEGY_FILE, previous)
        _git(["reset", "--", "strategy.toml"], check=False)
    except Exception as exc:
        print(f"[CRITICAL] Failed to restore last committed strategy: {exc}", file=sys.stderr)


def current_branch():
    branch = _git_output(["branch", "--show-current"])
    if not branch:
        raise RuntimeError("detached HEAD: refuse automatic strategy commit/push")
    return branch


def validate_strategy_file(file_path=STRATEGY_FILE):
    """Validate TOML plus critical semantic/hard-cap invariants."""
    if not os.path.exists(file_path):
        print(f"[ERROR] Strategy file {file_path} does not exist.", file=sys.stderr)
        return False
    try:
        with open(file_path, "rb") as f:
            data = tomllib.load(f)

        required = [
            "system", "trading", "strategy", "inventory", "risk_management",
            "volatility", "order_book", "trailing_stop", "profit_skimmer",
            "adaptive_cooldown", "lead_lag", "hawkes_process", "vpin_guard",
            "avellaneda_stoikov",
        ]
        missing = [key for key in required if key not in data]
        if missing:
            raise ValueError(f"missing required sections: {', '.join(missing)}")

        def finite(name, value):
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                raise ValueError(f"{name} must be numeric")
            number = float(value)
            if not number == number or abs(number) == float("inf"):
                raise ValueError(f"{name} must be finite")
            return number

        system = data["system"]
        trading = data["trading"]
        strat = data["strategy"]
        risk = data["risk_management"]
        vol = data["volatility"]
        cooldown = data["adaptive_cooldown"]

        reload_s = int(system.get("reload_interval_seconds", 0))
        if not (1 <= reload_s <= 3600):
            raise ValueError(f"reload_interval_seconds out of range: {reload_s}")

        max_orders = int(trading.get("max_open_orders", 0))
        if not (1 <= max_orders <= 10):
            raise ValueError(f"max_open_orders out of hard range 1..10: {max_orders}")

        ofi = finite("ofi_trigger_threshold", strat.get("ofi_trigger_threshold"))
        confidence = finite("min_confidence_score", strat.get("min_confidence_score"))
        if not (0.0 < ofi <= 1.0):
            raise ValueError(f"ofi_trigger_threshold out of range: {ofi}")
        if not (0.0 <= confidence <= 1.0):
            raise ValueError(f"min_confidence_score out of range: {confidence}")
        if int(strat.get("ofi_window_size", 0)) <= 0:
            raise ValueError("ofi_window_size must be > 0")
        if int(strat.get("trade_cooldown_ms", 0)) <= 0:
            raise ValueError("trade_cooldown_ms must be > 0")

        max_exp = finite("max_aggregate_exposure_pct", risk.get("max_aggregate_exposure_pct"))
        max_single = finite("max_single_trade_risk_pct", risk.get("max_single_trade_risk_pct"))
        pos = finite("position_size_pct", risk.get("position_size_pct"))
        min_pos = finite("min_position_size_pct", risk.get("min_position_size_pct"))
        max_pos = finite("max_position_size_pct", risk.get("max_position_size_pct"))
        if not (0.01 <= max_exp <= 90.0):
            raise ValueError(f"max_aggregate_exposure_pct exceeds hard cap: {max_exp}")
        if not (0.01 <= max_single <= 5.0):
            raise ValueError(f"max_single_trade_risk_pct exceeds hard cap: {max_single}")
        if not (1.0 <= min_pos <= pos <= max_pos <= 25.0):
            raise ValueError(
                f"position sizing invariant violated: min={min_pos}, baseline={pos}, max={max_pos}"
            )
        slippage = int(risk.get("max_slippage_bps", 0))
        if not (1 <= slippage <= 10):
            raise ValueError(f"max_slippage_bps out of hard range 1..10: {slippage}")

        if int(vol.get("atr_period", 0)) <= 0 or int(vol.get("ticks_per_bar", 0)) <= 0:
            raise ValueError("atr_period and ticks_per_bar must be > 0")
        min_tp = finite("min_tp_usd", vol.get("min_tp_usd"))
        max_tp = finite("max_tp_usd", vol.get("max_tp_usd"))
        min_sl = finite("min_sl_usd", vol.get("min_sl_usd"))
        max_sl = finite("max_sl_usd", vol.get("max_sl_usd"))
        if not (0.0 < min_tp <= max_tp and 0.0 < min_sl <= max_sl):
            raise ValueError("TP/SL min/max invariant violated")

        min_cd = int(cooldown.get("min_ms", 0))
        max_cd = int(cooldown.get("max_ms", 0))
        if not (0 < min_cd <= max_cd):
            raise ValueError("adaptive_cooldown min_ms/max_ms invariant violated")

        print("[OK] Strategy syntax, semantic invariants and hard caps are valid.")
        return True
    except Exception as exc:
        print(f"[ERROR] Strategy validation failed: {exc}", file=sys.stderr)
        return False


def commit_strategy(reason="Autonomous AI tuning by Caslav"):
    """Commit and publish strategy.toml on the currently checked-out branch."""
    previous = None
    old_head = None
    committed = False
    try:
        previous = _committed_strategy("HEAD")
        old_head = _git_output(["rev-parse", "HEAD"])

        if not validate_strategy_file(STRATEGY_FILE):
            _restore_last_committed(previous)
            return False

        branch = current_branch()

        _git(["add", "--", "strategy.toml"])
        staged = _git(["diff", "--staged", "--quiet"], check=False)
        if staged.returncode == 0:
            print("[INFO] No changes detected in strategy.toml to commit.")
            return True

        # Backup is deliberately the previous COMMITTED version.
        _atomic_write(BACKUP_FILE, previous)

        timestamp = datetime.now().astimezone().isoformat(timespec="seconds")
        commit_msg = f"chore(strategy): {reason} [{timestamp}]"
        _git(["commit", "-m", commit_msg, "--", "strategy.toml"])
        committed = True
        new_head = _git_output(["rev-parse", "HEAD"])

        # Publish the exact commit to the exact current branch.
        _git(["push", REMOTE, f"{new_head}:refs/heads/{branch}"], timeout=30)
        print(f"[OK] Strategy committed and pushed to {REMOTE}/{branch}: {new_head[:12]}")
        return True
    except Exception as exc:
        print(f"[ERROR] Strategy commit/push aborted: {exc}", file=sys.stderr)
        if committed and old_head:
            _git(["reset", "--mixed", old_head], check=False)
        if previous is not None:
            _restore_last_committed(previous)
        return False


def rollback_strategy():
    """Create an auditable rollback to the strategy stored in HEAD~1."""
    print("⚠️ Initiating strategy rollback...")
    try:
        previous = _committed_strategy("HEAD~1")
        _atomic_write(STRATEGY_FILE, previous)
        if not validate_strategy_file(STRATEGY_FILE):
            raise RuntimeError("previous committed strategy is invalid")
        if not commit_strategy("rollback to previous committed strategy"):
            raise RuntimeError("rollback could not be committed and published")
        print("[OK] Rollback committed and published.")
        return True
    except Exception as exc:
        print(f"[ERROR] Git rollback failed: {exc}", file=sys.stderr)

    if os.path.exists(BACKUP_FILE):
        try:
            with open(BACKUP_FILE, "rb") as f:
                backup = f.read()
            _atomic_write(STRATEGY_FILE, backup)
            if validate_strategy_file(STRATEGY_FILE) and commit_strategy("rollback from verified backup"):
                print("[OK] Rollback restored from verified backup and published.")
                return True
        except Exception as exc:
            print(f"[ERROR] Backup rollback failed: {exc}", file=sys.stderr)

    print("[CRITICAL] Rollback failed; do not restart with unverified config.", file=sys.stderr)
    return False


if __name__ == "__main__":
    cmd = sys.argv[1].lower() if len(sys.argv) > 1 else "validate"
    if cmd == "validate":
        sys.exit(0 if validate_strategy_file() else 1)
    if cmd == "commit":
        reason = sys.argv[2] if len(sys.argv) > 2 else "Manual strategy update"
        sys.exit(0 if commit_strategy(reason) else 1)
    if cmd == "rollback":
        sys.exit(0 if rollback_strategy() else 1)
    print("Usage: strategy_versioning.py [validate | commit <reason> | rollback]")
    sys.exit(1)
