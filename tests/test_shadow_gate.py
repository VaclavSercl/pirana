#!/usr/bin/env python3
"""Isolated Synthetic Tests for shadow_gate.py Promotion Gate CLI.

ALL test fixtures in this suite are PURELY SYNTHETIC, isolated, and deterministic.
They exist solely to verify the fail-closed machine-checkable promotion barrier.

Test Coverage:
1. Valid Synthetic Candidate Passes Gate:
   - >=300 non-overlapping resolved trades, post-change, fresh timestamps,
     executable fill model, verified 64-hex SHA-256 hashes, CI_low > 0 @ 2bps, beats baseline.
2. Rejection of Historical Proxy Schemas ('gauntlet-holdout-v2', 'gauntlet-holdout-v1', '1.0.0').
3. Rejection of Unresolved Quote Observations (kind='quote_observation').
4. Mandatory 64-Hex SHA-256 Hash Enforcement:
   - Missing mandatory expected hashes (candidate, config, data).
   - Malformed / non-64hex hashes (empty string, short hash, invalid characters).
   - Hash mismatches (candidate, config, data).
5. Sample Size Floor Enforcement:
   - Attempt to lower floor below 300.
   - Insufficient sample size (<300 trades).
6. Conservative Cost Floor Enforcement (<2.0 bps).
7. Post-Change Timing & Bounded Freshness:
   - Missing post_change_since floor.
   - Pre-change observation timestamps.
   - Stale evidence (> max_age_seconds using injected now).
   - Future timestamp detection.
8. Raw Trade Evidence & Data Integrity:
   - Aggregate claims without raw trade records rejected.
   - Non-finite metrics (NaN, Inf) in trade records rejected.
   - Malformed trade records (missing fields, inverted timestamps) rejected.
   - Gap-censored trades rejected.
   - Overlapping trade executions rejected.
   - Unrealistic / historical proxy fill models rejected.
9. Statistical Edge & Baseline Superiority:
   - Negative or zero lower 95% CI bound rejected.
   - Non-positive Net EV rejected.
   - Sub-baseline performance rejected.
10. Strict Safety Invariant: trading_enabled is always False.
11. Exact Next Proof Visibility in Reports.
12. CLI Subprocess End-to-End Execution:
    - Exit code 0 on valid synthetic forward evidence.
    - Exit code 1 on proxy, stale, corrupt, or unhashed evidence.
    - CLI --data-file hash computation and verification.
"""

from __future__ import annotations

import json
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Optional

# Add scripts directory to path to import shadow_gate
sys.path.insert(0, str(Path(__file__).parent.parent / "scripts"))

import pytest
from shadow_gate import (
    MIN_COST_BPS_HARD_FLOOR,
    MIN_TRADES_HARD_FLOOR,
    SHADOW_GATE_SCHEMA_VERSION,
    GateReport,
    compute_file_sha256,
    evaluate_promotion_gate,
    format_gate_markdown,
)

# Standard synthetic 64-hex SHA-256 hashes
SYNTHETIC_CANDIDATE_HASH = "a" * 64
SYNTHETIC_DATA_HASH = "b" * 64
SYNTHETIC_CONFIG_HASH = "c" * 64

# Base reference timestamp: 2026-09-12 00:00:00 UTC (1789171200000 ms)
BASE_REF_MS = 1789171200000
BASE_REF_TIME = datetime.fromtimestamp(BASE_REF_MS / 1000.0, tz=timezone.utc)


def generate_synthetic_trade_records(
    n_trades: int = 350,
    start_time_ms: int = BASE_REF_MS,
    trade_interval_ms: int = 60_000,
    trade_hold_ms: int = 15_000,
    mean_raw_pnl_bps: float = 3.85,
    std_raw_pnl_bps: float = 2.0,
    fill_model: str = "resting_limit_queue",
    is_censored_count: int = 0,
    overlap_count: int = 0,
    malformed_count: int = 0,
    nan_count: int = 0,
) -> List[Dict[str, Any]]:
    """Generates synthetic deterministic trade records for testing."""
    trades = []
    current_time_ms = start_time_ms

    for i in range(n_trades):
        # Alternate slightly around mean to get deterministic standard deviation
        pnl_offset = (1 if i % 2 == 0 else -1) * std_raw_pnl_bps * 0.7
        raw_pnl = mean_raw_pnl_bps + pnl_offset

        entry_ms = current_time_ms
        exit_ms = current_time_ms + trade_hold_ms
        is_censored = i < is_censored_count
        exit_reason = "GAP_CENSORED" if is_censored else ("TP" if raw_pnl > 0 else "SL")

        trade = {
            "strategy": "Pullback_Flow_ForwardShadow",
            "trade_id": f"synthetic_trade_{i:04d}",
            "entry_time_ms": entry_ms,
            "exit_time_ms": exit_ms,
            "entry_price": 60000.0,
            "exit_price": 60000.0 * (1.0 + raw_pnl / 10000.0),
            "raw_pnl_bps": raw_pnl,
            "exit_reason": exit_reason,
            "fill_model": fill_model,
            "is_censored": is_censored,
        }

        # Inject anomalies for specific tests
        if i < nan_count:
            trade["raw_pnl_bps"] = float("nan")

        if i < malformed_count:
            trade.pop("entry_price")

        trades.append(trade)

        # Advance time
        if i < overlap_count:
            # Overlapping next trade starts BEFORE this trade exits
            current_time_ms += trade_hold_ms // 2
        else:
            current_time_ms += trade_interval_ms

    return trades


def create_synthetic_forward_evidence(
    candidate_name: str = "Pullback_Flow_ForwardShadow",
    baseline_name: str = "Buying_Pressure_Baseline",
    split_name: str = "forward_shadow",
    schema_version: str = "gauntlet-forward-resolved-v1",
    kind: str = "forward_resolved_execution",
    candidate_hash: str = SYNTHETIC_CANDIDATE_HASH,
    data_hash: str = SYNTHETIC_DATA_HASH,
    config_hash: str = SYNTHETIC_CONFIG_HASH,
    n_trades: int = 350,
    cand_mean_raw_bps: float = 3.85,  # At 2.0 bps cost, net EV = 1.85 bps
    base_mean_raw_bps: float = 1.20,  # At 2.0 bps cost, net EV = -0.80 bps
    post_change_since_ms: int = BASE_REF_MS,
    trade_start_ms: int = BASE_REF_MS + 1000,
    fill_model: str = "resting_limit_queue",
    is_censored_count: int = 0,
    overlap_count: int = 0,
    malformed_count: int = 0,
    nan_count: int = 0,
    include_raw_candidate_trades: bool = True,
    include_raw_baseline_trades: bool = True,
) -> Dict[str, Any]:
    """Generates a complete, synthetic forward resolved execution evidence payload."""
    cand_trades = []
    if include_raw_candidate_trades:
        cand_trades = generate_synthetic_trade_records(
            n_trades=n_trades,
            start_time_ms=trade_start_ms,
            mean_raw_pnl_bps=cand_mean_raw_bps,
            fill_model=fill_model,
            is_censored_count=is_censored_count,
            overlap_count=overlap_count,
            malformed_count=malformed_count,
            nan_count=nan_count,
        )

    base_trades = []
    if include_raw_baseline_trades:
        base_trades = generate_synthetic_trade_records(
            n_trades=n_trades,
            start_time_ms=trade_start_ms,
            mean_raw_pnl_bps=base_mean_raw_bps,
            fill_model=fill_model,
        )

    max_ts = trade_start_ms + n_trades * 60_000

    evidence = {
        "schema_version": schema_version,
        "kind": kind,
        "candidate_hash": candidate_hash,
        "config_hash": config_hash,
        "data_hash": data_hash,
        "post_change_since_ms": post_change_since_ms,
        "generated_at_ms": max_ts,
        "generated_at_utc": datetime.fromtimestamp(max_ts / 1000.0, tz=timezone.utc).isoformat(),
        "data_statistics": {
            "data_sha256": data_hash,
            "total_ticks": 500000,
            "span_hours": 24.0,
        },
        "metrics": {
            split_name: {
                candidate_name: {
                    "strategy": candidate_name,
                    "split_name": split_name,
                    "n_trades": n_trades,
                    "trades": cand_trades,
                },
                baseline_name: {
                    "strategy": baseline_name,
                    "split_name": split_name,
                    "n_trades": n_trades,
                    "trades": base_trades,
                    "cost_sensitivity": {
                        "2.0_bps": {"net_ev_bps": base_mean_raw_bps - 2.0}
                    },
                },
            }
        },
    }
    return evidence


# ==============================================================================
# 1. Valid Synthetic Execution Passes Gate
# ==============================================================================


@pytest.mark.parametrize('corruption', ['false_pnl', 'aggregate_baseline', 'future_baseline', 'stale_baseline'])
def test_independent_critic_execution_evidence_regressions(corruption):
    evidence = create_synthetic_forward_evidence()
    split = evidence['metrics']['forward_shadow']
    if corruption == 'false_pnl':
        for trade in split['Pullback_Flow_ForwardShadow']['trades']:
            trade['exit_price'] = trade['entry_price'] * .9
    elif corruption == 'aggregate_baseline':
        del split['Buying_Pressure_Baseline']['trades']
    else:
        delta = 30 * 86400000 * (1 if corruption == 'future_baseline' else -1)
        for trade in split['Buying_Pressure_Baseline']['trades']:
            trade['entry_time_ms'] += delta
            trade['exit_time_ms'] += delta
    report = evaluate_promotion_gate(
        evidence,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
        now_utc=datetime.fromtimestamp((evidence['generated_at_ms'] + 1000) / 1000, tz=timezone.utc),
    )
    assert not report.passed


def test_gate_passes_on_valid_synthetic_forward_evidence() -> None:
    """A valid candidate with 350 post-change forward resolved trades, valid 64-hex

    hashes, realistic fill model, positive lower CI at 2bps, and zero censorship MUST PASS.
    """
    ev = create_synthetic_forward_evidence(
        cand_mean_raw_bps=3.85,  # Net EV = 1.85 bps @ 2.0 bps cost
        base_mean_raw_bps=1.20,  # Baseline Net EV = -0.80 bps
        n_trades=350,
    )

    eval_now = datetime.fromtimestamp((BASE_REF_MS + 350 * 60_000 + 3600_000) / 1000.0, tz=timezone.utc)

    report = evaluate_promotion_gate(
        evidence=ev,
        candidate_name="Pullback_Flow_ForwardShadow",
        baseline_name="Buying_Pressure_Baseline",
        split_name="forward_shadow",
        min_trades=300,
        min_cost_bps=2.0,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
        now_utc=eval_now,
    )

    assert report.verdict == "PASS"
    assert report.passed is True
    assert len(report.failures) == 0
    assert report.trading_enabled is False  # Safety Invariant
    assert report.criteria.actual_resolved_trades == 350
    assert report.criteria.candidate_ci_95_low_bps > 0.0
    assert report.criteria.candidate_net_ev_bps > report.criteria.baseline_net_ev_bps
    assert report.criteria.gap_censored_trades == 0
    assert report.criteria.overlapping_trades == 0
    assert report.recomputed_candidate_metrics is not None


# ==============================================================================
# 2. Historical Proxy & Unresolved Quote Rejections
# ==============================================================================


@pytest.mark.parametrize("proxy_schema", ["gauntlet-holdout-v2", "gauntlet-holdout-v1", "1.0.0", "historical_proxy"])
def test_gate_rejects_historical_proxy_schemas(proxy_schema: str) -> None:
    """Historical backtest proxy schemas MUST be strictly rejected."""
    ev = create_synthetic_forward_evidence(schema_version=proxy_schema)
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    assert report.passed is False
    codes = [f.code for f in report.failures]
    assert "HISTORICAL_PROXY_SCHEMA_REJECTED" in codes


def test_gate_rejects_unresolved_quote_observations() -> None:
    """Unsimulated quote observations (kind='quote_observation') MUST be rejected."""
    ev = create_synthetic_forward_evidence(kind="quote_observation")
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    assert report.passed is False
    codes = [f.code for f in report.failures]
    assert "UNRESOLVED_QUOTE_OBSERVATION_REJECTED" in codes


# ==============================================================================
# 3. Mandatory 64-Hex SHA-256 Hash Enforcement
# ==============================================================================


def test_gate_fails_when_expected_hashes_missing() -> None:
    """Evaluating without providing mandatory expected hashes MUST FAIL (fail-closed)."""
    ev = create_synthetic_forward_evidence()
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=None,
        expected_data_hash=None,
        expected_config_hash=None,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "MISSING_MANDATORY_EXPECTED_CANDIDATE_HASH" in codes
    assert "MISSING_MANDATORY_EXPECTED_DATA_HASH" in codes
    assert "MISSING_MANDATORY_EXPECTED_CONFIG_HASH" in codes


@pytest.mark.parametrize("bad_hash", ["", "short_hash", "z" * 64, "12345", None])
def test_gate_rejects_malformed_hashes(bad_hash: Optional[str]) -> None:
    """Malformed, non-64hex, or empty hashes in evidence or expected MUST FAIL."""
    ev = create_synthetic_forward_evidence(candidate_hash=bad_hash or "")
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert any("HASH" in c for c in codes)


def test_gate_fails_on_candidate_hash_mismatch() -> None:
    """Mismatch between evidence candidate hash and expected hash MUST FAIL."""
    ev = create_synthetic_forward_evidence(candidate_hash="1" * 64)
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash="2" * 64,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "CANDIDATE_HASH_MISMATCH" in codes


def test_gate_fails_on_data_hash_mismatch() -> None:
    """Mismatch between evidence data hash and expected hash MUST FAIL."""
    ev = create_synthetic_forward_evidence(data_hash="3" * 64)
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash="4" * 64,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "DATA_HASH_MISMATCH" in codes


def test_gate_fails_on_config_hash_mismatch() -> None:
    """Mismatch between evidence config hash and expected hash MUST FAIL."""
    ev = create_synthetic_forward_evidence(config_hash="5" * 64)
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash="6" * 64,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "CONFIG_HASH_MISMATCH" in codes


# ==============================================================================
# 4. Sample Size & Cost Floor Hard Constraints
# ==============================================================================


def test_sample_size_floor_cannot_be_lowered() -> None:
    """Configuring --min-trades below 300 MUST be rejected."""
    ev = create_synthetic_forward_evidence()
    report = evaluate_promotion_gate(
        evidence=ev,
        min_trades=150,  # Below 300 hard floor
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "SAMPLE_SIZE_FLOOR_CANNOT_BE_LOWERED" in codes


def test_gate_fails_on_insufficient_sample_size() -> None:
    """Candidate with only 200 trades (< 300) MUST FAIL."""
    ev = create_synthetic_forward_evidence(n_trades=200)
    report = evaluate_promotion_gate(
        evidence=ev,
        min_trades=300,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "INSUFFICIENT_SAMPLE_SIZE" in codes


def test_gate_fails_on_cost_below_floor() -> None:
    """Evaluating at cost < 2.0 bps MUST FAIL."""
    ev = create_synthetic_forward_evidence()
    report = evaluate_promotion_gate(
        evidence=ev,
        min_cost_bps=1.0,  # Below conservative floor of 2.0 bps
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "INSUFFICIENT_COST_FLOOR" in codes


# ==============================================================================
# 5. Timing, Post-Change, & Freshness Verification
# ==============================================================================


def test_gate_fails_on_pre_change_trades() -> None:
    """Trades observed before post_change_since floor MUST trigger failure."""
    ev = create_synthetic_forward_evidence(
        post_change_since_ms=BASE_REF_MS + 500_000,
        trade_start_ms=BASE_REF_MS,  # Trades started before post_change_since
    )
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "PRE_CHANGE_OBSERVATION_PRESENT" in codes


def test_gate_fails_on_stale_evidence() -> None:
    """Evidence older than max_age_seconds MUST FAIL."""
    ev = create_synthetic_forward_evidence()
    # Injected evaluation time: 30 days after evidence generation
    eval_now_stale = datetime.fromtimestamp((BASE_REF_MS + 30 * 86400 * 1000) / 1000.0, tz=timezone.utc)

    report = evaluate_promotion_gate(
        evidence=ev,
        max_age_seconds=7 * 86400,  # 7 days limit
        now_utc=eval_now_stale,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "STALE_EVIDENCE" in codes


# ==============================================================================
# 6. Raw Trade Recomputation & Data Integrity
# ==============================================================================


def test_gate_fails_when_raw_trades_missing() -> None:
    """Evidence that merely asserts aggregate stats without raw trades MUST FAIL."""
    ev = create_synthetic_forward_evidence(include_raw_candidate_trades=False)
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "MISSING_RAW_TRADE_RECORDS" in codes


def test_gate_fails_on_nonfinite_metrics() -> None:
    """Trades containing NaN or Inf values MUST FAIL."""
    ev = create_synthetic_forward_evidence(nan_count=3)
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "NONFINITE_METRIC_DETECTED" in codes


def test_gate_fails_on_malformed_trade_records() -> None:
    """Trades with missing essential fields MUST FAIL."""
    ev = create_synthetic_forward_evidence(malformed_count=5)
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "MALFORMED_TRADE_RECORDS_DETECTED" in codes


def test_gate_fails_on_gap_censored_trades() -> None:
    """Evidence containing gap-censored trades MUST FAIL."""
    ev = create_synthetic_forward_evidence(is_censored_count=2)
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "GAP_CENSORED_TRADES_PRESENT" in codes


def test_gate_fails_on_overlapping_trades() -> None:
    """Evidence containing overlapping trade execution intervals MUST FAIL."""
    ev = create_synthetic_forward_evidence(overlap_count=3)
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "OVERLAPPING_OBSERVATIONS_PRESENT" in codes


def test_gate_rejects_historical_proxy_fill_model() -> None:
    """Fill model set to 'historical_proxy_instant' MUST be rejected."""
    ev = create_synthetic_forward_evidence(fill_model="historical_proxy_instant")
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "UNREALISTIC_FILL_MODEL" in codes


# ==============================================================================
# 7. Statistical Edge & Baseline Superiority
# ==============================================================================


def test_gate_fails_on_negative_or_zero_lower_ci() -> None:
    """Candidate with lower 95% CI bound <= 0.0 bps MUST FAIL."""
    # Low raw return of 2.1 bps -> at 2.0 bps cost, Net EV = 0.1 bps, CI_low will be negative
    ev = create_synthetic_forward_evidence(cand_mean_raw_bps=2.10)
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "NEGATIVE_OR_ZERO_LOWER_CI" in codes


def test_gate_fails_when_below_baseline() -> None:
    """Candidate Net EV <= Baseline Net EV MUST FAIL."""
    ev = create_synthetic_forward_evidence(
        cand_mean_raw_bps=3.00,  # Net EV = 1.00 bps
        base_mean_raw_bps=3.50,  # Baseline Net EV = 1.50 bps > candidate
    )
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    codes = [f.code for f in report.failures]
    assert "BELOW_BASELINE_EDGE" in codes


# ==============================================================================
# 8. Report Formatting & Next Proof Section
# ==============================================================================


def test_exact_next_proof_visible_in_report() -> None:
    """Gate report MUST contain explicit next proof requirements on failure."""
    ev = create_synthetic_forward_evidence(schema_version="gauntlet-holdout-v2")
    report = evaluate_promotion_gate(
        evidence=ev,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
    )

    assert report.verdict == "FAIL"
    assert "Exact Next Proof Required" in report.exact_next_proof_required
    md = format_gate_markdown(report)
    assert "Exact Next Proof Required" in md
    assert "gauntlet-forward-resolved-v1" in md


def test_trading_enabled_always_false() -> None:
    """Safety Invariant: trading_enabled must ALWAYS be False on PASS or FAIL."""
    ev_pass = create_synthetic_forward_evidence()
    eval_now = datetime.fromtimestamp((BASE_REF_MS + 350 * 60_000 + 3600_000) / 1000.0, tz=timezone.utc)
    rep_pass = evaluate_promotion_gate(
        evidence=ev_pass,
        expected_candidate_hash=SYNTHETIC_CANDIDATE_HASH,
        expected_data_hash=SYNTHETIC_DATA_HASH,
        expected_config_hash=SYNTHETIC_CONFIG_HASH,
        now_utc=eval_now,
    )
    assert rep_pass.trading_enabled is False

    ev_fail = create_synthetic_forward_evidence(n_trades=10)
    rep_fail = evaluate_promotion_gate(evidence=ev_fail)
    assert rep_fail.trading_enabled is False


# ==============================================================================
# 9. CLI Subprocess End-to-End Tests
# ==============================================================================


def test_cli_subprocess_pass(tmp_path: Path) -> None:
    """End-to-end CLI execution on valid synthetic forward evidence returns exit code 0."""
    ev_file = tmp_path / "valid_forward_evidence.json"
    data_file = tmp_path / "synthetic_ticks.jsonl"
    out_json = tmp_path / "gate_out.json"
    out_md = tmp_path / "gate_report.md"

    # Write dummy data file and compute its sha256
    data_file.write_text("synthetic tick data row 1\nsynthetic tick data row 2\n")
    real_data_hash = compute_file_sha256(data_file)

    ev_data = create_synthetic_forward_evidence(
        data_hash=real_data_hash,
        cand_mean_raw_bps=3.85,
        base_mean_raw_bps=1.20,
        n_trades=350,
    )
    with open(ev_file, "w", encoding="utf-8") as f:
        json.dump(ev_data, f)

    cmd = [
        sys.executable,
        str(Path(__file__).parent.parent / "scripts" / "shadow_gate.py"),
        "--evidence", str(ev_file),
        "--expected-candidate-hash", SYNTHETIC_CANDIDATE_HASH,
        "--expected-data-hash", real_data_hash,
        "--expected-config-hash", SYNTHETIC_CONFIG_HASH,
        "--data-file", str(data_file),
        "--post-change-since", str(BASE_REF_MS),
        "--max-age-hours", "100000.0",  # Large window to avoid test flakiness
        "--output", str(out_json),
        "--report", str(out_md),
        "--quiet",
    ]
    res = subprocess.run(cmd, capture_output=True, text=True)
    assert res.returncode == 0, f"STDOUT: {res.stdout}\nSTDERR: {res.stderr}"
    assert out_json.exists()
    assert out_md.exists()

    with open(out_json, "r", encoding="utf-8") as f:
        saved_report = json.load(f)
    assert saved_report["verdict"] == "PASS"
    assert saved_report["trading_enabled"] is False


def test_cli_subprocess_fail(tmp_path: Path) -> None:
    """End-to-end CLI execution on failing evidence returns exit code 1."""
    ev_file = tmp_path / "failing_proxy_evidence.json"
    out_json = tmp_path / "gate_out_fail.json"

    # Historical proxy schema
    ev_data = create_synthetic_forward_evidence(schema_version="gauntlet-holdout-v2")
    with open(ev_file, "w", encoding="utf-8") as f:
        json.dump(ev_data, f)

    cmd = [
        sys.executable,
        str(Path(__file__).parent.parent / "scripts" / "shadow_gate.py"),
        "--evidence", str(ev_file),
        "--output", str(out_json),
        "--quiet",
    ]
    res = subprocess.run(cmd, capture_output=True, text=True)
    assert res.returncode == 1
    assert out_json.exists()

    with open(out_json, "r", encoding="utf-8") as f:
        saved_report = json.load(f)
    assert saved_report["verdict"] == "FAIL"
    assert len(saved_report["failures"]) >= 1
