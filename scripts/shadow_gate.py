#!/usr/bin/env python3
"""Shadow Promotion Gate CLI (Machine-Checkable Verification Tool).

Enforces a strictly fail-closed machine-checkable promotion barrier:
1. Schema & Rejection of Historical Proxies / Raw Quote Observations:
   - Rejects historical proxy schemas ('gauntlet-holdout-v1', 'gauntlet-holdout-v2', '1.0.0', etc.).
   - Rejects unresolved quote observation schemas ('quote_observation', 'historical_proxy').
   - Requires explicit forward resolved execution evidence schema ('gauntlet-forward-resolved-v1',
     'gauntlet-forward-execution-v1', 'forward-shadow-resolved-v1').

2. Mandatory 64-Hex SHA-256 Hash Verification:
   - Requires valid 64-hex SHA-256 hashes for candidate code/binary, config, and dataset.
   - Rejects missing, empty, malformed, or mismatched hashes.
   - Expected hashes are mandatory and cannot be bypassed.
   - Dataset file hash can be directly verified from CLI via `--data-file`.

3. Sample Size Floor:
   - Requires >= 300 non-overlapping resolved forward trades.
   - Floor CANNOT be lowered by configuration (<300 is rejected).

4. Post-Change Observation Timing & Bounded Freshness:
   - All trade observation timestamps must be strictly post-change (>= post_change_since).
   - Evidence age is bounded by configurable freshness window (<= max_age_seconds relative to evaluation time).
   - Injected evaluation timestamp support for deterministic testing.

5. Rigorous Raw Trade Recomputation & Data Integrity:
   - Evidence cannot merely assert aggregate metrics or claim eligibility.
   - Gate parses raw trade evidence records and independently recomputes:
     sample size, non-overlapping sequence, zero gap-censored trades, sample mean net EV,
     standard deviation, standard error (SE), and 95% confidence interval lower bound.
   - Rejects missing keys, malformed records, non-finite values (NaN, Inf, -Inf).
   - Rejects overlapping trades and gap-censored trades.
   - Requires explicit executable fill model (e.g. 'resting_limit_queue', 'forward_quote_depth', 'taker_crossing').
   - Evaluates at conservative cost floor (>= 2.0 bps).

6. Statistical Edge:
   - Candidate recomputed Net EV at cost >= 2.0 bps must have lower 95% CI bound > 0.0 bps.
   - Candidate recomputed Net EV must strictly beat flat no-trade baseline (EV > 0.0 bps).
   - Candidate recomputed Net EV must strictly beat baseline strategy at the same cost.

7. Fail-Closed Invariant & Visible Next Proof:
   - If full executable forward evidence is unavailable or invalid, ALWAYS FAIL with structured reasons.
   - Exact next proof required for promotion is explicitly detailed in reports.
   - TRADING IS NEVER ENABLED (trading_enabled is always False).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import re
import sys
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Dict, List, Optional, Set, Tuple

SHADOW_GATE_SCHEMA_VERSION = "gauntlet-promotion-gate-v2"

# Accepted forward resolved execution evidence schemas
ACCEPTED_FORWARD_EVIDENCE_SCHEMAS: Set[str] = {
    "gauntlet-forward-resolved-v1",
    "gauntlet-forward-execution-v1",
    "forward-shadow-resolved-v1",
}

# Explicitly rejected historical proxy and unresolved quote schemas
REJECTED_HISTORICAL_SCHEMAS: Set[str] = {
    "gauntlet-holdout-v1",
    "gauntlet-holdout-v2",
    "1.0.0",
    "historical_proxy",
}

REJECTED_EVIDENCE_KINDS: Set[str] = {
    "quote_observation",
    "historical_proxy",
    "unresolved_quote_snapshot",
}

# Valid executable fill models (historical instant fill is rejected)
VALID_EXECUTABLE_FILL_MODELS: Set[str] = {
    "resting_limit_queue",
    "forward_quote_depth",
    "taker_crossing",
    "touch_and_trade",
    "queue_priority_sim",
    "forward_l2_cross",
}

HEX64_REGEX = re.compile(r"^[0-9a-fA-F]{64}$")
MIN_TRADES_HARD_FLOOR = 300
MIN_COST_BPS_HARD_FLOOR = 2.0
DEFAULT_MAX_AGE_SECONDS = 7 * 86400.0  # 7 days


@dataclass
class FailureReason:
    code: str
    message: str
    details: Dict[str, Any] = field(default_factory=dict)


@dataclass
class RecomputedMetrics:
    total_raw_trades: int
    resolved_trades_count: int
    gap_censored_count: int
    overlapping_count: int
    malformed_count: int
    cost_bps: float
    mean_raw_ev_bps: float
    mean_net_ev_bps: float
    std_bps: float
    se_bps: float
    ci_95_low_bps: float
    ci_95_high_bps: float
    min_timestamp_ms: Optional[int]
    max_timestamp_ms: Optional[int]
    fill_models_observed: List[str]


@dataclass
class GateCriteriaEvaluation:
    schema_version_valid: bool
    evidence_schema: str
    evidence_kind: str
    candidate_found: bool
    candidate_name: str
    baseline_found: bool
    baseline_name: str
    split_evaluated: str
    min_trades_required: int
    actual_resolved_trades: int
    min_trades_passed: bool
    cost_bps_evaluated: float
    cost_floor_passed: bool
    candidate_net_ev_bps: float
    candidate_ci_95_low_bps: float
    candidate_se_bps: float
    positive_lower_ci_passed: bool
    baseline_net_ev_bps: float
    baseline_improvement_bps: float
    baseline_improvement_passed: bool
    beats_no_trade_passed: bool
    gap_censored_trades: int
    zero_gap_censored_passed: bool
    overlapping_trades: int
    zero_overlapping_passed: bool
    candidate_hash_matched: bool
    data_hash_matched: bool
    config_hash_matched: bool
    hashes_valid_64hex: bool
    fill_model_valid: bool
    freshness_passed: bool
    post_change_timing_passed: bool
    finite_metrics_passed: bool


@dataclass
class GateReport:
    schema_version: str
    gate_name: str
    verdict: str  # "PASS" | "FAIL"
    passed: bool
    trading_enabled: bool  # Always False!
    execution_mode: str
    evaluation_timestamp_utc: str
    evidence_file: str
    candidate: Dict[str, Any]
    baseline: Dict[str, Any]
    criteria: GateCriteriaEvaluation
    recomputed_candidate_metrics: Optional[Dict[str, Any]]
    recomputed_baseline_metrics: Optional[Dict[str, Any]]
    failures: List[FailureReason]
    exact_next_proof_required: str
    disclaimer: str


def compute_file_sha256(filepath: str | Path) -> str:
    """Computes SHA-256 hash of a file."""
    hasher = hashlib.sha256()
    with open(filepath, "rb") as f:
        while chunk := f.read(65536):
            hasher.update(chunk)
    return hasher.hexdigest()


def is_valid_64hex(val: Any) -> bool:
    """Checks if value is a valid 64-character hexadecimal SHA-256 string."""
    if not isinstance(val, str):
        return False
    return bool(HEX64_REGEX.match(val.strip()))


def parse_timestamp_ms(val: Any) -> Optional[int]:
    """Parses timestamp to milliseconds integer, handling iso strings or numbers."""
    if val is None:
        return None
    if isinstance(val, bool):
        return None
    if isinstance(val, (int, float)):
        if not math.isfinite(val) or val <= 0:
            return None
        # If timestamp is in seconds (< 1e11), convert to ms
        if val < 1e11:
            return int(val * 1000)
        return int(val)
    if isinstance(val, str):
        val_str = val.strip()
        try:
            # Try numeric string first
            num = float(val_str)
            if not math.isfinite(num) or num <= 0:
                return None
            if num < 1e11:
                return int(num * 1000)
            return int(num)
        except ValueError:
            pass
        try:
            # Try ISO-8601
            dt = datetime.fromisoformat(val_str.replace("Z", "+00:00"))
            return int(dt.timestamp() * 1000)
        except Exception:
            return None
    return None


def validate_and_recompute_trades(
    raw_trades: Any,
    cost_bps: float,
    post_change_since_ms: Optional[int] = None,
    allowed_fill_models: Optional[Set[str]] = None,
) -> Tuple[Optional[RecomputedMetrics], List[FailureReason]]:
    """Strictly validates raw trade records and recomputes statistical metrics.

    Checks:
    - Finite numerical values (no NaN, Inf, -Inf).
    - Valid timestamps and chronological progression.
    - Zero overlapping trades (next entry >= prev exit).
    - Zero gap-censored trades.
    - Post-change observation bounds.
    - Valid executable fill models.
    """
    failures: List[FailureReason] = []
    if allowed_fill_models is None:
        allowed_fill_models = VALID_EXECUTABLE_FILL_MODELS

    if not isinstance(raw_trades, list):
        failures.append(
            FailureReason(
                code="MISSING_RAW_TRADE_RECORDS",
                message="Evidence does not contain a list of raw trade execution records. Aggregate claims alone are invalid.",
                details={"found_type": type(raw_trades).__name__},
            )
        )
        return None, failures

    if len(raw_trades) == 0:
        failures.append(
            FailureReason(
                code="EMPTY_RAW_TRADE_RECORDS",
                message="Raw trade records list is empty. Forward resolved trades are required.",
                details={"trades_count": 0},
            )
        )
        return None, failures

    total_raw = len(raw_trades)
    valid_resolved_trades: List[Dict[str, Any]] = []
    gap_censored_count = 0
    overlapping_count = 0
    malformed_count = 0
    nonfinite_count = 0
    pre_change_count = 0
    invalid_fill_model_count = 0
    fill_models_seen: Set[str] = set()

    for idx, tr in enumerate(raw_trades):
        if not isinstance(tr, dict):
            malformed_count += 1
            continue

        # Check required fields
        entry_ms = parse_timestamp_ms(tr.get("entry_time_ms") or tr.get("entry_time") or tr.get("entry_timestamp_ms"))
        exit_ms = parse_timestamp_ms(tr.get("exit_time_ms") or tr.get("exit_time") or tr.get("exit_timestamp_ms"))
        entry_p = tr.get("entry_price")
        exit_p = tr.get("exit_price")
        raw_pnl = tr.get("raw_pnl_bps")
        exit_reason = tr.get("exit_reason", "")
        fill_model = tr.get("fill_model", "")
        is_censored = tr.get("is_censored", False)

        # Check finite numbers and types
        numeric_fields = [("entry_price", entry_p), ("exit_price", exit_p), ("raw_pnl_bps", raw_pnl)]
        record_malformed = False
        for name, val in numeric_fields:
            if val is None or isinstance(val, bool) or not isinstance(val, (int, float)):
                malformed_count += 1
                record_malformed = True
                break
            if not math.isfinite(val):
                nonfinite_count += 1
                record_malformed = True
                break

        if record_malformed:
            continue

        if entry_ms is None or exit_ms is None or entry_ms <= 0 or exit_ms < entry_ms:
            malformed_count += 1
            continue

        if entry_p <= 0 or exit_p <= 0:
            malformed_count += 1
            continue

        # This gate evaluates long BTC entries. A supplied PnL is a checksum,
        # never the authority: derive return from the recorded execution prices.
        calculated_pnl = (exit_p / entry_p - 1.0) * 10000.0
        if not math.isfinite(calculated_pnl) or not math.isclose(raw_pnl, calculated_pnl, rel_tol=1e-8, abs_tol=1e-7):
            malformed_count += 1
            continue

        # Check fill model
        if not isinstance(fill_model, str) or not fill_model.strip():
            invalid_fill_model_count += 1
        else:
            model_clean = fill_model.strip()
            fill_models_seen.add(model_clean)
            if model_clean not in allowed_fill_models or model_clean == "historical_proxy_instant":
                invalid_fill_model_count += 1

        # Check post-change timing
        if post_change_since_ms is not None and entry_ms < post_change_since_ms:
            pre_change_count += 1

        # Check censorship
        if is_censored is True or exit_reason == "GAP_CENSORED":
            gap_censored_count += 1
            continue

        valid_resolved_trades.append({
            "idx": idx,
            "entry_time_ms": entry_ms,
            "exit_time_ms": exit_ms,
            "entry_price": float(entry_p),
            "exit_price": float(exit_p),
            "raw_pnl_bps": calculated_pnl,
            "exit_reason": str(exit_reason),
            "fill_model": str(fill_model),
        })

    if malformed_count > 0:
        failures.append(
            FailureReason(
                code="MALFORMED_TRADE_RECORDS_DETECTED",
                message=f"Evidence contains {malformed_count} malformed trade records.",
                details={"malformed_count": malformed_count, "total_raw": total_raw},
            )
        )

    if nonfinite_count > 0:
        failures.append(
            FailureReason(
                code="NONFINITE_METRIC_DETECTED",
                message=f"Evidence contains {nonfinite_count} trade records with non-finite values (NaN/Inf).",
                details={"nonfinite_count": nonfinite_count},
            )
        )

    if invalid_fill_model_count > 0:
        failures.append(
            FailureReason(
                code="UNREALISTIC_FILL_MODEL",
                message=f"Evidence contains {invalid_fill_model_count} trades with invalid or historical proxy fill models. Executable fill model required.",
                details={
                    "invalid_count": invalid_fill_model_count,
                    "observed_models": list(fill_models_seen),
                    "allowed_models": list(allowed_fill_models),
                },
            )
        )

    if pre_change_count > 0:
        failures.append(
            FailureReason(
                code="PRE_CHANGE_OBSERVATION_PRESENT",
                message=f"Evidence contains {pre_change_count} trades with timestamps before post_change_since floor ({post_change_since_ms} ms).",
                details={"pre_change_count": pre_change_count, "post_change_since_ms": post_change_since_ms},
            )
        )

    if gap_censored_count > 0:
        failures.append(
            FailureReason(
                code="GAP_CENSORED_TRADES_PRESENT",
                message=f"Evidence contains {gap_censored_count} gap-censored trades resulting from data feed gaps. Zero censored trades required.",
                details={"gap_censored_count": gap_censored_count},
            )
        )

    # Sort valid resolved trades chronologically and check for overlapping executions
    valid_resolved_trades.sort(key=lambda t: t["entry_time_ms"])
    for i in range(1, len(valid_resolved_trades)):
        prev_exit = valid_resolved_trades[i - 1]["exit_time_ms"]
        curr_entry = valid_resolved_trades[i]["entry_time_ms"]
        if curr_entry < prev_exit:
            overlapping_count += 1

    if overlapping_count > 0:
        failures.append(
            FailureReason(
                code="OVERLAPPING_OBSERVATIONS_PRESENT",
                message=f"Evidence contains {overlapping_count} overlapping trade execution intervals. Strictly non-overlapping trades required.",
                details={"overlapping_count": overlapping_count},
            )
        )

    n_resolved = len(valid_resolved_trades)
    if n_resolved == 0:
        failures.append(
            FailureReason(
                code="ZERO_RESOLVED_TRADES",
                message="No valid resolved uncensored trades found after filtering.",
                details={"total_raw": total_raw},
            )
        )
        return None, failures

    # Statistical recomputation
    raw_pnls = [t["raw_pnl_bps"] for t in valid_resolved_trades]
    mean_raw_ev = sum(raw_pnls) / n_resolved
    mean_net_ev = mean_raw_ev - cost_bps

    if n_resolved >= 2:
        variance = sum((x - mean_raw_ev) ** 2 for x in raw_pnls) / (n_resolved - 1)
        std_bps = math.sqrt(variance)
        se_bps = std_bps / math.sqrt(n_resolved)
    else:
        std_bps = 0.0
        se_bps = 0.0

    ci_95_low = mean_net_ev - 1.96 * se_bps
    ci_95_high = mean_net_ev + 1.96 * se_bps

    min_ts = valid_resolved_trades[0]["entry_time_ms"]
    max_ts = max(t["exit_time_ms"] for t in valid_resolved_trades)

    recomputed = RecomputedMetrics(
        total_raw_trades=total_raw,
        resolved_trades_count=n_resolved,
        gap_censored_count=gap_censored_count,
        overlapping_count=overlapping_count,
        malformed_count=malformed_count + nonfinite_count,
        cost_bps=cost_bps,
        mean_raw_ev_bps=round(mean_raw_ev, 4),
        mean_net_ev_bps=round(mean_net_ev, 4),
        std_bps=round(std_bps, 4),
        se_bps=round(se_bps, 4),
        ci_95_low_bps=round(ci_95_low, 4),
        ci_95_high_bps=round(ci_95_high, 4),
        min_timestamp_ms=min_ts,
        max_timestamp_ms=max_ts,
        fill_models_observed=sorted(list(fill_models_seen)),
    )

    return recomputed, failures


def evaluate_promotion_gate(
    evidence: Dict[str, Any],
    candidate_name: str = "Pullback_Flow_ForwardShadow",
    baseline_name: str = "Buying_Pressure_Baseline",
    split_name: str = "forward_shadow",
    min_trades: int = 300,
    min_cost_bps: float = 2.0,
    expected_candidate_hash: Optional[str] = None,
    expected_data_hash: Optional[str] = None,
    expected_config_hash: Optional[str] = None,
    evidence_file_path: str = "",
    post_change_since: Optional[Any] = None,
    max_age_seconds: float = DEFAULT_MAX_AGE_SECONDS,
    now_utc: Optional[datetime] = None,
    data_file_path: Optional[str | Path] = None,
) -> GateReport:
    """Evaluates the machine-checkable promotion criteria against evidence payload.

    Strict Fail-Closed Architecture:
    - Rejects historical proxy schemas and quote_observation snapshots.
    - Requires verified 64-hex SHA-256 hashes matching expected values for candidate, config, and data.
    - Requires >= 300 non-overlapping resolved trades (cannot lower floor).
    - Requires post-change timestamps and bounded freshness.
    - Recomputes all metrics directly from raw trade records.
    - Requires lower 95% CI > 0.0 bps at conservative cost >= 2.0 bps beating baseline.
    """
    failures: List[FailureReason] = []
    if now_utc is None:
        now_utc = datetime.now(timezone.utc)
    now_ts_sec = now_utc.timestamp()

    # --------------------------------------------------------------------------
    # 1. Schema & Evidence Kind Rejection Checks
    # --------------------------------------------------------------------------
    ev_schema = str(evidence.get("schema_version", "unknown"))
    ev_kind = str(evidence.get("kind", ""))

    schema_is_rejected_historical = ev_schema in REJECTED_HISTORICAL_SCHEMAS
    kind_is_rejected = ev_kind in REJECTED_EVIDENCE_KINDS

    if schema_is_rejected_historical:
        failures.append(
            FailureReason(
                code="HISTORICAL_PROXY_SCHEMA_REJECTED",
                message=(
                    f"Evidence uses historical backtest proxy schema '{ev_schema}'. "
                    "Historical signal proxies on trade prints are strictly rejected for promotion. "
                    "Forward resolved execution evidence ('gauntlet-forward-resolved-v1') is required."
                ),
                details={"found_schema": ev_schema},
            )
        )

    if kind_is_rejected or ev_kind == "quote_observation":
        failures.append(
            FailureReason(
                code="UNRESOLVED_QUOTE_OBSERVATION_REJECTED",
                message=(
                    f"Evidence contains unresolved quote observations (kind='{ev_kind}'). "
                    "Raw quote snapshots without simulated trade execution resolution cannot be evaluated for promotion."
                ),
                details={"found_kind": ev_kind},
            )
        )

    schema_valid = ev_schema in ACCEPTED_FORWARD_EVIDENCE_SCHEMAS and not schema_is_rejected_historical
    if not schema_valid and not schema_is_rejected_historical:
        failures.append(
            FailureReason(
                code="INVALID_EVIDENCE_SCHEMA",
                message=(
                    f"Evidence schema '{ev_schema}' is not recognized as forward execution evidence. "
                    f"Accepted: {sorted(list(ACCEPTED_FORWARD_EVIDENCE_SCHEMAS))}"
                ),
                details={"found_schema": ev_schema, "accepted_schemas": sorted(list(ACCEPTED_FORWARD_EVIDENCE_SCHEMAS))},
            )
        )

    # --------------------------------------------------------------------------
    # 2. Mandatory 64-Hex SHA-256 Hash Verification
    # --------------------------------------------------------------------------
    cand_hash_in_ev = str(evidence.get("candidate_hash", "") or "")
    data_stats = evidence.get("data_statistics", {})
    if not isinstance(data_stats, dict):
        data_stats = {}
    data_hash_in_ev = str(
        evidence.get("data_hash") or data_stats.get("data_sha256") or evidence.get("data_sha256") or ""
    )
    config_hash_in_ev = str(evidence.get("config_hash", "") or "")

    # Candidate Hash Check
    candidate_hash_matched = False
    if not expected_candidate_hash:
        failures.append(
            FailureReason(
                code="MISSING_MANDATORY_EXPECTED_CANDIDATE_HASH",
                message="Expected candidate code SHA-256 hash was not provided. Hash verification is mandatory.",
                details={"expected_candidate_hash": expected_candidate_hash},
            )
        )
    elif not is_valid_64hex(expected_candidate_hash):
        failures.append(
            FailureReason(
                code="INVALID_EXPECTED_CANDIDATE_HASH",
                message=f"Expected candidate hash is not a valid 64-character hexadecimal SHA-256 string: '{expected_candidate_hash}'",
                details={"expected_candidate_hash": expected_candidate_hash},
            )
        )
    elif not is_valid_64hex(cand_hash_in_ev):
        failures.append(
            FailureReason(
                code="INVALID_CANDIDATE_HASH_IN_EVIDENCE",
                message=f"Candidate hash in evidence is missing or not a valid 64-hex SHA-256 string: '{cand_hash_in_ev}'",
                details={"evidence_candidate_hash": cand_hash_in_ev},
            )
        )
    elif cand_hash_in_ev.lower() != expected_candidate_hash.lower():
        failures.append(
            FailureReason(
                code="CANDIDATE_HASH_MISMATCH",
                message=(
                    f"Candidate hash in evidence ({cand_hash_in_ev[:8]}...) does not match "
                    f"expected hash ({expected_candidate_hash[:8]}...)."
                ),
                details={"evidence_hash": cand_hash_in_ev, "expected_hash": expected_candidate_hash},
            )
        )
    else:
        candidate_hash_matched = True

    # Data Hash Check
    data_hash_matched = False
    if data_file_path:
        # If raw dataset file is specified, verify file exists and compute its hash
        data_p = Path(data_file_path)
        if not data_p.exists():
            failures.append(
                FailureReason(
                    code="DATA_FILE_NOT_FOUND",
                    message=f"Specified dataset file does not exist: {data_p}",
                    details={"data_file": str(data_p)},
                )
            )
        else:
            computed_file_hash = compute_file_sha256(data_p)
            if expected_data_hash and computed_file_hash.lower() != expected_data_hash.lower():
                failures.append(
                    FailureReason(
                        code="DATA_FILE_HASH_MISMATCH",
                        message=(
                            f"Computed SHA-256 of data file {data_p.name} ({computed_file_hash[:8]}...) "
                            f"does not match expected data hash ({expected_data_hash[:8]}...)."
                        ),
                        details={"computed_file_hash": computed_file_hash, "expected_hash": expected_data_hash},
                    )
                )

    if not expected_data_hash:
        failures.append(
            FailureReason(
                code="MISSING_MANDATORY_EXPECTED_DATA_HASH",
                message="Expected dataset SHA-256 hash was not provided. Hash verification is mandatory.",
                details={"expected_data_hash": expected_data_hash},
            )
        )
    elif not is_valid_64hex(expected_data_hash):
        failures.append(
            FailureReason(
                code="INVALID_EXPECTED_DATA_HASH",
                message=f"Expected data hash is not a valid 64-character hexadecimal SHA-256 string: '{expected_data_hash}'",
                details={"expected_data_hash": expected_data_hash},
            )
        )
    elif not is_valid_64hex(data_hash_in_ev):
        failures.append(
            FailureReason(
                code="INVALID_DATA_HASH_IN_EVIDENCE",
                message=f"Data hash in evidence is missing or not a valid 64-hex SHA-256 string: '{data_hash_in_ev}'",
                details={"evidence_data_hash": data_hash_in_ev},
            )
        )
    elif data_hash_in_ev.lower() != expected_data_hash.lower():
        failures.append(
            FailureReason(
                code="DATA_HASH_MISMATCH",
                message=(
                    f"Data hash in evidence ({data_hash_in_ev[:8]}...) does not match "
                    f"expected hash ({expected_data_hash[:8]}...)."
                ),
                details={"evidence_hash": data_hash_in_ev, "expected_hash": expected_data_hash},
            )
        )
    else:
        data_hash_matched = True

    # Config Hash Check
    config_hash_matched = False
    if not expected_config_hash:
        failures.append(
            FailureReason(
                code="MISSING_MANDATORY_EXPECTED_CONFIG_HASH",
                message="Expected strategy config SHA-256 hash was not provided. Hash verification is mandatory.",
                details={"expected_config_hash": expected_config_hash},
            )
        )
    elif not is_valid_64hex(expected_config_hash):
        failures.append(
            FailureReason(
                code="INVALID_EXPECTED_CONFIG_HASH",
                message=f"Expected config hash is not a valid 64-character hexadecimal SHA-256 string: '{expected_config_hash}'",
                details={"expected_config_hash": expected_config_hash},
            )
        )
    elif not is_valid_64hex(config_hash_in_ev):
        failures.append(
            FailureReason(
                code="INVALID_CONFIG_HASH_IN_EVIDENCE",
                message=f"Config hash in evidence is missing or not a valid 64-hex SHA-256 string: '{config_hash_in_ev}'",
                details={"evidence_config_hash": config_hash_in_ev},
            )
        )
    elif config_hash_in_ev.lower() != expected_config_hash.lower():
        failures.append(
            FailureReason(
                code="CONFIG_HASH_MISMATCH",
                message=(
                    f"Config hash in evidence ({config_hash_in_ev[:8]}...) does not match "
                    f"expected hash ({expected_config_hash[:8]}...)."
                ),
                details={"evidence_hash": config_hash_in_ev, "expected_hash": expected_config_hash},
            )
        )
    else:
        config_hash_matched = True

    hashes_valid_64hex = (
        is_valid_64hex(cand_hash_in_ev)
        and is_valid_64hex(data_hash_in_ev)
        and is_valid_64hex(config_hash_in_ev)
    )

    # --------------------------------------------------------------------------
    # 3. Sample Size Floor & Conservative Cost Constraints
    # --------------------------------------------------------------------------
    min_trades_passed = False
    effective_min_trades = max(MIN_TRADES_HARD_FLOOR, min_trades)
    if min_trades < MIN_TRADES_HARD_FLOOR:
        failures.append(
            FailureReason(
                code="SAMPLE_SIZE_FLOOR_CANNOT_BE_LOWERED",
                message=f"Requested minimum trades ({min_trades}) is below the non-negotiable floor of {MIN_TRADES_HARD_FLOOR}.",
                details={"requested_min_trades": min_trades, "hard_floor": MIN_TRADES_HARD_FLOOR},
            )
        )

    cost_floor_passed = min_cost_bps >= MIN_COST_BPS_HARD_FLOOR
    if not cost_floor_passed:
        failures.append(
            FailureReason(
                code="INSUFFICIENT_COST_FLOOR",
                message=f"Evaluation cost {min_cost_bps} bps is lower than conservative requirement of >= {MIN_COST_BPS_HARD_FLOOR} bps.",
                details={"requested_cost_bps": min_cost_bps, "hard_floor": MIN_COST_BPS_HARD_FLOOR},
            )
        )

    # --------------------------------------------------------------------------
    # 4. Post-Change Timing & Freshness Verification
    # --------------------------------------------------------------------------
    post_change_timing_passed = False
    post_change_ms = parse_timestamp_ms(post_change_since or evidence.get("post_change_since") or evidence.get("post_change_since_ms") or evidence.get("post_change_since_utc"))
    if post_change_ms is None:
        failures.append(
            FailureReason(
                code="MISSING_POST_CHANGE_TIMESTAMP",
                message="Mandatory post-change observation timestamp floor (post_change_since) was not provided.",
                details={"post_change_since": post_change_since},
            )
        )
    else:
        post_change_timing_passed = True

    freshness_passed = False
    max_ev_ts_ms: Optional[int] = None
    ev_gen_ms = parse_timestamp_ms(evidence.get("generated_at_utc") or evidence.get("generated_at_ms") or evidence.get("timestamp_utc"))

    # --------------------------------------------------------------------------
    # 5. Extract Split & Raw Execution Evidence
    # --------------------------------------------------------------------------
    metrics = evidence.get("metrics", {})
    if not isinstance(metrics, dict):
        metrics = {}

    split_metrics = metrics.get(split_name, {})
    if not isinstance(split_metrics, dict):
        split_metrics = {}

    # Support raw trades nested in metrics[split_name][strategy] or top-level candidate_trades
    candidate_container = split_metrics.get(candidate_name) or evidence.get("candidate", {})
    baseline_container = split_metrics.get(baseline_name) or evidence.get("baseline", {})

    candidate_found = isinstance(candidate_container, dict) and bool(candidate_container)
    baseline_found = isinstance(baseline_container, dict) and bool(baseline_container)

    if not candidate_found:
        failures.append(
            FailureReason(
                code="CANDIDATE_STRATEGY_NOT_FOUND",
                message=f"Candidate strategy '{candidate_name}' not found in split '{split_name}'.",
                details={"available_strategies": list(split_metrics.keys())},
            )
        )

    if not baseline_found:
        failures.append(
            FailureReason(
                code="BASELINE_STRATEGY_NOT_FOUND",
                message=f"Baseline strategy '{baseline_name}' not found in split '{split_name}'.",
                details={"available_strategies": list(split_metrics.keys())},
            )
        )

    # --------------------------------------------------------------------------
    # 6. Raw Trade Recomputation & Data Integrity Checks
    # --------------------------------------------------------------------------
    cand_recomputed: Optional[RecomputedMetrics] = None
    base_recomputed: Optional[RecomputedMetrics] = None

    actual_resolved_trades = 0
    candidate_net_ev = 0.0
    candidate_ci_low = 0.0
    candidate_se = 0.0
    gap_censored_trades = 0
    overlapping_trades = 0
    fill_model_valid = False
    finite_metrics_passed = True

    if candidate_found:
        raw_cand_trades = candidate_container.get("trades") or candidate_container.get("raw_trades") or evidence.get("candidate_trades")
        cand_recomputed, cand_recompute_failures = validate_and_recompute_trades(
            raw_trades=raw_cand_trades,
            cost_bps=min_cost_bps,
            post_change_since_ms=post_change_ms,
        )
        failures.extend(cand_recompute_failures)

        if cand_recomputed is not None:
            actual_resolved_trades = cand_recomputed.resolved_trades_count
            candidate_net_ev = cand_recomputed.mean_net_ev_bps
            candidate_ci_low = cand_recomputed.ci_95_low_bps
            candidate_se = cand_recomputed.se_bps
            gap_censored_trades = cand_recomputed.gap_censored_count
            overlapping_trades = cand_recomputed.overlapping_count
            fill_model_valid = len(cand_recomputed.fill_models_observed) > 0 and all(
                m in VALID_EXECUTABLE_FILL_MODELS for m in cand_recomputed.fill_models_observed
            )
            max_ev_ts_ms = cand_recomputed.max_timestamp_ms

            if cand_recomputed.malformed_count > 0:
                finite_metrics_passed = False

            # Sample size check
            min_trades_passed = actual_resolved_trades >= effective_min_trades
            if not min_trades_passed:
                failures.append(
                    FailureReason(
                        code="INSUFFICIENT_SAMPLE_SIZE",
                        message=(
                            f"Actual resolved non-overlapping trades ({actual_resolved_trades}) is below "
                            f"minimum required ({effective_min_trades})."
                        ),
                        details={"actual_trades": actual_resolved_trades, "required_min": effective_min_trades},
                    )
                )

    # Freshness Check
    latest_ts_ms = max_ev_ts_ms or ev_gen_ms
    if latest_ts_ms is not None:
        latest_ts_sec = latest_ts_ms / 1000.0
        age_seconds = now_ts_sec - latest_ts_sec
        if age_seconds < -60.0:  # Allow 60s clock skew
            failures.append(
                FailureReason(
                    code="FUTURE_TIMESTAMP_DETECTED",
                    message=f"Evidence timestamp ({latest_ts_sec}) is in the future relative to evaluation time ({now_ts_sec}).",
                    details={"evidence_ts": latest_ts_sec, "now_ts": now_ts_sec},
                )
            )
        elif age_seconds > max_age_seconds:
            failures.append(
                FailureReason(
                    code="STALE_EVIDENCE",
                    message=(
                        f"Evidence age ({age_seconds / 3600.0:.1f} hours) exceeds maximum allowed freshness "
                        f"window of {max_age_seconds / 3600.0:.1f} hours."
                    ),
                    details={"age_hours": round(age_seconds / 3600.0, 2), "max_age_hours": round(max_age_seconds / 3600.0, 2)},
                )
            )
        else:
            freshness_passed = True
    else:
        failures.append(
            FailureReason(
                code="MISSING_EVIDENCE_TIMESTAMP",
                message="Evidence does not contain valid observation or generation timestamps for freshness verification.",
            )
        )

    # Baseline Recomputation / Lookup
    baseline_net_ev = 0.0
    if baseline_found:
        raw_base_trades = baseline_container.get("trades") or baseline_container.get("raw_trades") or evidence.get("baseline_trades")
        if raw_base_trades is not None:
            base_recomputed, base_recompute_failures = validate_and_recompute_trades(
                raw_trades=raw_base_trades,
                cost_bps=min_cost_bps,
                post_change_since_ms=post_change_ms,
            )
            # Baseline failures also invalidate promotion if baseline is corrupted
            failures.extend(base_recompute_failures)
            if base_recomputed is not None:
                baseline_net_ev = base_recomputed.mean_net_ev_bps
                baseline_age = now_ts_sec - base_recomputed.max_timestamp_ms / 1000.0
                if baseline_age < -60.0 or baseline_age > max_age_seconds:
                    freshness_passed = False
                    failures.append(FailureReason(
                        code="INVALID_BASELINE_FRESHNESS",
                        message="Baseline executions are future-dated or stale.",
                    ))
                if cand_recomputed is not None and (
                    base_recomputed.min_timestamp_ms > cand_recomputed.max_timestamp_ms
                    or cand_recomputed.min_timestamp_ms > base_recomputed.max_timestamp_ms
                ):
                    failures.append(FailureReason(
                        code="DISJOINT_COMPARISON_WINDOWS",
                        message="Candidate and baseline observation periods do not overlap.",
                    ))
        else:
            failures.append(FailureReason(
                code="MISSING_BASELINE_RAW_TRADES",
                message="Baseline raw executions are required; aggregate metrics are not evidence.",
            ))

    # --------------------------------------------------------------------------
    # 7. Statistical Edge & Baseline Superiority
    # --------------------------------------------------------------------------
    positive_lower_ci_passed = False
    beats_no_trade_passed = False
    baseline_improvement_passed = False
    baseline_improvement = round(candidate_net_ev - baseline_net_ev, 4)

    if cand_recomputed is not None and cost_floor_passed:
        # Positive lower bound (95% CI low > 0)
        positive_lower_ci_passed = candidate_ci_low > 0.0
        if not positive_lower_ci_passed:
            failures.append(
                FailureReason(
                    code="NEGATIVE_OR_ZERO_LOWER_CI",
                    message=(
                        f"Candidate 95% CI lower bound ({candidate_ci_low:+.2f} bps) is not strictly "
                        f"positive (> 0.0) at {min_cost_bps} bps cost."
                    ),
                    details={
                        "ci_95_low": candidate_ci_low,
                        "net_ev": candidate_net_ev,
                        "se": candidate_se,
                        "cost_bps": min_cost_bps,
                    },
                )
            )

        # Beats flat no-trade baseline (EV > 0)
        beats_no_trade_passed = candidate_net_ev > 0.0
        if not beats_no_trade_passed:
            failures.append(
                FailureReason(
                    code="NEGATIVE_OR_ZERO_NET_EV",
                    message=f"Candidate net EV ({candidate_net_ev:+.2f} bps) does not beat no-trade baseline (> 0.0 bps) at {min_cost_bps} bps cost.",
                    details={"net_ev": candidate_net_ev, "cost_bps": min_cost_bps},
                )
            )

        # Beats benchmark baseline
        if baseline_found:
            baseline_improvement_passed = baseline_improvement > 0.0
            if not baseline_improvement_passed:
                failures.append(
                    FailureReason(
                        code="BELOW_BASELINE_EDGE",
                        message=(
                            f"Candidate net EV ({candidate_net_ev:+.2f} bps) does not outperform baseline "
                            f"'{baseline_name}' ({baseline_net_ev:+.2f} bps) at {min_cost_bps} bps cost."
                        ),
                        details={
                            "candidate_net_ev": candidate_net_ev,
                            "baseline_net_ev": baseline_net_ev,
                            "improvement_bps": baseline_improvement,
                        },
                    )
                )

    zero_gap_censored_passed = gap_censored_trades == 0 and cand_recomputed is not None
    zero_overlapping_passed = overlapping_trades == 0 and cand_recomputed is not None

    gate_passed = len(failures) == 0
    verdict = "PASS" if gate_passed else "FAIL"

    criteria_eval = GateCriteriaEvaluation(
        schema_version_valid=schema_valid,
        evidence_schema=ev_schema,
        evidence_kind=ev_kind,
        candidate_found=candidate_found,
        candidate_name=candidate_name,
        baseline_found=baseline_found,
        baseline_name=baseline_name,
        split_evaluated=split_name,
        min_trades_required=effective_min_trades,
        actual_resolved_trades=actual_resolved_trades,
        min_trades_passed=min_trades_passed,
        cost_bps_evaluated=min_cost_bps,
        cost_floor_passed=cost_floor_passed,
        candidate_net_ev_bps=candidate_net_ev,
        candidate_ci_95_low_bps=candidate_ci_low,
        candidate_se_bps=candidate_se,
        positive_lower_ci_passed=positive_lower_ci_passed,
        baseline_net_ev_bps=baseline_net_ev,
        baseline_improvement_bps=baseline_improvement,
        baseline_improvement_passed=baseline_improvement_passed,
        beats_no_trade_passed=beats_no_trade_passed,
        gap_censored_trades=gap_censored_trades,
        zero_gap_censored_passed=zero_gap_censored_passed,
        overlapping_trades=overlapping_trades,
        zero_overlapping_passed=zero_overlapping_passed,
        candidate_hash_matched=candidate_hash_matched,
        data_hash_matched=data_hash_matched,
        config_hash_matched=config_hash_matched,
        hashes_valid_64hex=hashes_valid_64hex,
        fill_model_valid=fill_model_valid,
        freshness_passed=freshness_passed,
        post_change_timing_passed=post_change_timing_passed,
        finite_metrics_passed=finite_metrics_passed,
    )

    exact_next_proof = (
        "## Exact Next Proof Required for Promotion\n"
        "Promotion is fail-closed. To achieve a PASS verdict, the following forward executable evidence must be recorded and submitted:\n"
        "1. Schema: Must be 'gauntlet-forward-resolved-v1' containing raw forward trade execution records (not proxy trade prints or unsimulated quote snapshots).\n"
        f"2. Sample Size: >= {effective_min_trades} non-overlapping resolved trades recorded strictly post-change (>= {post_change_ms or 'post_change_since'}).\n"
        "3. Execution Integrity: Zero gap-censored trades, zero overlapping executions, explicit realistic executable fill model ('resting_limit_queue', 'forward_quote_depth', 'taker_crossing').\n"
        "4. Identity & Hashes: Verified 64-character hexadecimal SHA-256 hashes matching expected deployment hashes for candidate code, active strategy config, and orderbook/tick dataset.\n"
        f"5. Freshness: Evidence timestamp bounded within maximum allowable age (<= {max_age_seconds / 3600.0:.1f} hours) relative to evaluation time.\n"
        f"6. Statistical Edge: Independent recomputed Net EV at cost >= {min_cost_bps:.1f} bps must have lower 95% CI bound > 0.0 bps and strictly outperform baseline."
    )

    report = GateReport(
        schema_version=SHADOW_GATE_SCHEMA_VERSION,
        gate_name="ShadowPromotionGate",
        verdict=verdict,
        passed=gate_passed,
        trading_enabled=False,  # Strict invariant: never enable trading
        execution_mode="VERIFICATION_ONLY",
        evaluation_timestamp_utc=now_utc.isoformat(),
        evidence_file=evidence_file_path,
        candidate={
            "name": candidate_name,
            "hash": cand_hash_in_ev,
            "split": split_name,
        },
        baseline={
            "name": baseline_name,
            "split": split_name,
        },
        criteria=criteria_eval,
        recomputed_candidate_metrics=asdict(cand_recomputed) if cand_recomputed else None,
        recomputed_baseline_metrics=asdict(base_recomputed) if base_recomputed else None,
        failures=failures,
        exact_next_proof_required=exact_next_proof,
        disclaimer=(
            "Machine-checkable promotion verification gate only. "
            "A PASS verdict indicates mathematical and statistical criteria are satisfied on forward resolved trades. "
            "Trading is NEVER enabled by this gate."
        ),
    )

    return report


def format_gate_markdown(report: GateReport) -> str:
    """Formats human-readable markdown summary of gate evaluation."""
    lines = []
    lines.append(f"# Shadow Promotion Gate Report: {report.verdict}")
    lines.append("")
    lines.append(f"> **Verdict:** `{report.verdict}` | **Trading Enabled:** `False` (Immutable Invariant)")
    lines.append(f"> **Timestamp (UTC):** `{report.evaluation_timestamp_utc}` | **Schema:** `{report.schema_version}`")
    lines.append("")

    c = report.criteria
    lines.append("## 1. Candidate Specification & Hashes")
    lines.append(f"- **Candidate Strategy:** `{c.candidate_name}`")
    lines.append(f"- **Candidate Hash:** `{report.candidate.get('hash', 'N/A')}`")
    lines.append(f"- **Baseline Strategy:** `{c.baseline_name}`")
    lines.append(f"- **Evaluation Partition:** `{c.split_evaluated}`")
    lines.append(f"- **Evidence File:** `{report.evidence_file}`")
    lines.append("")

    lines.append("## 2. Gate Criteria Checklist")
    lines.append("| Criterion | Required | Measured / Status | Pass/Fail |")
    lines.append("| :--- | :---: | :---: | :---: |")
    lines.append(f"| Evidence Schema | Forward Resolved | `{c.evidence_schema}` | {'PASS' if c.schema_version_valid else 'FAIL'} |")
    lines.append(f"| Candidate Hash Match | 64-Hex Expected | `{'Matched' if c.candidate_hash_matched else 'Mismatch/Missing'}` | {'PASS' if c.candidate_hash_matched else 'FAIL'} |")
    lines.append(f"| Data Hash Match | 64-Hex Expected | `{'Matched' if c.data_hash_matched else 'Mismatch/Missing'}` | {'PASS' if c.data_hash_matched else 'FAIL'} |")
    lines.append(f"| Config Hash Match | 64-Hex Expected | `{'Matched' if c.config_hash_matched else 'Mismatch/Missing'}` | {'PASS' if c.config_hash_matched else 'FAIL'} |")
    lines.append(f"| Resolved Trades | >= {c.min_trades_required} (Floor >= 300) | `{c.actual_resolved_trades}` trades | {'PASS' if c.min_trades_passed else 'FAIL'} |")
    lines.append(f"| Non-Overlapping Trades | Zero Overlap | `{c.overlapping_trades}` overlapping | {'PASS' if c.zero_overlapping_passed else 'FAIL'} |")
    lines.append(f"| Gap-Censored Trades | == 0 | `{c.gap_censored_trades}` censored | {'PASS' if c.zero_gap_censored_passed else 'FAIL'} |")
    lines.append(f"| Post-Change Timing | >= post_change_since | `{'Valid' if c.post_change_timing_passed else 'Pre-change / Missing'}` | {'PASS' if c.post_change_timing_passed else 'FAIL'} |")
    lines.append(f"| Evidence Freshness | <= Max Age Window | `{'Fresh' if c.freshness_passed else 'Stale / Missing'}` | {'PASS' if c.freshness_passed else 'FAIL'} |")
    lines.append(f"| Executable Fill Model | Valid Realistic | `{'Valid' if c.fill_model_valid else 'Invalid / Proxy'}` | {'PASS' if c.fill_model_valid else 'FAIL'} |")
    lines.append(f"| Cost Sensitivity | >= 2.0 bps | `{c.cost_bps_evaluated:.1f}` bps | {'PASS' if c.cost_floor_passed else 'FAIL'} |")
    lines.append(f"| Lower 95% CI Bound | > 0.0 bps | `{c.candidate_ci_95_low_bps:+.2f}` bps (EV: `{c.candidate_net_ev_bps:+.2f}`) | {'PASS' if c.positive_lower_ci_passed else 'FAIL'} |")
    lines.append(f"| Beats No-Trade | > 0.0 bps | `{c.candidate_net_ev_bps:+.2f}` bps | {'PASS' if c.beats_no_trade_passed else 'FAIL'} |")
    lines.append(f"| Baseline Improvement | > Baseline EV | `{c.baseline_improvement_bps:+.2f}` bps vs `{c.baseline_name}` (`{c.baseline_net_ev_bps:+.2f}`) | {'PASS' if c.baseline_improvement_passed else 'FAIL'} |")
    lines.append("")

    if report.failures:
        lines.append("## 3. Failure Reasons")
        for idx, f in enumerate(report.failures, 1):
            lines.append(f"{idx}. **`{f.code}`**: {f.message}")
        lines.append("")
    else:
        lines.append("## 3. Findings")
        lines.append("All statistical and integrity requirements passed. Candidate strategy meets prerequisites for shadow orderbook simulation.")
        lines.append("")

    lines.append(report.exact_next_proof_required)
    lines.append("")
    lines.append("## 5. Fundamental Quant Reality")
    lines.append(report.disclaimer)
    lines.append("")
    return "\n".join(lines)


def print_cli_summary(report: GateReport) -> None:
    """Prints terminal summary table."""
    c = report.criteria
    print("\n" + "=" * 90)
    print(f" PIRANA SHADOW PROMOTION GATE: [{report.verdict}] ")
    print("=" * 90)
    print(f"Candidate: {c.candidate_name:<30} | Baseline: {c.baseline_name}")
    print(f"Split:     {c.split_evaluated:<30} | Cost:     {c.cost_bps_evaluated:.1f} bps (floor: >=2.0 bps)")
    print(f"Trades:    {c.actual_resolved_trades:<30} | Min Req:  {c.min_trades_required} (Floor >= 300)")
    print(f"Net EV:    {c.candidate_net_ev_bps:+7.2f} bps                     | 95% CI:   [{c.candidate_ci_95_low_bps:+.2f}, {c.candidate_net_ev_bps + 1.96*c.candidate_se_bps:+.2f}] bps")
    print(f"Censored:  {c.gap_censored_trades:<30} | Overlap:  {c.overlapping_trades}")
    print(f"Trading:   DISABLED (Invariant: Gate never enables trading)")
    print("-" * 90)

    if report.failures:
        print("\n[FAILURES]")
        for f in report.failures:
            print(f"  * [{f.code}] {f.message}")
        print("-" * 90)
        print("\n" + report.exact_next_proof_required)
        print("-" * 90)

    print(f"\nFinal Verdict: {report.verdict}\n" + "=" * 90 + "\n")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Machine-checkable fail-closed promotion gate for Pirana entry signals."
    )
    parser.add_argument(
        "--evidence",
        "-e",
        type=str,
        required=True,
        help="Path to forward resolved evaluation JSON artifact",
    )
    parser.add_argument(
        "--candidate",
        "-c",
        type=str,
        default="Pullback_Flow_ForwardShadow",
        help="Candidate strategy name (default: Pullback_Flow_ForwardShadow)",
    )
    parser.add_argument(
        "--baseline",
        "-b",
        type=str,
        default="Buying_Pressure_Baseline",
        help="Baseline strategy name (default: Buying_Pressure_Baseline)",
    )
    parser.add_argument(
        "--split",
        "-s",
        type=str,
        default="forward_shadow",
        help="Split partition to evaluate (default: forward_shadow)",
    )
    parser.add_argument(
        "--min-trades",
        "-n",
        type=int,
        default=300,
        help="Minimum non-overlapping resolved trades required (default: 300, floor cannot lower)",
    )
    parser.add_argument(
        "--min-cost-bps",
        "-k",
        type=float,
        default=2.0,
        help="Conservative cost level in bps (default: 2.0, floor >= 2.0)",
    )
    parser.add_argument(
        "--expected-candidate-hash",
        type=str,
        default=None,
        help="Expected SHA-256 hash of candidate script/binary (mandatory 64-hex)",
    )
    parser.add_argument(
        "--expected-data-hash",
        type=str,
        default=None,
        help="Expected SHA-256 hash of tick dataset (mandatory 64-hex)",
    )
    parser.add_argument(
        "--expected-config-hash",
        type=str,
        default=None,
        help="Expected SHA-256 hash of strategy config (mandatory 64-hex)",
    )
    parser.add_argument(
        "--data-file",
        type=str,
        default=None,
        help="Path to raw dataset file to independently compute and verify SHA-256",
    )
    parser.add_argument(
        "--post-change-since",
        type=str,
        default=None,
        help="Timestamp floor (ISO-8601 or epoch ms). All observations must be strictly >= this floor.",
    )
    parser.add_argument(
        "--max-age-hours",
        type=float,
        default=168.0,
        help="Maximum allowable evidence age in hours (default: 168.0 = 7 days)",
    )
    parser.add_argument(
        "--output",
        "-o",
        type=str,
        default=None,
        help="Path to output JSON report",
    )
    parser.add_argument(
        "--report",
        "-r",
        type=str,
        default=None,
        help="Path to output Markdown report",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="Suppress terminal output",
    )

    args = parser.parse_args()

    evidence_path = Path(args.evidence)
    if not evidence_path.exists():
        print(f"Error: Evidence file {evidence_path} does not exist.", file=sys.stderr)
        sys.exit(1)

    try:
        with open(evidence_path, "r", encoding="utf-8") as f:
            evidence_data = json.load(f)
    except Exception as exc:
        print(f"Error: Failed to parse evidence JSON: {exc}", file=sys.stderr)
        sys.exit(1)

    report = evaluate_promotion_gate(
        evidence=evidence_data,
        candidate_name=args.candidate,
        baseline_name=args.baseline,
        split_name=args.split,
        min_trades=args.min_trades,
        min_cost_bps=args.min_cost_bps,
        expected_candidate_hash=args.expected_candidate_hash,
        expected_data_hash=args.expected_data_hash,
        expected_config_hash=args.expected_config_hash,
        evidence_file_path=str(evidence_path),
        post_change_since=args.post_change_since,
        max_age_seconds=args.max_age_hours * 3600.0,
        data_file_path=args.data_file,
    )

    if not args.quiet:
        print_cli_summary(report)

    report_dict = asdict(report)

    if args.output:
        out_path = Path(args.output)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        with open(out_path, "w", encoding="utf-8") as f:
            json.dump(report_dict, f, indent=2)
        print(f"Saved Gate JSON to {out_path}")

    if args.report:
        md_path = Path(args.report)
        md_path.parent.mkdir(parents=True, exist_ok=True)
        md_content = format_gate_markdown(report)
        with open(md_path, "w", encoding="utf-8") as f:
            f.write(md_content)
        print(f"Saved Gate Markdown report to {md_path}")

    # Return code: 0 if PASS, 1 if FAIL
    sys.exit(0 if report.passed else 1)


if __name__ == "__main__":
    main()
