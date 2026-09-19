#!/usr/bin/env python3
"""Pirana post-deploy security/recovery gate.

This script is intentionally fail-closed. It does not mutate the host.
A production deployment must not be declared GREEN while any check fails.
"""

from __future__ import annotations

import argparse
import ipaddress
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import urllib.request

REPO_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_DB = "/var/lib/pirana/accounting.sqlite3"
SNAPSHOT_URL = "http://127.0.0.1:8080/api/snapshot"
REQUIRED_LOCAL_PORTS = {8080, 9091, 9100}
MONITORING_PORTS = {3000, 9090}
STALE_MS = 120_000


class GateError(RuntimeError):
    pass


def run(cmd, *, timeout=30, cwd=None):
    return subprocess.run(
        cmd,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=timeout,
        check=False,
    )


def parse_ss_listeners(text: str) -> dict[int, set[str]]:
    listeners: dict[int, set[str]] = {}
    for raw in text.splitlines():
        parts = raw.split()
        if len(parts) < 4:
            continue
        local = parts[3]
        if local.startswith("[") and "]:" in local:
            host, port_s = local.rsplit("]:", 1)
            host += "]"
        elif ":" in local:
            host, port_s = local.rsplit(":", 1)
        else:
            continue
        try:
            port = int(port_s)
        except ValueError:
            continue
        listeners.setdefault(port, set()).add(host)
    return listeners


def is_loopback_host(host: str) -> bool:
    host = host.strip("[]")
    if host in {"localhost"}:
        return True
    if host in {"*", "0.0.0.0", "::"}:
        return False
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def check_no_general_passwordless_sudo():
    sudo = run(["sudo", "-n", "/usr/bin/true"], timeout=5)
    if sudo.returncode == 0:
        raise GateError(
            "general passwordless sudo is available to this account "
            "(sudo -n /usr/bin/true succeeded); Hermes/control-plane isolation is not proven"
        )


def check_listeners():
    result = run(["ss", "-H", "-ltn"], timeout=10)
    if result.returncode != 0:
        raise GateError(f"cannot inspect TCP listeners: {result.stderr.strip()}")
    listeners = parse_ss_listeners(result.stdout)

    missing = sorted(port for port in REQUIRED_LOCAL_PORTS if port not in listeners)
    if missing:
        raise GateError(f"required Pirana listeners are missing: {missing}")

    for port in sorted(REQUIRED_LOCAL_PORTS | MONITORING_PORTS):
        hosts = listeners.get(port)
        if not hosts:
            continue
        public = sorted(h for h in hosts if not is_loopback_host(h))
        if public:
            raise GateError(
                f"port {port} has non-loopback listener(s): {public}; "
                "production monitoring/trading endpoints must be local/private by policy"
            )
    return listeners


def fetch_json(url: str):
    with urllib.request.urlopen(url, timeout=5) as response:
        if response.status != 200:
            raise GateError(f"{url} returned HTTP {response.status}")
        return json.loads(response.read().decode("utf-8"))


def raw_accounting_report(db_path: str):
    cmd = [
        sys.executable,
        str(REPO_ROOT / "scripts" / "pirana_accounting.py"),
        "--db",
        db_path,
        "report",
    ]
    result = run(cmd, timeout=60, cwd=REPO_ROOT)
    if result.returncode != 0:
        raise GateError(
            "canonical accounting helper failed: "
            + (result.stderr.strip() or f"exit={result.returncode}")
        )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise GateError(f"canonical accounting helper returned invalid JSON: {exc}") from exc


def decimal_nonnegative(value, name: str) -> float:
    try:
        number = float(value)
    except (TypeError, ValueError) as exc:
        raise GateError(f"{name} is not numeric") from exc
    if not (number >= 0.0) or not (number < float("inf")):
        raise GateError(f"{name} is invalid")
    return number


def validate_operational(report: dict, snapshot: dict, *, now_ms: int | None = None):
    now_ms = int(time.time() * 1000) if now_ms is None else now_ms
    operational = report.get("operational")
    p = operational if isinstance(operational, dict) else report

    if p.get("status") != "complete":
        raise GateError("operational accounting status is not complete")
    sync = p.get("sync")
    if not isinstance(sync, dict) or sync.get("complete") is not True:
        raise GateError("operational accounting sync is not complete")

    cursor = sync.get("cursor_ms")
    if not isinstance(cursor, int):
        raise GateError("operational accounting cursor is missing")
    age = now_ms - cursor
    if cursor > now_ms or age > STALE_MS:
        raise GateError(f"operational accounting cursor is stale/invalid (age_ms={age})")

    reserved = 0.0
    if isinstance(operational, dict):
        if p.get("scope") != "operational:tBTCUSD:excludes_opening_reserve":
            raise GateError("operational epoch scope is invalid")
        if not isinstance(p.get("id"), str) or not p["id"].strip():
            raise GateError("operational epoch id is missing")
        start_ms = p.get("start_ms")
        if not isinstance(start_ms, int) or start_ms < 0 or start_ms > cursor:
            raise GateError("operational epoch start_ms is invalid")
        if sync.get("coverage_start_ms") != 0:
            raise GateError("operational epoch does not have full coverage_start_ms=0")
        reserved = decimal_nonnegative(p.get("reserved_btc"), "reserved_btc")

    lots = p.get("open_lots")
    if not isinstance(lots, list):
        raise GateError("operational open_lots are missing")
    expected_btc = reserved
    for idx, lot in enumerate(lots):
        if not isinstance(lot, dict):
            raise GateError(f"open_lots[{idx}] is invalid")
        expected_btc += decimal_nonnegative(lot.get("remaining_btc"), f"open_lots[{idx}].remaining_btc")

    btc_balance = snapshot.get("btc_balance")
    if isinstance(btc_balance, bool) or not isinstance(btc_balance, (int, float)):
        raise GateError("runtime btc_balance is missing/invalid")
    if abs(float(btc_balance) - expected_btc) > 1e-10:
        raise GateError(
            "wallet reconciliation failed: exchange/runtime BTC does not equal "
            "quarantined reserve + operational open lots"
        )

    block = snapshot.get("execution_block_reason")
    if block not in (None, "", False):
        raise GateError(f"execution_block_reason is set: {block}")
    if snapshot.get("system_mode") == "Halted":
        raise GateError("runtime is Halted")

    return {
        "operational_id": p.get("id"),
        "cursor_ms": cursor,
        "reserved_btc": reserved,
        "open_lot_count": len(lots),
        "expected_btc": expected_btc,
    }


def check_flat(snapshot: dict):
    nonempty = {}
    for key in ("open_orders",):
        value = snapshot.get(key)
        if isinstance(value, list) and value:
            nonempty[key] = len(value)
    for key in ("active_position", "pending_entry_intent", "pending_exit_intent"):
        value = snapshot.get(key)
        if value not in (None, False, "", [], {}):
            nonempty[key] = "present"
    if nonempty:
        raise GateError(f"runtime is not flat: {nonempty}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--db", default=os.environ.get("PIRANA_ACCOUNTING_DB", DEFAULT_DB))
    parser.add_argument("--require-flat", action="store_true")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    checks = []

    def perform(name, fn):
        try:
            detail = fn()
            checks.append({"name": name, "ok": True, "detail": detail})
        except Exception as exc:
            checks.append({"name": name, "ok": False, "detail": str(exc)})

    perform("no_general_passwordless_sudo", check_no_general_passwordless_sudo)
    perform("loopback_listeners", check_listeners)

    snapshot_holder = {}
    report_holder = {}

    def load_snapshot():
        snapshot_holder["value"] = fetch_json(SNAPSHOT_URL)
        return {"system_mode": snapshot_holder["value"].get("system_mode")}

    def load_report():
        report_holder["value"] = raw_accounting_report(args.db)
        return {"db": args.db}

    perform("runtime_snapshot", load_snapshot)
    perform("canonical_accounting_report", load_report)

    if "value" in snapshot_holder and "value" in report_holder:
        perform(
            "operational_recovery",
            lambda: validate_operational(report_holder["value"], snapshot_holder["value"]),
        )
        if args.require_flat:
            perform("flat_execution_state", lambda: check_flat(snapshot_holder["value"]))

    ok = all(item["ok"] for item in checks)
    output = {"status": "GREEN" if ok else "BLOCKED", "checks": checks}

    if args.json:
        print(json.dumps(output, ensure_ascii=False, indent=2, default=str))
    else:
        for item in checks:
            mark = "OK" if item["ok"] else "FAIL"
            print(f"[{mark}] {item['name']}: {item['detail']}")
        print(output["status"])

    return 0 if ok else 2


if __name__ == "__main__":
    raise SystemExit(main())
