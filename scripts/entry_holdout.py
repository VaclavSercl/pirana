#!/usr/bin/env python3
"""Chronological Holdout Evaluation Framework for Entry Signals (Pirana System).

Implements rigorous, zero-lookahead, causal holdout backtesting on high-frequency tick data:
1. Data Ingestion & Sanitization:
   - Deduplicates by `tid`, rejects malformed rows, causally sorts by recv_ms/ms.
   - Computes immutable SHA-256 data hash for machine-checkable auditability.
2. Signal Feature Computation:
   - Rolling FlowCalculator (buy/sell volume imbalance + HWM).
3. Realistic Causal Execution Mechanics:
   - Strict Later Timestamp Next Fill: signal at tick i (timestamp T_i) -> entry fill
     at first subsequent tick k > i with recv_ms[k] > recv_ms[i].
   - Intervening ticks with identical recv_ms continue updating feature state.
   - Strict one-position-at-a-time (no overlapping trades).
   - Real millisecond durations (timeout, post-trade min gap cooldown).
   - Zero-Foresight Gap Handling: feed gaps (> max_data_gap_ms) during open positions
     flag the position as GAP_CENSORED. No optimistic retrospective pre-gap exit!
     Censored trades invalidate promotion.
4. Strict Chronological Split:
   - Train partition (e.g. 70%) and Test Holdout partition (30%).
   - Zero outcome crossing between splits.
   - No holdout fitting / parameter snooping.
5. Production Signal Proxy Labeling:
   - Explicitly labeled as 'Pullback_Flow_ProdProxy' (signal proxy on trade prints,
     not the exact full production strategy with L2 book depth, queue position, or fill proof).
6. Multi-Tier Cost Sensitivity & Deterministic Uncertainty:
   - 0.0, 1.0, 2.0, 5.0 bps roundtrip fee/friction levels.
   - Standard Error (SE), 95% Confidence Interval (CI), t-statistic, p-value vs baselines.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import sys
from collections import deque
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Callable, Dict, List, Optional, Tuple

HOLDOUT_SCHEMA_VERSION = "gauntlet-holdout-v2"


@dataclass(frozen=True)
class Tick:
    tid: int
    ts: int  # unix timestamp in seconds
    ms: int  # exchange timestamp in milliseconds
    recv_ms: int  # system receipt timestamp in milliseconds
    p: float  # execution price
    q: float  # execution quantity
    s: int  # side: 1 for buy, -1 for sell


@dataclass
class Trade:
    strategy: str
    entry_idx: int
    entry_tid: int
    entry_time_ms: int
    entry_price: float
    exit_idx: int
    exit_tid: int
    exit_time_ms: int
    exit_price: float
    exit_reason: str  # 'TP', 'SL', 'TIMEOUT', 'GAP_CENSORED', 'SPLIT_END'
    hold_duration_ms: int
    raw_pnl_bps: float
    is_censored: bool = False


class FlowCalculator:
    """Exact Python replica of crates/pirana-features/src/flow.rs."""

    def __init__(self, window_size: int = 20, hwm_window_size: int = 100) -> None:
        self.window_size = window_size
        self.hwm_window_size = hwm_window_size
        self.window: deque[float] = deque(maxlen=window_size)
        self.hwm_window: deque[float] = deque(maxlen=hwm_window_size)
        self.current_flow_val: float = 0.0
        self.current_hwm_val: float = 0.0

    def process_trade(self, side: int, qty: float, price: float) -> None:
        signed_vol = qty if side == 1 else -qty
        self.window.append(signed_vol)
        self.hwm_window.append(price)

        sum_vol = sum(self.window)
        abs_sum_vol = sum(abs(v) for v in self.window)
        self.current_flow_val = (sum_vol / abs_sum_vol) if abs_sum_vol > 0.0 else 0.0
        self.current_hwm_val = max(self.hwm_window) if self.hwm_window else 0.0

    def current_flow(self) -> float:
        return self.current_flow_val

    def hwm(self) -> float:
        return self.current_hwm_val

    def reset(self) -> None:
        self.window.clear()
        self.hwm_window.clear()
        self.current_flow_val = 0.0
        self.current_hwm_val = 0.0


def compute_file_sha256(filepath: str | Path) -> str:
    """Computes SHA-256 hash of a file for immutable audit trails."""
    hasher = hashlib.sha256()
    with open(filepath, "rb") as f:
        while chunk := f.read(65536):
            hasher.update(chunk)
    return hasher.hexdigest()


def parse_and_clean_tick(raw: dict) -> Optional[Tick]:
    """Validates and constructs a Tick, returning None if record is invalid."""
    try:
        tid = int(raw["tid"])
        ts = int(raw.get("ts", 0))
        ms = int(raw["ms"])
        recv_ms = int(raw.get("recv_ms", ms))
        p = float(raw["p"])
        q = float(raw["q"])
        s_raw = raw["s"]

        if isinstance(s_raw, str):
            s_raw_lower = s_raw.strip().lower()
            if s_raw_lower in ("buy", "b", "1"):
                s = 1
            elif s_raw_lower in ("sell", "s", "-1"):
                s = -1
            else:
                return None
        else:
            s = int(s_raw)

        if s not in (1, -1):
            return None
        if not (math.isfinite(p) and p > 0.0):
            return None
        if not (math.isfinite(q) and q > 0.0):
            return None
        if ms <= 0 or recv_ms <= 0:
            return None

        return Tick(tid=tid, ts=ts, ms=ms, recv_ms=recv_ms, p=p, q=q, s=s)
    except (KeyError, ValueError, TypeError):
        return None


def load_research_ticks(
    filepath: str | Path,
) -> Tuple[List[Tick], Dict[str, int | float | str]]:
    """Loads and sanitizes tick data with deduplication and causal sorting."""
    seen_tids = set()
    clean_ticks: List[Tick] = []
    total_lines = 0
    duplicate_count = 0
    invalid_count = 0

    data_hash = compute_file_sha256(filepath) if Path(filepath).exists() else ""

    with open(filepath, "r", encoding="utf-8") as f:
        for line in f:
            total_lines += 1
            line_str = line.strip()
            if not line_str:
                continue
            try:
                raw = json.loads(line_str)
            except json.JSONDecodeError:
                invalid_count += 1
                continue

            tick = parse_and_clean_tick(raw)
            if tick is None:
                invalid_count += 1
                continue

            if tick.tid in seen_tids:
                duplicate_count += 1
                continue

            seen_tids.add(tick.tid)
            clean_ticks.append(tick)

    # Sort causally by recv_ms, secondary ms, tertiary tid
    clean_ticks.sort(key=lambda t: (t.recv_ms, t.ms, t.tid))

    stats: Dict[str, int | float | str] = {
        "data_sha256": data_hash,
        "total_lines": total_lines,
        "valid_unique_ticks": len(clean_ticks),
        "duplicates_dropped": duplicate_count,
        "invalid_dropped": invalid_count,
        "time_span_hours": (
            (clean_ticks[-1].recv_ms - clean_ticks[0].recv_ms) / 3_600_000.0
            if len(clean_ticks) > 1
            else 0.0
        ),
        "min_timestamp_ms": clean_ticks[0].recv_ms if clean_ticks else 0,
        "max_timestamp_ms": clean_ticks[-1].recv_ms if clean_ticks else 0,
    }
    return clean_ticks, stats


# ==============================================================================
# Signal Evaluators (Labeled as Production Proxies & Baselines)
# ==============================================================================


def signal_pullback_flow_prod_proxy(
    calc: FlowCalculator,
    current_price: float,
    flow_thresh: float = 0.05,
    hwm_factor: float = 0.999,
) -> bool:
    """Production signal trigger proxy on trade prints:

    flow_calculator.current_flow() > 0.05 && flow_hwm > 0.0 && price < flow_hwm * 0.999

    NOTE: This is a signal trigger proxy, NOT the full production execution strategy.
    It does not simulate resting limit order queues, L2 orderbook depth, or fill latency.
    """
    flow = calc.current_flow()
    hwm = calc.hwm()
    return flow > flow_thresh and hwm > 0.0 and current_price < (hwm * hwm_factor)


def signal_pullback_flow_variant(
    calc: FlowCalculator,
    current_price: float,
    flow_thresh: float = 0.40,
    hwm_factor: float = 0.9995,
) -> bool:
    """Variant candidate rule: flow > 0.40 && price < hwm * 0.9995."""
    flow = calc.current_flow()
    hwm = calc.hwm()
    return flow > flow_thresh and hwm > 0.0 and current_price < (hwm * hwm_factor)


def signal_buying_pressure_baseline(
    calc: FlowCalculator,
    current_price: float,
    flow_thresh: float = 0.10,
) -> bool:
    """Baseline rule: buy when buying pressure flow > threshold (no pullback requirement)."""
    return calc.current_flow() > flow_thresh


def signal_no_trade_baseline(
    calc: FlowCalculator,
    current_price: float,
) -> bool:
    """Baseline: no trading (flat, EV = 0)."""
    return False


# ==============================================================================
# Causal Execution Engine (Zero-Lookahead, Strict Later Timestamp, Gap Censored)
# ==============================================================================


def simulate_trading(
    ticks: List[Tick],
    signal_fn: Callable[[FlowCalculator, float], bool],
    strategy_name: str,
    tp_bps: float = 5.0,
    sl_bps: float = 10.0,
    timeout_ms: int = 60_000,
    min_gap_ms: int = 30_000,
    max_data_gap_ms: int = 30_000,
    flow_window: int = 20,
    hwm_window: int = 100,
    start_idx: int = 0,
    end_idx: Optional[int] = None,
) -> List[Trade]:
    """Simulates trading strictly forward in time with realistic execution constraints:

    - Signal at tick i (time T_i) -> Entry strictly filled at first subsequent tick k > i
      with recv_ms[k] > T_i (strictly later timestamp).
    - Intervening ticks with recv_ms == T_i update feature calculator continuously.
    - Non-overlapping trades (max 1 active position).
    - Cooldown of min_gap_ms after trade exit.
    - Data gap protection:
      * Pre-entry gap voids signal and resets calculator.
      * Feed gap during holding flags trade as GAP_CENSORED (no optimistic pre-gap exit).
    """
    if end_idx is None:
        end_idx = len(ticks)

    trades: List[Trade] = []
    calc = FlowCalculator(window_size=flow_window, hwm_window_size=hwm_window)

    warm_up_needed = max(flow_window, hwm_window)
    last_exit_time_ms = -min_gap_ms - 1
    last_gap_idx = start_idx

    i = start_idx
    while i < end_idx - 1:
        tick_curr = ticks[i]

        # Check for data feed gap immediately before tick i
        if i > start_idx and (tick_curr.recv_ms - ticks[i - 1].recv_ms) > max_data_gap_ms:
            calc.reset()
            last_gap_idx = i

        calc.process_trade(tick_curr.s, tick_curr.q, tick_curr.p)

        # Check for warm-up period since start or last gap
        if (i - last_gap_idx) < warm_up_needed:
            i += 1
            continue

        # Check post-trade cooldown
        if tick_curr.recv_ms < last_exit_time_ms + min_gap_ms:
            i += 1
            continue

        # Evaluate signal on tick i
        if not signal_fn(calc, tick_curr.p):
            i += 1
            continue

        # Causal execution: find first tick k > i with strictly later timestamp (recv_ms[k] > tick_curr.recv_ms)
        signal_time_ms = tick_curr.recv_ms
        entry_idx = -1
        k = i + 1
        gap_abort = False

        while k < end_idx:
            # Check for data gap between consecutive ticks in feed
            if ticks[k].recv_ms - ticks[k - 1].recv_ms > max_data_gap_ms:
                gap_abort = True
                calc.reset()
                last_gap_idx = k
                i = k
                break

            if ticks[k].recv_ms > signal_time_ms:
                entry_idx = k
                break
            else:
                # Same millisecond batch (recv_ms == signal_time_ms):
                # Update feature calculator to preserve state continuity
                calc.process_trade(ticks[k].s, ticks[k].q, ticks[k].p)
                k += 1

        if gap_abort:
            continue

        if entry_idx == -1:
            # Reached end of partition before a strictly later timestamp arrived
            break

        entry_tick = ticks[entry_idx]
        entry_price = entry_tick.p
        entry_time_ms = entry_tick.recv_ms
        tp_target_price = entry_price * (1.0 + tp_bps / 10_000.0)
        sl_target_price = entry_price * (1.0 - sl_bps / 10_000.0)

        # Update calculator for the entry tick
        calc.process_trade(entry_tick.s, entry_tick.q, entry_tick.p)

        # Walk forward through subsequent ticks to resolve the trade
        exit_idx = entry_idx
        exit_reason = "TIMEOUT"
        exit_price = entry_price
        exit_time_ms = entry_time_ms
        is_censored = False

        j = entry_idx + 1
        trade_resolved = False

        while j < end_idx:
            t_prev = ticks[j - 1]
            t_curr = ticks[j]

            # Check if a market data disconnection/gap occurs while holding
            if t_curr.recv_ms - t_prev.recv_ms > max_data_gap_ms:
                # Market data gap interrupted active position.
                # NO FORESIGHT: Do not assume optimistic exit before the gap.
                # Mark as GAP_CENSORED. Exit price is first post-gap price.
                exit_idx = j
                exit_reason = "GAP_CENSORED"
                exit_price = t_curr.p
                exit_time_ms = t_curr.recv_ms
                is_censored = True
                trade_resolved = True
                calc.reset()
                last_gap_idx = j
                break

            # Update feature calculator
            calc.process_trade(t_curr.s, t_curr.q, t_curr.p)

            # Check TP hit
            if t_curr.p >= tp_target_price:
                exit_idx = j
                exit_reason = "TP"
                exit_price = t_curr.p
                exit_time_ms = t_curr.recv_ms
                trade_resolved = True
                break

            # Check SL hit
            if t_curr.p <= sl_target_price:
                exit_idx = j
                exit_reason = "SL"
                exit_price = t_curr.p
                exit_time_ms = t_curr.recv_ms
                trade_resolved = True
                break

            # Check Timeout (elapsed ms >= timeout_ms)
            if (t_curr.recv_ms - entry_time_ms) >= timeout_ms:
                exit_idx = j
                exit_reason = "TIMEOUT"
                exit_price = t_curr.p
                exit_time_ms = t_curr.recv_ms
                trade_resolved = True
                break

            j += 1

        if not trade_resolved:
            # Reached end of partition while holding
            last_tick = ticks[min(j, end_idx - 1)]
            exit_idx = min(j, end_idx - 1)
            exit_reason = "SPLIT_END"
            exit_price = last_tick.p
            exit_time_ms = last_tick.recv_ms
            is_censored = True

        raw_pnl_bps = ((exit_price - entry_price) / entry_price) * 10_000.0
        hold_duration_ms = max(0, exit_time_ms - entry_time_ms)

        trade = Trade(
            strategy=strategy_name,
            entry_idx=entry_idx,
            entry_tid=entry_tick.tid,
            entry_time_ms=entry_time_ms,
            entry_price=entry_price,
            exit_idx=exit_idx,
            exit_tid=ticks[exit_idx].tid,
            exit_time_ms=exit_time_ms,
            exit_price=exit_price,
            exit_reason=exit_reason,
            hold_duration_ms=hold_duration_ms,
            raw_pnl_bps=raw_pnl_bps,
            is_censored=is_censored,
        )
        trades.append(trade)

        last_exit_time_ms = exit_time_ms
        # Advance simulation index past the exit tick
        i = max(exit_idx, entry_idx) + 1

    return trades


# ==============================================================================
# Statistical Analysis & Cost Modeling
# ==============================================================================


@dataclass
class CostLevelMetrics:
    cost_bps: float
    n_trades: int
    n_resolved: int
    n_censored: int
    net_ev_bps: float
    win_rate_pct: float
    total_net_bps: float
    max_drawdown_bps: float
    std_bps: float
    se_bps: float
    ci_95_low: float
    ci_95_high: float
    t_stat: float
    p_value: float


@dataclass
class StrategyMetricsReport:
    strategy: str
    split_name: str
    n_trades: int
    n_resolved: int
    n_censored: int
    gap_censored_count: int
    raw_ev_bps: float
    avg_hold_duration_sec: float
    exit_breakdown: Dict[str, int]
    cost_sensitivity: Dict[str, CostLevelMetrics]


def compute_metrics(
    trades: List[Trade],
    strategy_name: str,
    split_name: str,
    cost_levels: List[float] = [0.0, 1.0, 2.0, 5.0],
) -> StrategyMetricsReport:
    n = len(trades)
    resolved_trades = [t for t in trades if not t.is_censored]
    censored_trades = [t for t in trades if t.is_censored]
    gap_censored_count = sum(1 for t in trades if t.exit_reason == "GAP_CENSORED")
    n_resolved = len(resolved_trades)
    n_censored = len(censored_trades)

    exit_counts: Dict[str, int] = {}
    for t in trades:
        exit_counts[t.exit_reason] = exit_counts.get(t.exit_reason, 0) + 1

    if n == 0:
        empty_costs: Dict[str, CostLevelMetrics] = {}
        for c in cost_levels:
            empty_costs[f"{c:.1f}_bps"] = CostLevelMetrics(
                cost_bps=c,
                n_trades=0,
                n_resolved=0,
                n_censored=0,
                net_ev_bps=0.0,
                win_rate_pct=0.0,
                total_net_bps=0.0,
                max_drawdown_bps=0.0,
                std_bps=0.0,
                se_bps=0.0,
                ci_95_low=0.0,
                ci_95_high=0.0,
                t_stat=0.0,
                p_value=1.0,
            )
        return StrategyMetricsReport(
            strategy=strategy_name,
            split_name=split_name,
            n_trades=0,
            n_resolved=0,
            n_censored=0,
            gap_censored_count=0,
            raw_ev_bps=0.0,
            avg_hold_duration_sec=0.0,
            exit_breakdown=exit_counts,
            cost_sensitivity=empty_costs,
        )

    # Performance stats calculated on resolved trades if present
    eval_trades = resolved_trades if resolved_trades else trades
    eval_n = len(eval_trades)

    raw_pnls = [t.raw_pnl_bps for t in eval_trades]
    raw_ev = sum(raw_pnls) / eval_n
    avg_hold_sec = (sum(t.hold_duration_ms for t in eval_trades) / eval_n) / 1000.0

    cost_results: Dict[str, CostLevelMetrics] = {}

    for c in cost_levels:
        net_pnls = [p - c for p in raw_pnls]
        total_net = sum(net_pnls)
        net_ev = total_net / eval_n

        wins = [p for p in net_pnls if p > 0.0]
        win_rate = (len(wins) / eval_n) * 100.0

        # Max Drawdown calculation on cumulative equity curve
        cum = 0.0
        peak = 0.0
        max_dd = 0.0
        for p in net_pnls:
            cum += p
            if cum > peak:
                peak = cum
            dd = peak - cum
            if dd > max_dd:
                max_dd = dd

        # Sample standard deviation & deterministic standard error
        if eval_n > 1:
            variance = sum((p - net_ev) ** 2 for p in net_pnls) / (eval_n - 1)
            std = math.sqrt(max(0.0, variance))
            se = std / math.sqrt(eval_n)
        else:
            std = 0.0
            se = 0.0

        ci_low = net_ev - 1.96 * se
        ci_high = net_ev + 1.96 * se

        t_stat = (net_ev / se) if se > 1e-9 else 0.0

        # One-tailed p-value for H0: EV <= 0 using standard normal approximation
        if se > 1e-9:
            p_val = 0.5 * math.erfc(t_stat / math.sqrt(2.0))
        else:
            p_val = 0.0 if net_ev > 0 else 1.0

        cost_results[f"{c:.1f}_bps"] = CostLevelMetrics(
            cost_bps=c,
            n_trades=n,
            n_resolved=n_resolved,
            n_censored=n_censored,
            net_ev_bps=round(net_ev, 3),
            win_rate_pct=round(win_rate, 2),
            total_net_bps=round(total_net, 2),
            max_drawdown_bps=round(max_dd, 2),
            std_bps=round(std, 3),
            se_bps=round(se, 3),
            ci_95_low=round(ci_low, 3),
            ci_95_high=round(ci_high, 3),
            t_stat=round(t_stat, 3),
            p_value=round(p_val, 5),
        )

    return StrategyMetricsReport(
        strategy=strategy_name,
        split_name=split_name,
        n_trades=n,
        n_resolved=n_resolved,
        n_censored=n_censored,
        gap_censored_count=gap_censored_count,
        raw_ev_bps=round(raw_ev, 3),
        avg_hold_duration_sec=round(avg_hold_sec, 2),
        exit_breakdown=exit_counts,
        cost_sensitivity=cost_results,
    )


# ==============================================================================
# Full Holdout Evaluation Pipeline
# ==============================================================================


def run_full_holdout_evaluation(
    ticks_file: str | Path,
    train_ratio: float = 0.70,
    tp_bps: float = 5.0,
    sl_bps: float = 10.0,
    timeout_sec: float = 60.0,
    min_gap_sec: float = 30.0,
    max_gap_sec: float = 30.0,
    cost_levels: List[float] = [0.0, 1.0, 2.0, 5.0],
) -> Dict:
    """Executes the complete chronological train/test holdout evaluation."""
    clean_ticks, data_stats = load_research_ticks(ticks_file)
    n_ticks = len(clean_ticks)
    if n_ticks < 500:
        raise ValueError(
            f"Insufficient data: {n_ticks} ticks found. At least 500 ticks required."
        )

    split_idx = int(n_ticks * train_ratio)
    split_timestamp_ms = clean_ticks[split_idx].recv_ms

    timeout_ms = int(timeout_sec * 1000)
    min_gap_ms = int(min_gap_sec * 1000)
    max_gap_ms = int(max_gap_sec * 1000)

    # Strategy configurations with explicit Proxy labeling
    strategies = [
        (
            "Pullback_Flow_ProdProxy",
            lambda c, p: signal_pullback_flow_prod_proxy(
                c, p, flow_thresh=0.05, hwm_factor=0.999
            ),
            20,
            100,
        ),
        (
            "Pullback_Flow_PromptVariant",
            lambda c, p: signal_pullback_flow_variant(
                c, p, flow_thresh=0.40, hwm_factor=0.9995
            ),
            20,
            100,
        ),
        (
            "Buying_Pressure_Baseline",
            lambda c, p: signal_buying_pressure_baseline(c, p, flow_thresh=0.10),
            20,
            100,
        ),
        (
            "No_Trade_Baseline",
            signal_no_trade_baseline,
            20,
            100,
        ),
    ]

    results: Dict[str, Dict[str, StrategyMetricsReport]] = {
        "train": {},
        "test_holdout": {},
        "full_dataset": {},
    }

    all_trades: Dict[str, Dict[str, List[Trade]]] = {
        "train": {},
        "test_holdout": {},
        "full_dataset": {},
    }

    for name, s_fn, f_win, h_win in strategies:
        # 1. Train Evaluation (strictly bounded by split_idx)
        train_trades = simulate_trading(
            clean_ticks,
            s_fn,
            name,
            tp_bps=tp_bps,
            sl_bps=sl_bps,
            timeout_ms=timeout_ms,
            min_gap_ms=min_gap_ms,
            max_data_gap_ms=max_gap_ms,
            flow_window=f_win,
            hwm_window=h_win,
            start_idx=0,
            end_idx=split_idx,
        )
        results["train"][name] = compute_metrics(
            train_trades, name, "Train (70%)", cost_levels
        )
        all_trades["train"][name] = train_trades

        # 2. Test Holdout Evaluation (strictly starting at split_idx, out-of-sample)
        test_trades = simulate_trading(
            clean_ticks,
            s_fn,
            name,
            tp_bps=tp_bps,
            sl_bps=sl_bps,
            timeout_ms=timeout_ms,
            min_gap_ms=min_gap_ms,
            max_data_gap_ms=max_gap_ms,
            flow_window=f_win,
            hwm_window=h_win,
            start_idx=split_idx,
            end_idx=n_ticks,
        )
        results["test_holdout"][name] = compute_metrics(
            test_trades, name, "Test Holdout (30%)", cost_levels
        )
        all_trades["test_holdout"][name] = test_trades

        # 3. Full Dataset Evaluation (reference only)
        full_trades = simulate_trading(
            clean_ticks,
            s_fn,
            name,
            tp_bps=tp_bps,
            sl_bps=sl_bps,
            timeout_ms=timeout_ms,
            min_gap_ms=min_gap_ms,
            max_data_gap_ms=max_gap_ms,
            flow_window=f_win,
            hwm_window=h_win,
            start_idx=0,
            end_idx=n_ticks,
        )
        results["full_dataset"][name] = compute_metrics(
            full_trades, name, "Full Dataset", cost_levels
        )
        all_trades["full_dataset"][name] = full_trades

    script_path = Path(__file__).resolve()
    script_hash = compute_file_sha256(script_path) if script_path.exists() else ""

    report_payload = {
        "schema_version": HOLDOUT_SCHEMA_VERSION,
        "candidate_hash": script_hash,
        "parameters": {
            "ticks_file": str(ticks_file),
            "train_ratio": train_ratio,
            "tp_bps": tp_bps,
            "sl_bps": sl_bps,
            "timeout_sec": timeout_sec,
            "min_gap_sec": min_gap_sec,
            "max_data_gap_sec": max_gap_sec,
            "cost_levels_bps": cost_levels,
        },
        "data_statistics": data_stats,
        "split_information": {
            "total_clean_ticks": n_ticks,
            "train_ticks": split_idx,
            "test_ticks": n_ticks - split_idx,
            "split_timestamp_ms": split_timestamp_ms,
        },
        "metrics": {
            split_key: {
                strat_key: asdict(m_report)
                for strat_key, m_report in strat_dict.items()
            }
            for split_key, strat_dict in results.items()
        },
        "notices": {
            "methodology": "Causal chronological holdout on deduplicated ticks with strict later timestamp next fill, non-overlapping positions, and zero-foresight gap censorship.",
            "production_proxy_notice": "Trade prints proxy evaluation on historical ticks does not constitute proof of execution in live market conditions. Live market execution requires order book quote depth at the inside L2/L3 levels, queue priority modeling, adverse selection markout against toxic market sweeps, cancellation latency budget (<5ms), and exchange-acknowledged fill proof. A positive proxy edge is a necessary prerequisite, but alone is NOT deployable to live capital without live quote depth and fill proof.",
        },
    }

    return report_payload


# ==============================================================================
# Presentation & CLI Formatting
# ==============================================================================


def format_markdown_report(data: Dict) -> str:
    """Generates a detailed markdown report for Gauntlet audit artifacts."""
    lines = []
    lines.append("# Chronological Holdout Evaluation Report: Entry Signals")
    lines.append("")
    lines.append(
        "> **Methodology Notice:** Honest causal evaluation on deduplicated research ticks with strict later timestamp fills, non-overlapping positions, zero-foresight gap censorship, and multi-tier roundtrip cost proxy sensitivity."
    )
    lines.append("")

    p = data["parameters"]
    d = data["data_statistics"]
    s = data["split_information"]

    lines.append("## 1. Dataset & Split Specification")
    lines.append(f"- **Input File:** `{p['ticks_file']}`")
    lines.append(f"- **Data SHA-256:** `{d.get('data_sha256', 'N/A')}`")
    lines.append(
        f"- **Raw Lines Processed:** {d['total_lines']:,} | **Valid Unique Ticks:** {d['valid_unique_ticks']:,} | **Duplicates Dropped:** {d['duplicates_dropped']:,} ({d['duplicates_dropped']/d['total_lines']*100:.1f}%)"
    )
    lines.append(f"- **Time Span:** {d['time_span_hours']:.2f} hours")
    lines.append(
        f"- **Chronological Train Split:** {s['train_ticks']:,} ticks ({p['train_ratio']*100:.0f}%)"
    )
    lines.append(
        f"- **Chronological Test Holdout Split:** {s['test_ticks']:,} ticks ({(1-p['train_ratio'])*100:.0f}%)"
    )
    lines.append(
        f"- **Fixed Constraints:** TP = {p['tp_bps']} bps | SL = {p['sl_bps']} bps | Timeout = {p['timeout_sec']}s | Post-Trade Gap = {p['min_gap_sec']}s | Max Feed Gap = {p['max_data_gap_sec']}s"
    )
    lines.append("")

    for split_key, split_title in [
        ("test_holdout", "2. TEST HOLDOUT EVALUATION (Strict Out-of-Sample 30%)"),
        ("train", "3. TRAIN EVALUATION (In-Sample 70%)"),
        ("full_dataset", "4. FULL DATASET REFERENCE"),
    ]:
        lines.append(f"## {split_title}")
        lines.append("")
        lines.append(
            "| Strategy | N Trades (Res/Cens) | Raw EV (bps) | Cost 0bps Net EV [95% CI] | Cost 1bps Net EV | Cost 2bps Net EV | Cost 5bps Net EV | WR (0bps) | MaxDD (2bps) | Exits (TP/SL/TO/Gap) |"
        )
        lines.append(
            "| :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :--- |"
        )

        strats = data["metrics"][split_key]
        for s_name, s_data in strats.items():
            n = s_data["n_trades"]
            n_res = s_data.get("n_resolved", n)
            n_cens = s_data.get("n_censored", 0)
            raw_ev = s_data["raw_ev_bps"]
            c0 = s_data["cost_sensitivity"]["0.0_bps"]
            c1 = s_data["cost_sensitivity"]["1.0_bps"]
            c2 = s_data["cost_sensitivity"]["2.0_bps"]
            c5 = s_data["cost_sensitivity"]["5.0_bps"]

            ci_str = f"{c0['net_ev_bps']:+.2f} [{c0['ci_95_low']:+.2f}, {c0['ci_95_high']:+.2f}]"
            exits = s_data["exit_breakdown"]
            exit_str = f"{exits.get('TP', 0)}/{exits.get('SL', 0)}/{exits.get('TIMEOUT', 0)}/{exits.get('GAP_CENSORED', 0)}"

            lines.append(
                f"| **{s_name}** | {n} ({n_res}/{n_cens}) | {raw_ev:+.2f} | {ci_str} | {c1['net_ev_bps']:+.2f} | {c2['net_ev_bps']:+.2f} | {c5['net_ev_bps']:+.2f} | {c0['win_rate_pct']:.1f}% | {c2['max_drawdown_bps']:.1f} bps | {exit_str} |"
            )
        lines.append("")

    lines.append("## 5. Critical Quant Reality & Deployability Limitations")
    lines.append("1. **Trade Prints Proxy vs. Executable Quotes:**")
    lines.append(
        "   - Trade prints represent past executed transactions, NOT resting quotes available to cross. Evaluating signals against trade prints is a necessary initial proxy, but provides ZERO proof of fill probability or queue priority."
    )
    lines.append("2. **Strict Causal Later-Timestamp Fill:**")
    lines.append(
        "   - Fills must occur strictly at a later timestamp (`recv_ms > signal_recv_ms`). Fills within the same millisecond packet are unphysical."
    )
    lines.append("3. **Zero-Foresight Gap Handling:**")
    lines.append(
        "   - Feed interruptions cannot retroactively exit before the gap. Any interrupted positions are flagged as `GAP_CENSORED` and invalidate strategy promotion."
    )
    lines.append("4. **Promotion Requirement:**")
    lines.append(
        "   - A candidate strategy requires >= 300 non-overlapping resolved trades, net EV lower confidence bound > 0 at >= 2.0 bps cost, outperforming baselines, and zero censored data before progressing to shadow orderbook validation."
    )
    lines.append("")
    return "\n".join(lines)


def print_cli_summary(data: Dict) -> None:
    """Prints a clean, concise ASCII table to terminal."""
    print("\n" + "=" * 105)
    print(" PIRANA ENTRY SIGNALS: CHRONOLOGICAL HOLDOUT EVALUATION ")
    print("=" * 105)

    p = data["parameters"]
    d = data["data_statistics"]
    s = data["split_information"]

    print(
        f"Data: {d['total_lines']:,} lines -> {d['valid_unique_ticks']:,} unique ticks ({d['duplicates_dropped']:,} dups dropped, span {d['time_span_hours']:.1f}h)"
    )
    print(
        f"Train/Test Split: {s['train_ticks']:,} train / {s['test_ticks']:,} test holdout ({p['train_ratio']*100:.0f}% / {(1-p['train_ratio'])*100:.0f}%)"
    )
    print(
        f"Execution: Strict later-ts fill, 1-pos max, TP={p['tp_bps']}bps, SL={p['sl_bps']}bps, Timeout={p['timeout_sec']}s, Cooldown={p['min_gap_sec']}s"
    )
    print("-" * 105)

    for split_key, split_label in [
        ("test_holdout", "TEST HOLDOUT (Out-of-Sample 30%)"),
        ("train", "TRAIN (In-Sample 70%)"),
    ]:
        print(f"\n[{split_label}]")
        print(
            f"{'Strategy':<30} | {'N (Res/Cens)':>12} | {'Raw EV':>7} | {'Net 1bps':>8} | {'Net 2bps':>8} | {'Net 5bps':>8} | {'WR%':>5} | {'MaxDD(2b)':>9} | {'SE':>5}"
        )
        print("-" * 105)
        strats = data["metrics"][split_key]
        for s_name, s_data in strats.items():
            n = s_data["n_trades"]
            n_res = s_data.get("n_resolved", n)
            n_cens = s_data.get("n_censored", 0)
            raw_ev = s_data["raw_ev_bps"]
            c0 = s_data["cost_sensitivity"]["0.0_bps"]
            c1 = s_data["cost_sensitivity"]["1.0_bps"]
            c2 = s_data["cost_sensitivity"]["2.0_bps"]
            c5 = s_data["cost_sensitivity"]["5.0_bps"]
            wr = c0["win_rate_pct"]
            max_dd = c2["max_drawdown_bps"]
            se = c0["se_bps"]

            count_str = f"{n} ({n_res}/{n_cens})"
            if n > 0:
                print(
                    f"{s_name:<30} | {count_str:>12} | {raw_ev:+7.2f} | {c1['net_ev_bps']:+8.2f} | {c2['net_ev_bps']:+8.2f} | {c5['net_ev_bps']:+8.2f} | {wr:5.1f} | {max_dd:8.1f}b | {se:5.2f}"
                )
            else:
                print(
                    f"{s_name:<30} | {count_str:>12} | {'---':>7} | {'---':>8} | {'---':>8} | {'---':>8} | {'---':>5} | {'---':>9} | {'---':>5}"
                )
    print("=" * 105 + "\n")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Chronological holdout evaluation for Pirana entry signals."
    )
    parser.add_argument(
        "--input",
        "-i",
        type=str,
        default="/var/lib/pirana/research_ticks.jsonl",
        help="Path to research_ticks.jsonl",
    )
    parser.add_argument(
        "--output",
        "-o",
        type=str,
        default=".gauntlet/runs/20260911-profit-repair/artifacts/holdout_evaluation_v2.json",
        help="Path to output JSON artifact",
    )
    parser.add_argument(
        "--report",
        "-r",
        type=str,
        default=".gauntlet/runs/20260911-profit-repair/artifacts/holdout_report_v2.md",
        help="Path to output Markdown artifact",
    )
    parser.add_argument(
        "--train-ratio",
        type=float,
        default=0.70,
        help="Chronological train split ratio (default: 0.70)",
    )
    parser.add_argument(
        "--tp-bps",
        type=float,
        default=5.0,
        help="Take profit in bps (default: 5.0)",
    )
    parser.add_argument(
        "--sl-bps",
        type=float,
        default=10.0,
        help="Stop loss in bps (default: 10.0)",
    )
    parser.add_argument(
        "--timeout-sec",
        type=float,
        default=60.0,
        help="Holding timeout in seconds (default: 60.0)",
    )
    parser.add_argument(
        "--min-gap-sec",
        type=float,
        default=30.0,
        help="Post-trade min gap in seconds (default: 30.0)",
    )
    parser.add_argument(
        "--max-data-gap-sec",
        type=float,
        default=30.0,
        help="Max allowed tick data gap in seconds (default: 30.0)",
    )
    parser.add_argument(
        "--cost-levels",
        type=str,
        default="0.0,1.0,2.0,5.0",
        help="Comma-separated cost levels in bps (default: 0.0,1.0,2.0,5.0)",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="Suppress terminal table output",
    )

    args = parser.parse_args()

    input_path = Path(args.input)
    if not input_path.exists():
        print(f"Error: Input file {input_path} does not exist.", file=sys.stderr)
        sys.exit(1)

    cost_levels = [float(c.strip()) for c in args.cost_levels.split(",") if c.strip()]

    eval_data = run_full_holdout_evaluation(
        ticks_file=input_path,
        train_ratio=args.train_ratio,
        tp_bps=args.tp_bps,
        sl_bps=args.sl_bps,
        timeout_sec=args.timeout_sec,
        min_gap_sec=args.min_gap_sec,
        max_gap_sec=args.max_data_gap_sec,
        cost_levels=cost_levels,
    )

    if not args.quiet:
        print_cli_summary(eval_data)

    # Save versioned JSON artifact (never overwrite previous rejected reports without intention)
    out_json_path = Path(args.output)
    out_json_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_json_path, "w", encoding="utf-8") as f:
        json.dump(eval_data, f, indent=2)
    print(f"Saved JSON artifact to {out_json_path}")

    # Save versioned Markdown artifact
    out_md_path = Path(args.report)
    out_md_path.parent.mkdir(parents=True, exist_ok=True)
    md_content = format_markdown_report(eval_data)
    with open(out_md_path, "w", encoding="utf-8") as f:
        f.write(md_content)
    print(f"Saved Markdown report to {out_md_path}")


if __name__ == "__main__":
    main()

