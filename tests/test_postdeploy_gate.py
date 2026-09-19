import importlib.util
from pathlib import Path

import pytest


def load_gate():
    path = Path("scripts/postdeploy_gate.py")
    spec = importlib.util.spec_from_file_location("postdeploy_gate_test", path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_listener_parser_and_loopback_policy():
    gate = load_gate()
    parsed = gate.parse_ss_listeners(
        "LISTEN 0 4096 127.0.0.1:8080 0.0.0.0:*\n"
        "LISTEN 0 4096 [::1]:9100 [::]:*\n"
        "LISTEN 0 4096 *:9090 *:*\n"
    )
    assert parsed[8080] == {"127.0.0.1"}
    assert parsed[9100] == {"[::1]"}
    assert parsed[9090] == {"*"}
    assert gate.is_loopback_host("127.0.0.1")
    assert gate.is_loopback_host("[::1]")
    assert not gate.is_loopback_host("*")
    assert not gate.is_loopback_host("0.0.0.0")


def fixture(now):
    return {
        "status": "incomplete",
        "operational": {
            "id": "epoch",
            "scope": "operational:tBTCUSD:excludes_opening_reserve",
            "start_ms": now - 1_000,
            "reserved_btc": "0.00051",
            "status": "complete",
            "sync": {
                "complete": True,
                "cursor_ms": now,
                "coverage_start_ms": 0,
            },
            "open_lots": [],
        },
    }


def test_operational_unknown_history_can_still_be_safe():
    gate = load_gate()
    now = 1_800_000_000_000
    result = gate.validate_operational(
        fixture(now),
        {
            "btc_balance": 0.00051,
            "system_mode": "Active",
            "execution_block_reason": None,
        },
        now_ms=now,
    )
    assert result["reserved_btc"] == pytest.approx(0.00051)
    assert result["open_lot_count"] == 0


def test_operational_stale_or_wallet_mismatch_fails_closed():
    gate = load_gate()
    now = 1_800_000_000_000
    stale = fixture(now)
    stale["operational"]["sync"]["cursor_ms"] = now - gate.STALE_MS - 1
    with pytest.raises(gate.GateError):
        gate.validate_operational(
            stale,
            {"btc_balance": 0.00051, "system_mode": "Active", "execution_block_reason": None},
            now_ms=now,
        )

    with pytest.raises(gate.GateError):
        gate.validate_operational(
            fixture(now),
            {"btc_balance": 0.00052, "system_mode": "Active", "execution_block_reason": None},
            now_ms=now,
        )


def test_execution_block_and_halt_fail_closed():
    gate = load_gate()
    now = 1_800_000_000_000
    with pytest.raises(gate.GateError):
        gate.validate_operational(
            fixture(now),
            {"btc_balance": 0.00051, "system_mode": "Halted", "execution_block_reason": None},
            now_ms=now,
        )
    with pytest.raises(gate.GateError):
        gate.validate_operational(
            fixture(now),
            {"btc_balance": 0.00051, "system_mode": "Active", "execution_block_reason": "pending"},
            now_ms=now,
        )
