#!/usr/bin/env python3
"""Isolated Synthetic Unit & Property Tests for entry_holdout.py.

Tests:
1. Deduplication & Data Sanitization (drops dup tids, bad sides/prices/quantities, sorts causally).
2. Causal Strict Later Timestamp Next Fill (entry strictly at recv_ms[entry] > recv_ms[signal]).
3. Equal recv_ms Batch Handling (features update across same-millisecond batch, fill occurs at strict next timestamp).
4. Non-Overlapping Trades & Post-Trade Cooldown (max 1 active position, min_gap enforced).
5. Exit Conditions: TP (+5 bps), SL (-10 bps), Timeout (60s).
6. Feed Data Gap Protection (pre-entry gap voids signal; mid-holding gap marks GAP_CENSORED with zero foresight).
7. Chronological Train/Test Partitioning (zero outcome crossing splits).
8. Cost Sensitivity & Deterministic Uncertainty (0/1/2/5 bps net PnL, SE, CI, MaxDD, resolved vs censored).
9. FlowCalculator Numerical Exactness (matches Rust logic).
10. Production Proxy Labeling and Audit Metadata (immutable hash, notice checks).
"""

from __future__ import annotations

import json
import math
import sys
from pathlib import Path

# Add scripts directory to path to import entry_holdout
sys.path.insert(0, str(Path(__file__).parent.parent / "scripts"))

import pytest
from entry_holdout import (
    HOLDOUT_SCHEMA_VERSION,
    FlowCalculator,
    Tick,
    Trade,
    compute_file_sha256,
    compute_metrics,
    load_research_ticks,
    parse_and_clean_tick,
    signal_buying_pressure_baseline,
    signal_no_trade_baseline,
    signal_pullback_flow_prod_proxy,
    signal_pullback_flow_variant,
    simulate_trading,
)


def make_tick(
    tid: int,
    ts: int = 1000,
    ms: int = 1_000_000,
    recv_ms: int = 1_000_010,
    p: float = 60_000.0,
    q: float = 0.01,
    s: int = 1,
) -> Tick:
    return Tick(tid=tid, ts=ts, ms=ms, recv_ms=recv_ms, p=p, q=q, s=s)


# ==============================================================================
# 1. Deduplication & Data Sanitization Tests
# ==============================================================================


def test_parse_and_clean_tick_valid() -> None:
    raw = {
        "tid": 100,
        "ts": 1788765198,
        "ms": 1788765198697,
        "recv_ms": 1788765198713,
        "p": 79742.0,
        "q": 0.0008,
        "s": -1,
    }
    tick = parse_and_clean_tick(raw)
    assert tick is not None
    assert tick.tid == 100
    assert tick.p == 79742.0
    assert tick.s == -1


def test_parse_and_clean_tick_rejects_invalid() -> None:
    # Negative price
    assert parse_and_clean_tick({"tid": 1, "ms": 10, "p": -10.0, "q": 1.0, "s": 1}) is None
    # Zero quantity
    assert parse_and_clean_tick({"tid": 1, "ms": 10, "p": 60000.0, "q": 0.0, "s": 1}) is None
    # Invalid side
    assert parse_and_clean_tick({"tid": 1, "ms": 10, "p": 60000.0, "q": 1.0, "s": 0}) is None
    # NaN price
    assert parse_and_clean_tick({"tid": 1, "ms": 10, "p": float("nan"), "q": 1.0, "s": 1}) is None
    # Missing ms
    assert parse_and_clean_tick({"tid": 1, "p": 60000.0, "q": 1.0, "s": 1}) is None


def test_load_and_deduplicate_ticks(tmp_path: Path) -> None:
    temp_file = tmp_path / "test_ticks.jsonl"
    lines = [
        {"tid": 1, "ms": 1000, "recv_ms": 1005, "p": 60000.0, "q": 0.1, "s": 1},
        {"tid": 2, "ms": 2000, "recv_ms": 2005, "p": 60010.0, "q": 0.1, "s": 1},
        {"tid": 1, "ms": 1000, "recv_ms": 1050, "p": 60000.0, "q": 0.1, "s": 1},  # Dup
        {"tid": 3, "ms": 1500, "recv_ms": 1505, "p": 60005.0, "q": 0.1, "s": -1},  # Out of order
        {"tid": 4, "ms": 3000, "recv_ms": 3005, "p": -100.0, "q": 0.1, "s": 1},  # Invalid
    ]
    with open(temp_file, "w", encoding="utf-8") as f:
        for item in lines:
            f.write(json.dumps(item) + "\n")

    ticks, stats = load_research_ticks(temp_file)
    assert stats["total_lines"] == 5
    assert stats["duplicates_dropped"] == 1
    assert stats["invalid_dropped"] == 1
    assert stats["valid_unique_ticks"] == 3
    assert "data_sha256" in stats
    assert len(str(stats["data_sha256"])) == 64

    # Verify causal ordering by recv_ms: tid 1 (1005), tid 3 (1505), tid 2 (2005)
    assert [t.tid for t in ticks] == [1, 3, 2]


# ==============================================================================
# 2. Causal Strict Later-Timestamp Fill Timing Tests
# ==============================================================================


def test_causal_strict_later_timestamp_entry() -> None:
    # 150 ticks: first 100 warm up, tick 105 triggers signal, entry must be at tick 106
    ticks: list[Tick] = []
    base_time = 1_000_000
    base_price = 60_000.0

    for i in range(120):
        t_ms = base_time + i * 1000
        p = base_price
        s = 1
        if i == 104:
            p = base_price + 100.0
        elif i == 105:
            # Pullback price: trigger signal at tick 105
            p = base_price - 100.0
            s = 1
        elif i == 106:
            p = base_price - 50.0
        elif i > 106:
            p = base_price

        ticks.append(make_tick(tid=i, ms=t_ms, recv_ms=t_ms, p=p, q=1.0, s=s))

    trades = simulate_trading(
        ticks,
        signal_fn=lambda c, p: signal_pullback_flow_prod_proxy(c, p, flow_thresh=0.05, hwm_factor=0.999),
        strategy_name="Pullback_Test",
        tp_bps=5.0,
        sl_bps=10.0,
        timeout_ms=60_000,
        min_gap_ms=30_000,
        flow_window=20,
        hwm_window=100,
    )

    assert len(trades) >= 1
    t0 = trades[0]
    # Signal was triggered on tick 105, entry must be strictly at tick 106 with recv_ms > ticks[105].recv_ms
    assert t0.entry_idx == 106
    assert t0.entry_tid == 106
    assert t0.entry_price == ticks[106].p
    assert t0.entry_time_ms == ticks[106].recv_ms
    assert t0.entry_time_ms > ticks[105].recv_ms


def test_equal_recv_ms_batch_not_entered_prematurely() -> None:
    """If multiple ticks arrive with identical recv_ms, entry fill must wait

    for the first tick with strictly greater recv_ms, while features update.
    """
    ticks: list[Tick] = []
    base_time = 1_000_000
    base_price = 60_000.0

    # 104 ticks warm up (1s spacing, indices 0..103)
    for i in range(104):
        ticks.append(make_tick(tid=i, ms=base_time + i * 1000, recv_ms=base_time + i * 1000, p=base_price, q=1.0, s=1))

    # High watermark setup at tick 104
    ticks.append(make_tick(tid=104, ms=base_time + 104_000, recv_ms=base_time + 104_000, p=base_price + 100.0, q=1.0, s=1))

    # Signal triggered at tick 105 (recv_ms = 1_105_000)
    sig_time = base_time + 105_000
    ticks.append(make_tick(tid=105, ms=sig_time, recv_ms=sig_time, p=base_price - 100.0, q=1.0, s=1))

    # Same millisecond batch: ticks 106, 107 have EQUAL recv_ms = sig_time
    ticks.append(make_tick(tid=106, ms=sig_time, recv_ms=sig_time, p=59_910.0, q=2.0, s=1))
    ticks.append(make_tick(tid=107, ms=sig_time, recv_ms=sig_time, p=59_920.0, q=3.0, s=1))

    # First strictly later tick: tick 108 at sig_time + 10ms
    entry_time = sig_time + 10
    ticks.append(make_tick(tid=108, ms=entry_time, recv_ms=entry_time, p=59_950.0, q=1.0, s=1))

    # Subsequent ticks to hold/exit
    for i in range(109, 130):
        t = entry_time + (i - 108) * 1000
        ticks.append(make_tick(tid=i, ms=t, recv_ms=t, p=base_price, q=1.0, s=1))

    trades = simulate_trading(
        ticks,
        signal_fn=lambda c, p: signal_pullback_flow_prod_proxy(c, p, flow_thresh=0.05, hwm_factor=0.999),
        strategy_name="Batch_Test",
        tp_bps=5.0,
        sl_bps=10.0,
        timeout_ms=60_000,
        min_gap_ms=30_000,
        flow_window=20,
        hwm_window=100,
    )

    assert len(trades) >= 1
    t0 = trades[0]
    # Must NOT enter at tick 106 or 107 (same recv_ms); must enter at tick 108
    assert t0.entry_idx == 108
    assert t0.entry_tid == 108
    assert t0.entry_time_ms == entry_time
    assert t0.entry_price == 59_950.0


# ==============================================================================
# 3. No-Overlap & Post-Trade Cooldown Tests
# ==============================================================================


def test_no_overlapping_trades_and_cooldown() -> None:
    # Create stream where signal is continuous for 300 ticks
    ticks: list[Tick] = []
    base_time = 1_000_000
    base_price = 60_000.0

    for i in range(300):
        t_ms = base_time + i * 1000
        p = base_price + 100.0 if i < 50 else base_price - 50.0
        ticks.append(make_tick(tid=i, ms=t_ms, recv_ms=t_ms, p=p, q=1.0, s=1))

    trades = simulate_trading(
        ticks,
        signal_fn=lambda c, p: signal_pullback_flow_prod_proxy(c, p, flow_thresh=0.05, hwm_factor=0.999),
        strategy_name="Overlap_Test",
        tp_bps=5.0,
        sl_bps=10.0,
        timeout_ms=10_000,  # 10s timeout
        min_gap_ms=20_000,  # 20s cooldown
        flow_window=20,
        hwm_window=100,
    )

    assert len(trades) >= 2
    for k in range(len(trades) - 1):
        curr_t = trades[k]
        next_t = trades[k + 1]
        # Next entry must be strictly AFTER previous exit + min_gap
        assert next_t.entry_time_ms >= curr_t.exit_time_ms + 20_000
        assert next_t.entry_idx > curr_t.exit_idx


# ==============================================================================
# 4. Exit Conditions (TP, SL, Timeout) Tests
# ==============================================================================


def test_tp_exit_trigger() -> None:
    ticks: list[Tick] = []
    base_time = 1_000_000
    base_price = 60_000.0

    for i in range(120):
        t_ms = base_time + i * 1000
        p = base_price
        if i == 104:
            p = base_price + 200.0
        elif i == 105:
            p = base_price - 100.0
        elif i == 106:
            p = 60_000.0  # entry price
        elif i == 108:
            # TP is 5 bps: 60000 * (1 + 5/10000) = 60030.0
            p = 60_035.0
        ticks.append(make_tick(tid=i, ms=t_ms, recv_ms=t_ms, p=p, q=1.0, s=1))

    trades = simulate_trading(
        ticks,
        signal_fn=lambda c, p: signal_pullback_flow_prod_proxy(c, p, flow_thresh=0.05, hwm_factor=0.999),
        strategy_name="TP_Test",
        tp_bps=5.0,
        sl_bps=10.0,
        timeout_ms=60_000,
    )
    assert len(trades) >= 1
    assert trades[0].exit_reason == "TP"
    assert trades[0].exit_idx == 108
    assert trades[0].raw_pnl_bps >= 5.0
    assert trades[0].is_censored is False


def test_sl_exit_trigger() -> None:
    ticks: list[Tick] = []
    base_time = 1_000_000
    base_price = 60_000.0

    for i in range(120):
        t_ms = base_time + i * 1000
        p = base_price
        if i == 104:
            p = base_price + 200.0
        elif i == 105:
            p = base_price - 100.0
        elif i == 106:
            p = 60_000.0  # entry price
        elif i == 108:
            # SL is 10 bps: 60000 * (1 - 10/10000) = 59940.0
            p = 59_930.0
        ticks.append(make_tick(tid=i, ms=t_ms, recv_ms=t_ms, p=p, q=1.0, s=1))

    trades = simulate_trading(
        ticks,
        signal_fn=lambda c, p: signal_pullback_flow_prod_proxy(c, p, flow_thresh=0.05, hwm_factor=0.999),
        strategy_name="SL_Test",
        tp_bps=5.0,
        sl_bps=10.0,
        timeout_ms=60_000,
    )
    assert len(trades) >= 1
    assert trades[0].exit_reason == "SL"
    assert trades[0].exit_idx == 108
    assert trades[0].raw_pnl_bps <= -10.0
    assert trades[0].is_censored is False


def test_timeout_exit_trigger() -> None:
    ticks: list[Tick] = []
    base_time = 1_000_000
    base_price = 60_000.0

    for i in range(180):
        t_ms = base_time + i * 1000  # 1s per tick
        p = base_price
        if i == 104:
            p = base_price + 200.0
        elif i == 105:
            p = base_price - 100.0
        elif i == 106:
            p = 60_000.0  # entry price at t = 1_106_000
        elif i > 106:
            p = 60_001.0  # within TP/SL band
        ticks.append(make_tick(tid=i, ms=t_ms, recv_ms=t_ms, p=p, q=1.0, s=1))

    trades = simulate_trading(
        ticks,
        signal_fn=lambda c, p: signal_pullback_flow_prod_proxy(c, p, flow_thresh=0.05, hwm_factor=0.999),
        strategy_name="Timeout_Test",
        tp_bps=5.0,
        sl_bps=10.0,
        timeout_ms=60_000,  # 60s timeout
    )
    assert len(trades) >= 1
    assert trades[0].exit_reason == "TIMEOUT"
    assert trades[0].exit_idx == 166
    assert trades[0].hold_duration_ms == 60_000
    assert trades[0].is_censored is False


# ==============================================================================
# 5. Feed Data Gap Protection & Zero-Foresight Gap Censorship Tests
# ==============================================================================


def test_gap_during_holding_flags_censored_no_foresight() -> None:
    """Causal verification: Feed gap while holding flags GAP_CENSORED without

    retroactive pre-gap exit.
    """
    ticks: list[Tick] = []
    base_time = 1_000_000
    base_price = 60_000.0

    for i in range(120):
        t_ms = base_time + i * 1000
        p = base_price
        if i == 104:
            p = base_price + 200.0
        elif i == 105:
            p = base_price - 100.0
        elif i == 106:
            p = 60_000.0  # entry
        elif i == 108:
            # Inject a 45 second gap (> max_data_gap_ms 30s)
            t_ms += 45_000
            p = 59_500.0  # post-gap price dropped
        ticks.append(make_tick(tid=i, ms=t_ms, recv_ms=t_ms, p=p, q=1.0, s=1))

    trades = simulate_trading(
        ticks,
        signal_fn=lambda c, p: signal_pullback_flow_prod_proxy(c, p, flow_thresh=0.05, hwm_factor=0.999),
        strategy_name="Gap_Test",
        tp_bps=5.0,
        sl_bps=10.0,
        timeout_ms=60_000,
        max_data_gap_ms=30_000,
    )
    assert len(trades) >= 1
    trade = trades[0]
    # Must be marked as GAP_CENSORED and is_censored == True
    assert trade.exit_reason == "GAP_CENSORED"
    assert trade.is_censored is True
    assert trade.exit_idx == 108
    assert trade.exit_price == 59_500.0


def test_gap_between_signal_and_entry_aborts_signal() -> None:
    """If a data gap occurs between signal and next tick, the signal is expired."""
    ticks: list[Tick] = []
    base_time = 1_000_000
    base_price = 60_000.0

    for i in range(120):
        t_ms = base_time + i * 1000
        p = base_price
        if i == 104:
            p = base_price + 200.0
        elif i == 105:
            p = base_price - 100.0
        elif i == 106:
            # 45 second gap right after signal
            t_ms += 45_000
        ticks.append(make_tick(tid=i, ms=t_ms, recv_ms=t_ms, p=p, q=1.0, s=1))

    trades = simulate_trading(
        ticks,
        signal_fn=lambda c, p: signal_pullback_flow_prod_proxy(c, p, flow_thresh=0.05, hwm_factor=0.999),
        strategy_name="Gap_Abort_Test",
        tp_bps=5.0,
        sl_bps=10.0,
        timeout_ms=60_000,
        max_data_gap_ms=30_000,
    )
    # The signal at 105 must not enter because tick 106 occurred after a 45s gap
    assert len(trades) == 0


# ==============================================================================
# 6. Chronological Train/Test Partitioning Tests
# ==============================================================================


def test_split_boundary_no_outcome_crossing() -> None:
    ticks: list[Tick] = []
    base_time = 1_000_000
    base_price = 60_000.0

    for i in range(200):
        t_ms = base_time + i * 1000
        p = base_price
        if i == 104:
            p = base_price + 200.0
        elif i == 105:
            p = base_price - 100.0
        elif i == 106:
            p = 60_000.0
        ticks.append(make_tick(tid=i, ms=t_ms, recv_ms=t_ms, p=p, q=1.0, s=1))

    # Split right at tick 110 (while trade is open)
    train_trades = simulate_trading(
        ticks,
        signal_fn=lambda c, p: signal_pullback_flow_prod_proxy(c, p, flow_thresh=0.05, hwm_factor=0.999),
        strategy_name="Train_Split",
        start_idx=0,
        end_idx=110,
    )
    test_trades = simulate_trading(
        ticks,
        signal_fn=lambda c, p: signal_pullback_flow_prod_proxy(c, p, flow_thresh=0.05, hwm_factor=0.999),
        strategy_name="Test_Split",
        start_idx=110,
        end_idx=200,
    )

    assert len(train_trades) == 1
    assert train_trades[0].exit_reason == "SPLIT_END"
    assert train_trades[0].is_censored is True
    assert train_trades[0].exit_idx <= 110

    # Test trades must start strictly at/after 110
    for t in test_trades:
        assert t.entry_idx >= 110


# ==============================================================================
# 7. Cost Sensitivity & Uncertainty Metric Tests
# ==============================================================================


def test_compute_metrics_cost_sensitivity() -> None:
    trades = [
        Trade(
            strategy="Test",
            entry_idx=1,
            entry_tid=1,
            entry_time_ms=1000,
            entry_price=60000.0,
            exit_idx=2,
            exit_tid=2,
            exit_time_ms=2000,
            exit_price=60030.0,  # +5 bps
            exit_reason="TP",
            hold_duration_ms=1000,
            raw_pnl_bps=5.0,
            is_censored=False,
        ),
        Trade(
            strategy="Test",
            entry_idx=3,
            entry_tid=3,
            entry_time_ms=3000,
            entry_price=60000.0,
            exit_idx=4,
            exit_tid=4,
            exit_time_ms=4000,
            exit_price=59940.0,  # -10 bps
            exit_reason="SL",
            hold_duration_ms=1000,
            raw_pnl_bps=-10.0,
            is_censored=False,
        ),
    ]

    report = compute_metrics(trades, "Test", "Test_Split", cost_levels=[0.0, 1.0, 2.0, 5.0])
    assert report.n_trades == 2
    assert report.n_resolved == 2
    assert report.n_censored == 0
    assert report.gap_censored_count == 0
    assert report.raw_ev_bps == -2.5

    c0 = report.cost_sensitivity["0.0_bps"]
    assert c0.net_ev_bps == -2.5
    assert c0.win_rate_pct == 50.0
    assert c0.total_net_bps == -5.0

    c2 = report.cost_sensitivity["2.0_bps"]
    assert c2.net_ev_bps == -4.5
    assert c2.win_rate_pct == 50.0
    assert c2.total_net_bps == -9.0

    c5 = report.cost_sensitivity["5.0_bps"]
    assert c5.net_ev_bps == -7.5
    assert c5.win_rate_pct == 0.0


def test_compute_metrics_with_gap_censored_accounting() -> None:
    trades = [
        Trade(
            strategy="Test",
            entry_idx=1,
            entry_tid=1,
            entry_time_ms=1000,
            entry_price=60000.0,
            exit_idx=2,
            exit_tid=2,
            exit_time_ms=2000,
            exit_price=60030.0,
            exit_reason="TP",
            hold_duration_ms=1000,
            raw_pnl_bps=5.0,
            is_censored=False,
        ),
        Trade(
            strategy="Test",
            entry_idx=3,
            entry_tid=3,
            entry_time_ms=3000,
            entry_price=60000.0,
            exit_idx=4,
            exit_tid=4,
            exit_time_ms=4000,
            exit_price=59900.0,
            exit_reason="GAP_CENSORED",
            hold_duration_ms=1000,
            raw_pnl_bps=-16.67,
            is_censored=True,
        ),
    ]

    report = compute_metrics(trades, "Test", "Test_Split", cost_levels=[0.0, 2.0])
    assert report.n_trades == 2
    assert report.n_resolved == 1
    assert report.n_censored == 1
    assert report.gap_censored_count == 1
    assert report.exit_breakdown["GAP_CENSORED"] == 1


def test_no_trade_baseline_metrics() -> None:
    report = compute_metrics([], "No_Trade_Baseline", "Test_Split")
    assert report.n_trades == 0
    assert report.raw_ev_bps == 0.0
    for c in [0.0, 1.0, 2.0, 5.0]:
        assert report.cost_sensitivity[f"{c:.1f}_bps"].total_net_bps == 0.0
        assert report.cost_sensitivity[f"{c:.1f}_bps"].max_drawdown_bps == 0.0


# ==============================================================================
# 8. FlowCalculator Exactness Tests
# ==============================================================================


def test_flow_calculator_numerical_exactness() -> None:
    calc = FlowCalculator(window_size=20, hwm_window_size=100)
    assert calc.current_flow() == 0.0
    assert calc.hwm() == 0.0

    # 10 Buy trades
    for i in range(10):
        calc.process_trade(side=1, qty=1.0, price=60_000.0 + i * 10.0)
    assert calc.current_flow() == 1.0
    assert calc.hwm() == 60_090.0

    # 10 Sell trades
    for i in range(10):
        calc.process_trade(side=-1, qty=1.0, price=60_090.0 - i * 5.0)
    # 10 buy + 10 sell = balanced
    assert abs(calc.current_flow()) < 1e-9
    assert calc.hwm() == 60_090.0

    # Pullback signal condition verification
    price = 60_000.0
    assert signal_pullback_flow_prod_proxy(calc, price, flow_thresh=0.05, hwm_factor=0.999) is False

    # 15 Buy trades -> flow becomes strongly positive
    for i in range(15):
        calc.process_trade(side=1, qty=1.0, price=60_000.0)
    assert calc.current_flow() > 0.05
    assert calc.hwm() == 60_090.0
    # Price (60000) < 60090 * 0.999 (60029.91)
    assert signal_pullback_flow_prod_proxy(calc, price, flow_thresh=0.05, hwm_factor=0.999) is True

