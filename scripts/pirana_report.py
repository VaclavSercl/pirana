#!/usr/bin/env python3
"""
ČÁSLAV :: Pirana Institutional Performance & PnL Reporter
Read-only canonical account accounting projection reporter.
Legacy estimates require explicit --legacy and are never verified accounting.

Strictly adhering to:
- §1b Bitcoin Standard: Primary accounting unit is Satoshi.
- §4 Evidence Standard: No fabricated numbers, authoritative source labels.
- §8/§9 Reporting Doctrine: Explicit since_restart vs calendar_day vs ledger_lifetime.
- Concatenated JSON recovery, shadow/rebalance exclusion, malformed/unverifiable tracking.
"""

from __future__ import annotations

import argparse
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
import html
import json
import math
import os
import sys
from typing import Any, Dict, List, Optional, Tuple
import urllib.error
import urllib.request
import zoneinfo

DEFAULT_LEDGER_PATH = "/var/lib/pirana/trade_ledger.jsonl"
DEFAULT_SNAPSHOT_URLS = [
    "http://127.0.0.1:80/api/snapshot",
    "http://127.0.0.1:8080/api/snapshot",
]
DEFAULT_TIMEZONE = "Europe/Prague"
SATS_PER_BTC = 100_000_000.0


@dataclass
class ClosedTradeRecord:
    ts: int
    pnl_sats: float
    fill_price: float
    qty: float
    side: str
    fee_sats: float = 0.0
    cid: str = ""
    order_id: int = 0
    trade_id: int = 0
    vpin_at_close: float = 0.0
    raw_dict: Dict[str, Any] = field(default_factory=dict)
    is_unverifiable_order: bool = False
    unverifiable_reasons: List[str] = field(default_factory=list)

    @property
    def usd_pnl_reconstructed(self) -> float:
        """Summed from recorded fill price: pnl_sats * fill_price / 1e8."""
        return (self.pnl_sats * self.fill_price) / SATS_PER_BTC

    @property
    def fee_usd_reconstructed(self) -> float:
        """Reconstructed fee in USD: fee_sats * fill_price / 1e8."""
        return (self.fee_sats * self.fill_price) / SATS_PER_BTC


@dataclass
class TimeframeMetrics:
    name: str
    label: str
    period_start_ts: Optional[int]
    period_end_ts: Optional[int]
    period_start_iso: Optional[str]
    period_end_iso: Optional[str]
    is_available: bool = True
    unavailable_reason: Optional[str] = None
    
    trade_count: int = 0
    pnl_sats: float = 0.0
    pnl_usd_reconstructed: float = 0.0
    pnl_usd_semantics: str = "reconstructed_approx_from_fill_price"
    fee_sats: float = 0.0
    fee_usd_reconstructed: float = 0.0
    
    win_count: int = 0
    loss_count: int = 0
    zero_count: int = 0
    win_rate_closed_pct: float = 0.0  # wins / (wins + losses)
    win_rate_total_pct: float = 0.0   # wins / total_count
    
    gross_profit_sats: float = 0.0
    gross_loss_sats: float = 0.0
    profit_factor: Optional[float] = None
    avg_win_sats: float = 0.0
    avg_loss_sats: float = 0.0
    payoff_ratio: Optional[float] = None
    
    first_trade_ts: Optional[int] = None
    first_trade_iso: Optional[str] = None
    last_trade_ts: Optional[int] = None
    last_trade_iso: Optional[str] = None
    inactivity_gap_seconds: Optional[int] = None
    max_inter_trade_gap_seconds: Optional[int] = None
    unverifiable_orders_count: int = 0


@dataclass
class DataIntegritySummary:
    total_raw_segments_found: int = 0
    legacy_estimate_records: int = 0
    shadow_trades_excluded: int = 0
    rebalance_trades_excluded: int = 0
    malformed_segments_count: int = 0
    unverifiable_orders_count: int = 0
    malformed_errors: List[str] = field(default_factory=list)
    unverifiable_order_warnings: List[str] = field(default_factory=list)


@dataclass
class EquityState:
    source: str
    is_available: bool
    btc_balance: float = 0.0
    locked_btc_reserve: float = 0.0
    total_btc: float = 0.0
    usd_balance: float = 0.0
    last_close_btc_usd: float = 0.0
    last_close_source: str = "N/A"
    total_equity_usd: float = 0.0
    total_equity_sats: float = 0.0
    system_mode: str = "UNKNOWN"
    uptime_seconds: Optional[int] = None
    consecutive_losses: int = 0
    api_daily_pnl_usd: Optional[float] = None
    api_total_pnl_usd: Optional[float] = None


@dataclass
class ReportData:
    generated_at_ts: int
    generated_at_iso: str
    timezone_name: str
    ledger_path: str
    snapshot_source: str
    equity: EquityState
    integrity: DataIntegritySummary
    timeframes: Dict[str, TimeframeMetrics]
    warnings: List[str]
    provenance: Dict[str, List[str]]


def parse_timestamp_or_now(now_arg: Optional[str], tz: zoneinfo.ZoneInfo) -> datetime:
    """Parses ISO string, integer timestamp, or returns current time in tz."""
    if not now_arg:
        return datetime.now(tz)
    now_str = str(now_arg).strip()
    try:
        # Check numeric timestamp
        ts_val = float(now_str)
        return datetime.fromtimestamp(ts_val, tz=tz)
    except ValueError:
        pass

    # Try ISO formats
    try:
        dt = datetime.fromisoformat(now_str)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=tz)
        else:
            dt = dt.astimezone(tz)
        return dt
    except Exception as e:
        raise ValueError(f"Could not parse --now value {now_arg!r}: {e}")


def decode_concatenated_json(text: str) -> Tuple[List[Dict[str, Any]], List[str]]:
    """
    Decodes concatenated or line-by-line JSON objects from text.
    Handles multiple JSONs on one line ({...}{...}), whitespaces, and damaged segments.
    Never silently hides errors.
    """
    decoder = json.JSONDecoder()
    idx = 0
    n = len(text)
    records: List[Dict[str, Any]] = []
    malformed: List[str] = []

    while idx < n:
        # Skip leading whitespace
        while idx < n and text[idx].isspace():
            idx += 1
        if idx >= n:
            break

        # Search next opening brace '{'
        next_brace = text.find('{', idx)
        if next_brace == -1:
            remainder = text[idx:].strip()
            if remainder:
                malformed.append(f"Ignored non-JSON trailing data: {remainder[:80]!r}")
            break

        if next_brace > idx:
            skipped = text[idx:next_brace].strip()
            if skipped:
                malformed.append(f"Skipped malformed segment before JSON object: {skipped[:80]!r}")
            idx = next_brace

        try:
            obj, end_idx = decoder.raw_decode(text, idx)
            idx = end_idx
            if isinstance(obj, dict):
                records.append(obj)
            else:
                malformed.append(f"Parsed JSON element is not an object/dict: {type(obj).__name__}")
        except json.JSONDecodeError as err:
            malformed.append(f"JSON decode error at position {idx}: {err.msg}")
            # Advance past '{' to search for next potential record
            idx += 1

    return records, malformed


def validate_and_classify_trade(
    raw: Dict[str, Any]
) -> Tuple[Optional[ClosedTradeRecord], str, Optional[str]]:
    """
    Validates record fields and classifies trade:
    Returns (record, category, error_msg).
    Categories: 'live', 'shadow', 'rebalance', 'invalid'.
    """
    # Required core fields
    if "pnl_sats" not in raw or "ts" not in raw or "fill_price" not in raw:
        return None, "invalid", f"Missing essential trade fields (pnl_sats, ts, or fill_price): keys={list(raw.keys())}"

    try:
        ts = int(raw["ts"])
        pnl_sats = float(raw["pnl_sats"])
        fill_price = float(raw["fill_price"])
        qty = float(raw.get("qty", 0.0))
        side = str(raw.get("side", ""))
        fee_sats = float(raw.get("fee_sats", 0.0))
        cid = str(raw.get("cid", "")).strip()
        order_id = int(raw.get("order_id", 0))
        trade_id = int(raw.get("trade_id", 0))
        vpin = float(raw.get("vpin_at_close", 0.0))
    except (ValueError, TypeError, OverflowError) as e:
        return None, "invalid", f"Field type conversion error: {e}"

    if not all(math.isfinite(v) for v in (pnl_sats, fill_price, qty, fee_sats, vpin)):
        return None, "invalid", "Non-finite numeric trade field"
    try:
        datetime.fromtimestamp(ts, tz=timezone.utc)
    except (ValueError, OverflowError, OSError):
        return None, "invalid", "Timestamp outside supported range"

    if fill_price <= 0.0:
        return None, "invalid", f"Non-positive fill price: {fill_price}"

    # Exclusion filters
    cid_lower = cid.lower()
    if cid_lower.startswith("shadow") or "shadow" in cid_lower:
        return None, "shadow", None
    if "rebalance" in cid_lower:
        return None, "rebalance", None

    # Unverifiable order ID validation
    unverifiable_reasons = []
    if order_id <= 0:
        unverifiable_reasons.append(f"order_id={order_id} is non-positive or zero")
    if not cid:
        unverifiable_reasons.append("cid is empty")
    if trade_id <= 0:
        unverifiable_reasons.append(f"trade_id={trade_id} is non-positive")

    if "fee_sats" not in raw or "fee_currency" not in raw:
        unverifiable_reasons.append("fee evidence missing")
    # Legacy close summaries never independently prove execution or cost basis.
    unverifiable_reasons.append("legacy summary lacks authenticated fill reconciliation")
    is_unverifiable = len(unverifiable_reasons) > 0

    record = ClosedTradeRecord(
        ts=ts,
        pnl_sats=pnl_sats,
        fill_price=fill_price,
        qty=qty,
        side=side,
        fee_sats=fee_sats,
        cid=cid,
        order_id=order_id,
        trade_id=trade_id,
        vpin_at_close=vpin,
        raw_dict=raw,
        is_unverifiable_order=is_unverifiable,
        unverifiable_reasons=unverifiable_reasons,
    )
    return record, "live", None


def load_ledger_data(ledger_path: str) -> Tuple[List[ClosedTradeRecord], DataIntegritySummary]:
    """
    Reads ledger file safely.
    Handles concatenated records, exclusions, and records all errors.
    """
    integrity = DataIntegritySummary()

    if not os.path.exists(ledger_path):
        integrity.malformed_errors.append(f"Ledger file not found at: {ledger_path}")
        return [], integrity

    try:
        with open(ledger_path, "r", encoding="utf-8", errors="replace") as f:
            content = f.read()
    except Exception as e:
        integrity.malformed_errors.append(f"Failed to read ledger file {ledger_path}: {e}")
        return [], integrity

    raw_records, malformed_chunks = decode_concatenated_json(content)
    integrity.total_raw_segments_found = len(raw_records) + len(malformed_chunks)
    integrity.malformed_segments_count = len(malformed_chunks)
    integrity.malformed_errors.extend(malformed_chunks)

    live_trades: List[ClosedTradeRecord] = []
    for raw in raw_records:
        record, category, err = validate_and_classify_trade(raw)
        if category == "live" and record is not None:
            live_trades.append(record)
            if record.is_unverifiable_order:
                integrity.unverifiable_orders_count += 1
                integrity.unverifiable_order_warnings.append(
                    f"Trade ts={record.ts} cid={record.cid!r} order_id={record.order_id}: {', '.join(record.unverifiable_reasons)}"
                )
        elif category == "shadow":
            integrity.shadow_trades_excluded += 1
        elif category == "rebalance":
            integrity.rebalance_trades_excluded += 1
        else:
            integrity.malformed_segments_count += 1
            if err:
                integrity.malformed_errors.append(err)

    integrity.legacy_estimate_records = len(live_trades)
    # Sort chronologically by timestamp
    live_trades.sort(key=lambda t: t.ts)
    return live_trades, integrity


def fetch_snapshot(
    snapshot_file: Optional[str] = None,
    api_url: Optional[str] = None,
    no_api: bool = False,
    timeout_s: float = 3.0,
) -> Tuple[Optional[Dict[str, Any]], str, List[str]]:
    """
    Fetches snapshot from explicit file or localhost API.
    Returns (snapshot_dict, source_label, warnings).
    """
    warnings: List[str] = []

    if snapshot_file:
        if os.path.exists(snapshot_file):
            try:
                with open(snapshot_file, "r", encoding="utf-8") as f:
                    data = json.load(f)
                    return data, f"FILE: {snapshot_file}", warnings
            except Exception as e:
                warnings.append(f"Failed to read snapshot file {snapshot_file}: {e}")
                return None, "FAILED_FILE", warnings
        else:
            warnings.append(f"Specified snapshot file does not exist: {snapshot_file}")
            return None, "MISSING_FILE", warnings

    if no_api:
        return None, "DISABLED_NO_API", warnings

    urls_to_try = [api_url] if api_url else DEFAULT_SNAPSHOT_URLS
    for url in urls_to_try:
        try:
            req = urllib.request.Request(
                url,
                headers={"User-Agent": "Caslav-Report-Builder/1.0", "Accept": "application/json"},
            )
            with urllib.request.urlopen(req, timeout=timeout_s) as resp:
                if resp.status == 200:
                    raw_bytes = resp.read()
                    data = json.loads(raw_bytes.decode("utf-8"))
                    return data, f"LOCAL_API: {url}", warnings
        except urllib.error.URLError as e:
            # Expected when daemon is stopped or offline
            continue
        except Exception as e:
            warnings.append(f"Error querying {url}: {e}")
            continue

    warnings.append("Local API snapshot unavailable on default ports (80/8080).")
    return None, "UNAVAILABLE", warnings


def calculate_timeframe_metrics(
    name: str,
    label: str,
    trades: List[ClosedTradeRecord],
    start_ts: Optional[int],
    end_ts: Optional[int],
    now_ts: int,
    tz: zoneinfo.ZoneInfo,
    is_available: bool = True,
    unavailable_reason: Optional[str] = None,
) -> TimeframeMetrics:
    """Calculates all trading and PnL metrics for a given filtered set of trades."""
    start_iso = datetime.fromtimestamp(start_ts, tz=tz).isoformat() if start_ts is not None else None
    end_iso = datetime.fromtimestamp(end_ts, tz=tz).isoformat() if end_ts is not None else None

    metrics = TimeframeMetrics(
        name=name,
        label=label,
        period_start_ts=start_ts,
        period_end_ts=end_ts,
        period_start_iso=start_iso,
        period_end_iso=end_iso,
        is_available=is_available,
        unavailable_reason=unavailable_reason,
    )

    if not is_available:
        return metrics

    # Filter trades within the window
    window_trades = [
        t for t in trades
        if (start_ts is None or t.ts >= start_ts) and (end_ts is None or t.ts <= end_ts)
    ]

    metrics.trade_count = len(window_trades)
    if not window_trades:
        return metrics

    metrics.pnl_sats = sum(t.pnl_sats for t in window_trades)
    metrics.pnl_usd_reconstructed = sum(t.usd_pnl_reconstructed for t in window_trades)
    metrics.fee_sats = sum(t.fee_sats for t in window_trades)
    metrics.fee_usd_reconstructed = sum(t.fee_usd_reconstructed for t in window_trades)

    wins = [t for t in window_trades if t.pnl_sats > 0.0]
    losses = [t for t in window_trades if t.pnl_sats < 0.0]
    zeros = [t for t in window_trades if t.pnl_sats == 0.0]

    metrics.win_count = len(wins)
    metrics.loss_count = len(losses)
    metrics.zero_count = len(zeros)

    decisive_trades = metrics.win_count + metrics.loss_count
    metrics.win_rate_closed_pct = (metrics.win_count / decisive_trades * 100.0) if decisive_trades > 0 else 0.0
    metrics.win_rate_total_pct = (metrics.win_count / metrics.trade_count * 100.0) if metrics.trade_count > 0 else 0.0

    metrics.gross_profit_sats = sum(t.pnl_sats for t in wins)
    metrics.gross_loss_sats = abs(sum(t.pnl_sats for t in losses))

    if metrics.gross_loss_sats > 0:
        metrics.profit_factor = metrics.gross_profit_sats / metrics.gross_loss_sats
    elif metrics.gross_profit_sats > 0:
        metrics.profit_factor = float("inf")
    else:
        metrics.profit_factor = 0.0

    metrics.avg_win_sats = (metrics.gross_profit_sats / metrics.win_count) if metrics.win_count > 0 else 0.0
    metrics.avg_loss_sats = (metrics.gross_loss_sats / metrics.loss_count) if metrics.loss_count > 0 else 0.0

    if metrics.avg_loss_sats > 0:
        metrics.payoff_ratio = metrics.avg_win_sats / metrics.avg_loss_sats
    else:
        metrics.payoff_ratio = None

    metrics.first_trade_ts = window_trades[0].ts
    metrics.first_trade_iso = datetime.fromtimestamp(window_trades[0].ts, tz=tz).isoformat()
    metrics.last_trade_ts = window_trades[-1].ts
    metrics.last_trade_iso = datetime.fromtimestamp(window_trades[-1].ts, tz=tz).isoformat()

    metrics.inactivity_gap_seconds = max(0, now_ts - metrics.last_trade_ts)

    # Calculate max gap between consecutive trades in window
    if len(window_trades) > 1:
        gaps = [window_trades[i].ts - window_trades[i - 1].ts for i in range(1, len(window_trades))]
        metrics.max_inter_trade_gap_seconds = max(gaps) if gaps else 0
    else:
        metrics.max_inter_trade_gap_seconds = 0

    metrics.unverifiable_orders_count = sum(1 for t in window_trades if t.is_unverifiable_order)

    return metrics


def build_equity_state(
    snapshot: Optional[Dict[str, Any]],
    snapshot_source: str,
    trades: List[ClosedTradeRecord],
) -> EquityState:
    """Builds equity state from snapshot, with fallback to latest ledger price if available."""
    last_trade_price = trades[-1].fill_price if trades else 0.0

    if not snapshot:
        return EquityState(
            source=snapshot_source,
            is_available=False,
            last_close_btc_usd=last_trade_price,
            last_close_source="LEDGER_LAST_FILL" if last_trade_price > 0 else "NONE",
            system_mode="UNKNOWN (API_OFFLINE)",
        )

    btc_bal = float(snapshot.get("btc_balance", 0.0))
    locked_btc = float(snapshot.get("locked_btc_reserve", 0.0))
    usd_bal = float(snapshot.get("usd_balance", 0.0))
    btc_price = float(snapshot.get("btc_price", 0.0))
    mode = str(snapshot.get("system_mode", "Unknown"))
    uptime = snapshot.get("uptime_seconds")
    uptime_val = int(uptime) if uptime is not None else None
    cons_losses = int(snapshot.get("consecutive_losses", 0))

    if btc_price <= 0.0 and last_trade_price > 0.0:
        btc_price = last_trade_price
        price_src = "LEDGER_FALLBACK"
    else:
        price_src = "API_SNAPSHOT"

    total_btc = btc_bal + locked_btc + (usd_bal / btc_price if btc_price > 0 else 0.0)
    total_usd = (btc_bal + locked_btc) * btc_price + usd_bal
    total_sats = total_btc * SATS_PER_BTC

    api_daily_pnl = snapshot.get("daily_pnl")
    api_total_pnl = snapshot.get("total_pnl")

    return EquityState(
        source=snapshot_source,
        is_available=True,
        btc_balance=btc_bal,
        locked_btc_reserve=locked_btc,
        total_btc=total_btc,
        usd_balance=usd_bal,
        last_close_btc_usd=btc_price,
        last_close_source=price_src,
        total_equity_usd=total_usd,
        total_equity_sats=total_sats,
        system_mode=mode,
        uptime_seconds=uptime_val,
        consecutive_losses=cons_losses,
        api_daily_pnl_usd=float(api_daily_pnl) if api_daily_pnl is not None else None,
        api_total_pnl_usd=float(api_total_pnl) if api_total_pnl is not None else None,
    )


def generate_legacy_report_data(
    ledger_path: str = DEFAULT_LEDGER_PATH,
    snapshot_file: Optional[str] = None,
    api_url: Optional[str] = None,
    no_api: bool = False,
    now_arg: Optional[str] = None,
    timezone_name: str = DEFAULT_TIMEZONE,
) -> ReportData:
    """
    Main reusable reporting engine function.
    Reads ledger, fetches snapshot, calculates timeframes, generates provenance and audit warnings.
    """
    tz = zoneinfo.ZoneInfo(timezone_name)
    now_dt = parse_timestamp_or_now(now_arg, tz)
    now_ts = int(now_dt.timestamp())
    now_iso = now_dt.isoformat()

    # Load ledger
    trades, integrity = load_ledger_data(ledger_path)

    # Fetch snapshot
    snapshot, snapshot_source, snap_warnings = fetch_snapshot(
        snapshot_file=snapshot_file,
        api_url=api_url,
        no_api=no_api,
    )

    # Equity state
    equity = build_equity_state(snapshot, snapshot_source, trades)

    # 1. Calendar Day timeframe (survives restart, uses local midnight Europe/Prague accounting for DST)
    local_today_midnight = now_dt.replace(hour=0, minute=0, second=0, microsecond=0)
    day_start_ts = int(local_today_midnight.timestamp())
    day_metrics = calculate_timeframe_metrics(
        name="calendar_day",
        label="Dnešní kalendářní den (od půlnoci Europe/Prague)",
        trades=trades,
        start_ts=day_start_ts,
        end_ts=now_ts,
        now_ts=now_ts,
        tz=tz,
    )

    # 2. Since Restart timeframe
    restart_available = False
    restart_start_ts = None
    restart_reason = None

    if equity.uptime_seconds is not None and equity.uptime_seconds >= 0:
        restart_start_ts = max(0, now_ts - equity.uptime_seconds)
        restart_available = True
    else:
        restart_reason = "API snapshot uptime_seconds missing or unavailable"

    restart_metrics = calculate_timeframe_metrics(
        name="since_restart",
        label="Od posledního restartu runtime jádra",
        trades=trades,
        start_ts=restart_start_ts,
        end_ts=now_ts,
        now_ts=now_ts,
        tz=tz,
        is_available=restart_available,
        unavailable_reason=restart_reason,
    )

    # 3. Ledger Lifetime timeframe
    first_ts = trades[0].ts if trades else None
    lifetime_metrics = calculate_timeframe_metrics(
        name="ledger_lifetime",
        label="Celá historie ledgeru (lifetime)",
        trades=trades,
        start_ts=first_ts,
        end_ts=now_ts,
        now_ts=now_ts,
        tz=tz,
    )

    # All warnings
    warnings: List[str] = []
    warnings.extend(snap_warnings)
    if integrity.malformed_segments_count > 0:
        warnings.append(f"Zaznamenáno {integrity.malformed_segments_count} poškozených/nevalidních segmentů v ledgeru.")
    if integrity.unverifiable_orders_count > 0:
        warnings.append(f"Detekováno {integrity.unverifiable_orders_count} obchodů s neověřitelným order_id (např. order_id=0).")
    if not equity.is_available:
        warnings.append("Lokální API snapshot je nedostupný — balance a stav jádra jsou neověřené/neznámé.")

    # Data provenance (§4 / §9 PŮVOD DAT)
    provenance: Dict[str, List[str]] = {
        "measured": [
            f"Neověřené legacy souhrny (ClosedTrade) z logu {ledger_path} [LEGACY_UNVERIFIED]",
            f"Zůstatky účtu a stav jádra z API snapshotu ({snapshot_source}) [LOCAL_API]" if equity.is_available else "Zůstatky účtu z API snapshotu: NEDOSTUPNÉ [UNVERIFIED]",
        ],
        "derived": [
            "USD PnL: Sumováno z fillů vzorcem (pnl_sats * fill_price / 1e8) dle sémantiky zaznamenané v ledgeru [DERIVED_RECONSTRUCTED]",
            "Celková equity v sats a USD: Dopočtena ze zůstatků a aktuální ceny BTC [DERIVED]",
            "Win rate legacy odhadů: neprokazuje skutečné realizované výsledky [LEGACY_UNVERIFIED]",
            "Kalendářní den: Timezone-aware lokální půlnoc (Europe/Prague, DST-safe) [DERIVED]",
        ],
        "unverified": [],
    }

    if integrity.unverifiable_orders_count > 0:
        provenance["unverified"].append(
            f"{integrity.unverifiable_orders_count} historických obchodů má order_id <= 0 (přítomné v ranné fázi před fixem order ID routingu) [UNVERIFIED_ORDER_ID]"
        )
    if not equity.is_available:
        provenance["unverified"].append("Snapshot API status a real-time exposure [UNVERIFIED: API offline]")

    timeframes = {
        "calendar_day": day_metrics,
        "since_restart": restart_metrics,
        "ledger_lifetime": lifetime_metrics,
    }

    return ReportData(
        generated_at_ts=now_ts,
        generated_at_iso=now_iso,
        timezone_name=timezone_name,
        ledger_path=ledger_path,
        snapshot_source=snapshot_source,
        equity=equity,
        integrity=integrity,
        timeframes=timeframes,
        warnings=warnings,
        provenance=provenance,
    )


def format_legacy_text_report(data: ReportData) -> str:
    """Formats report into ČÁSLAV Institutional Standard text layout."""
    lines: List[str] = []
    e = data.equity
    tf_day = data.timeframes["calendar_day"]
    tf_restart = data.timeframes["since_restart"]
    tf_life = data.timeframes["ledger_lifetime"]

    dt_str = datetime.fromtimestamp(data.generated_at_ts, tz=zoneinfo.ZoneInfo(data.timezone_name)).strftime("%d.%m.%Y %H:%M:%S %Z")

    lines.append("👑 ČÁSLAV :: PIRANA INSTITUTIONAL PERFORMANCE REPORT")
    lines.append("═" * 65)
    lines.append(f"📅 Čas reportu: {dt_str} ({data.timezone_name})")
    lines.append(f"📁 Ledger: {data.ledger_path}")
    lines.append(f"🔌 Snapshot: {data.snapshot_source}")
    lines.append("")

    # §1b & §9: Sats as primary metric
    lines.append("🪙 KAPITÁLOVÁ ROZVAHA (BITCOIN STANDARD) [SOURCE: API & LEDGER]")
    lines.append("─" * 65)
    if e.is_available:
        lines.append(f"• Celková equity v sats:   {e.total_equity_sats:,.0f} sats ({e.total_btc:.8f} BTC) [SOURCE: DERIVED]")
        lines.append(f"• Celková equity v USD:    ${e.total_equity_usd:,.2f} USD [SOURCE: DERIVED]")
        lines.append(f"• Zůstatek BTC (volný):    {e.btc_balance:.8f} BTC ({e.btc_balance * SATS_PER_BTC:,.0f} sats) [SOURCE: LOCAL_API]")
        lines.append(f"• Trezor (locked reserve): {e.locked_btc_reserve:.8f} BTC ({e.locked_btc_reserve * SATS_PER_BTC:,.0f} sats) [SOURCE: LOCAL_API]")
        lines.append(f"• Zůstatek USD:            ${e.usd_balance:,.2f} USD [SOURCE: LOCAL_API]")
        lines.append(f"• Poslední cena BTC/USD:   ${e.last_close_btc_usd:,.2f} [{e.last_close_source}]")
        lines.append(f"• Režim jádra / Uptime:    {e.system_mode} | Uptime: {e.uptime_seconds or 0} s")
    else:
        lines.append("⚠️ [API OFFLINE] Údaje o zůstatcích nejsou k dispozici z živého API.")
        if e.last_close_btc_usd > 0:
            lines.append(f"• Poslední cena BTC (ledger): ${e.last_close_btc_usd:,.2f} [SOURCE: LEDGER_LAST_FILL]")
    lines.append("")

    # Timeframe performance tables
    lines.append("📊 REALIZOVANÉ VÝSLEDKY VE TŘECH ČASOVÝCH OKNECH [SOURCE: LEDGER]")
    lines.append("─" * 65)

    def render_tf_block(tf: TimeframeMetrics):
        sign_sats = "+" if tf.pnl_sats >= 0 else ""
        sign_usd = "+" if tf.pnl_usd_reconstructed >= 0 else ""
        lines.append(f"▶ {tf.label.upper()} [{tf.name}]")
        if not tf.is_available:
            lines.append(f"  ⚠️ NEDOSTUPNÉ: {tf.unavailable_reason}")
            lines.append("")
            return

        lines.append(f"  • Období:            {tf.period_start_iso or 'start'} ➔ {tf.period_end_iso or 'now'}")
        lines.append(f"  • Realizovaný PnL:   {sign_sats}{tf.pnl_sats:,.1f} sats ({sign_usd}${tf.pnl_usd_reconstructed:,.4f} USD*)")
        lines.append(f"  • Poplatky:          {tf.fee_sats:,.1f} sats (${tf.fee_usd_reconstructed:,.4f} USD*)")
        lines.append(f"  • Počet obchodů:     {tf.trade_count} celkem (W: {tf.win_count} | L: {tf.loss_count} | BE: {tf.zero_count})")
        lines.append(f"  • Win Rate:          {tf.win_rate_closed_pct:.1f}% (z rozhodnutých) | {tf.win_rate_total_pct:.1f}% (ze všech)")
        pf_str = f"{tf.profit_factor:.2f}" if tf.profit_factor is not None and not math.isinf(tf.profit_factor) else ("∞" if tf.profit_factor and math.isinf(tf.profit_factor) else "N/A")
        payoff_str = f"{tf.payoff_ratio:.2f}" if tf.payoff_ratio is not None else "N/A"
        lines.append(f"  • Profit Factor:     {pf_str} | Payoff ratio: {payoff_str}")
        if tf.first_trade_iso and tf.last_trade_iso:
            lines.append(f"  • První / Poslední:  {tf.first_trade_iso} ➔ {tf.last_trade_iso}")
        if tf.inactivity_gap_seconds is not None:
            lines.append(f"  • Neaktivita:        {tf.inactivity_gap_seconds} s od posledního fillu | Max mezera: {tf.max_inter_trade_gap_seconds or 0} s")
        if tf.unverifiable_orders_count > 0:
            lines.append(f"  • Neověřené ordery:  ⚠️ {tf.unverifiable_orders_count} obchodů v tomto okně má order_id=0")
        lines.append("")

    render_tf_block(tf_day)
    render_tf_block(tf_restart)
    render_tf_block(tf_life)

    lines.append("* Poznámka k USD: Částka v USD je rekonstruována z historických fillů (pnl_sats * fill_price / 1e8).")
    lines.append("")

    # Realized vs Equity change note
    lines.append("🔍 REALIZOVANÝ PNL vs. ZMĚNA CELKOVÉ EQUITY")
    lines.append("─" * 65)
    lines.append("• Legacy PnL: neověřený historický odhad; skutečný realizovaný zisk není prokázán.")
    lines.append("• Změna equity: Zahrnuje pohyb tržní ceny BTC a stav neuzavřených pozic/volného fiatu.")
    if e.is_available and e.api_daily_pnl_usd is not None:
        lines.append(f"• API snapshot daily_pnl: ${e.api_daily_pnl_usd:,.4f} USD | total_pnl: ${e.api_total_pnl_usd or 0.0:,.4f} USD")
    lines.append("")

    # Data integrity and audit
    lines.append("🛡 INTEGRITA DAT A AUDIT LEDGERU")
    lines.append("─" * 65)
    lines.append(f"• Neověřené legacy odhady:   {data.integrity.legacy_estimate_records}")
    lines.append(f"• Vyloučené Shadow obchody:   {data.integrity.shadow_trades_excluded}")
    lines.append(f"• Vyloučené Rebalance obchody:{data.integrity.rebalance_trades_excluded}")
    lines.append(f"• Poškozené/nevalidní bloky:  {data.integrity.malformed_segments_count}")
    lines.append(f"• Neověřitelné order ID:      {data.integrity.unverifiable_orders_count}")
    if data.integrity.malformed_errors:
        lines.append("  ⚠️ Detaily poškozených segmentů:")
        for err in data.integrity.malformed_errors[:5]:
            lines.append(f"     - {err}")
        if len(data.integrity.malformed_errors) > 5:
            lines.append(f"     ... a dalších {len(data.integrity.malformed_errors) - 5} chyb.")
    lines.append("")

    # §4 / §9 PŮVOD DAT
    lines.append("📜 PŮVOD DAT (DATA PROVENANCE) [ČÁSLAV v5.1 §4 / §9]")
    lines.append("─" * 65)
    lines.append("ZMĚŘENO:")
    for item in data.provenance["measured"]:
        lines.append(f"  [ZMĚŘENO] {item}")
    lines.append("ODVOZENO:")
    for item in data.provenance["derived"]:
        lines.append(f"  [ODVOZENO] {item}")
    if data.provenance["unverified"]:
        lines.append("NEOVĚŘENO / VAROVÁNÍ:")
        for item in data.provenance["unverified"]:
            lines.append(f"  [NEOVĚŘENO] {item}")
    else:
        lines.append("NEOVĚŘENO: Žádné kritické neověřené předpoklady.")
    lines.append("═" * 65)

    return "\n".join(lines)


def format_legacy_telegram_html(data: ReportData) -> str:
    """Formats report into clean Telegram HTML format for bot integration."""
    e = data.equity
    tf_day = data.timeframes["calendar_day"]
    tf_restart = data.timeframes["since_restart"]
    dt_str = datetime.fromtimestamp(data.generated_at_ts, tz=zoneinfo.ZoneInfo(data.timezone_name)).strftime("%d.%m.%Y %H:%M:%S")

    mode_icon = "🟢" if e.system_mode == "Active" else ("🟡" if e.system_mode == "Defensive" else "🔴")
    sign_day_sats = "+" if tf_day.pnl_sats >= 0 else ""
    sign_day_usd = "+" if tf_day.pnl_usd_reconstructed >= 0 else ""
    pnl_day_icon = "🟢" if tf_day.pnl_sats >= 0 else "🔴"

    html = [
        f"👑 <b>ČÁSLAV :: PIRANA INSTITUTIONAL REPORT</b>",
        f"📅 <i>{dt_str} {data.timezone_name}</i>\n",
        f"🦈 <b>Stav jádra:</b> {mode_icon} <code>{e.system_mode}</code>",
        f"• <b>Cena BTC:</b> <code>${e.last_close_btc_usd:,.1f}</code>",
    ]

    if e.is_available:
        html.extend([
            f"• <b>Celková equity:</b> <code>{e.total_equity_sats:,.0f} sats</code> (~${e.total_equity_usd:,.2f})",
            f"• <b>Zůstatek BTC:</b> <code>{e.btc_balance:.6f} BTC</code> (Trezor: <code>{e.locked_btc_reserve:.6f} BTC</code>)",
            f"• <b>Zůstatek USD:</b> <code>${e.usd_balance:,.2f}</code>\n",
        ])
    else:
        html.append("• <b>Equity:</b> ⚠️ <i>API snapshot offline</i>\n")

    html.extend([
        f"📊 <b>Dnešní obchodování (Kalendářní den):</b>",
        f"• Realizovaný PnL: {pnl_day_icon} <b>{sign_day_sats}{tf_day.pnl_sats:,.1f} sats</b> ({sign_day_usd}${tf_day.pnl_usd_reconstructed:,.4f})",
        f"• Obchody: <b>{tf_day.trade_count}</b> (W: <b>{tf_day.win_count}</b> | L: <b>{tf_day.loss_count}</b> | BE: <b>{tf_day.zero_count}</b>)",
        f"• Win Rate: <b>{tf_day.win_rate_closed_pct:.1f}%</b> | Poplatky: <code>{tf_day.fee_sats:,.1f} sats</code>\n",
    ])

    if tf_restart.is_available:
        sign_rest_sats = "+" if tf_restart.pnl_sats >= 0 else ""
        sign_rest_usd = "+" if tf_restart.pnl_usd_reconstructed >= 0 else ""
        html.extend([
            f"🔄 <b>Od restartu ({e.uptime_seconds or 0} s):</b>",
            f"• PnL: <b>{sign_rest_sats}{tf_restart.pnl_sats:,.1f} sats</b> ({sign_rest_usd}${tf_restart.pnl_usd_reconstructed:,.4f}) | Obchody: <b>{tf_restart.trade_count}</b>\n",
        ])

    if data.warnings:
        html.append(f"⚠️ <b>Varování integrity:</b>")
        for w in data.warnings[:3]:
            html.append(f"• <i>{w}</i>")
        html.append("")

    html.append("🛡 <i>Zdroj dat: /var/lib/pirana/trade_ledger.jsonl (Bitcoin Standard)</i>")
    return "\n".join(html)


def format_json_report(data: ReportData) -> str:
    """Serializes complete ReportData dataclass structure to JSON."""
    raw_dict = data if isinstance(data, dict) else asdict(data)
    if not isinstance(data, dict):
        raw_dict["status"] = "legacy_unverified_estimates"
    return json.dumps(raw_dict, indent=2, ensure_ascii=False)


DEFAULT_ACCOUNTING_PATH = "/var/lib/pirana/accounting_snapshot.json"


def canonical_unavailable(reason: str) -> dict:
    period = dict(gross_pnl_usd=None, net_pnl_usd=None, fees_usd=None,
                  closed_count=None, win_count=None, loss_count=None)
    return dict(schema_version=1, scope="account:tBTCUSD", status="incomplete",
                issues=[reason], daily=period.copy(), lifetime=period.copy(), active_period=None)


def validate_active_period(value: Any, now_ms: int):
    verified = _validate_active_period(value, now_ms)
    if verified is not None:
        return verified
    period = value.get("active_period") if isinstance(value, dict) else None
    if not isinstance(period, dict):
        return None
    start = period.get("start_ms")
    if (type(start) is not int or not 0 <= start <= now_ms
            or not isinstance(period.get("id"), str) or not period["id"].strip()):
        return None
    unknown = canonical_unavailable("active period unverified")["daily"]
    return dict(unknown, id=period["id"], start_ms=start, status="incomplete",
                issues=["active period unverified"])


def _validate_active_period(value: Any, now_ms: int):
    """Verify period independently of historical gaps, with shared capture gates."""
    if not isinstance(value, dict):
        return None
    if (type(value.get("schema_version")) is not int or value["schema_version"] != 1
            or value.get("source") != "authenticated_bitfinex_fills"
            or value.get("scope") != "account:tBTCUSD"):
        return None
    ts, sync = value.get("generated_at_ms"), value.get("sync")
    if (type(ts) is not int or not 0 <= now_ms - ts <= 120_000
            or not isinstance(sync, dict) or sync.get("complete") is not True):
        return None
    cursor, period = sync.get("cursor_ms"), value.get("active_period")
    if (type(cursor) is not int or not 0 <= now_ms - cursor <= 120_000
            or not isinstance(period, dict)):
        return None
    tz = zoneinfo.ZoneInfo(DEFAULT_TIMEZONE)
    today = datetime.fromtimestamp(now_ms / 1000, tz).date()
    if (datetime.fromtimestamp(ts / 1000, tz).date() != today
            or datetime.fromtimestamp(cursor / 1000, tz).date() != today
            or type(sync.get("coverage_start_ms")) is not int
            or sync["coverage_start_ms"] != 0):
        return None
    start = period.get("start_ms")
    if (type(start) is not int or not 0 <= start <= min(cursor, ts)
            or not isinstance(period.get("id"), str) or not period["id"].strip()):
        return None
    try:
        datetime.fromtimestamp(start / 1000, zoneinfo.ZoneInfo(DEFAULT_TIMEZONE))
    except (ValueError, OverflowError, OSError):
        return None
    if period.get("status") != "complete" or period.get("issues") != []:
        return None
    for field in ("gross_pnl_usd", "net_pnl_usd", "fees_usd"):
        try:
            if not isinstance(period.get(field), str) or not Decimal(period[field]).is_finite():
                return None
        except (InvalidOperation, ValueError):
            return None
    for field in ("closed_count", "win_count", "loss_count"):
        if type(period.get(field)) is not int or period[field] < 0:
            return None
    if period["win_count"] + period["loss_count"] > period["closed_count"]:
        return None
    return period.copy()


def validate_accounting_projection(value: Any, now_ms: int) -> dict:
    result = _validate_historical_projection(value, now_ms).copy()
    result["active_period"] = validate_active_period(value, now_ms)
    return result


def _validate_historical_projection(value: Any, now_ms: int) -> dict:
    if not isinstance(value, dict):
        return canonical_unavailable("canonical projection is not an object")
    if (type(value.get("schema_version")) is not int or value["schema_version"] != 1
            or value.get("source") != "authenticated_bitfinex_fills"
            or value.get("scope") != "account:tBTCUSD"):
        return canonical_unavailable("invalid schema/source/scope")
    ts = value.get("generated_at_ms")
    if type(ts) is not int or ts > now_ms or now_ms - ts > 120_000:
        return canonical_unavailable("missing, future or stale projection timestamp")
    tz = zoneinfo.ZoneInfo(DEFAULT_TIMEZONE)
    if datetime.fromtimestamp(ts / 1000, tz).date() != datetime.fromtimestamp(now_ms / 1000, tz).date():
        return canonical_unavailable("wrong Prague accounting day")
    if (value.get("status") != "complete" or not isinstance(value.get("sync"), dict)
            or value["sync"].get("complete") is not True or value.get("issues") != []):
        result = canonical_unavailable("accounting incomplete")
        result["upstream_issues"] = value.get("issues")
        return result
    cursor = value["sync"].get("cursor_ms")
    if type(cursor) is not int or cursor > now_ms or now_ms - cursor > 120_000:
        return canonical_unavailable("missing, future or stale sync cursor")
    if datetime.fromtimestamp(cursor / 1000, tz).date() != datetime.fromtimestamp(now_ms / 1000, tz).date():
        return canonical_unavailable("wrong Prague sync day")
    for name in ("daily", "lifetime"):
        period = value.get(name)
        if not isinstance(period, dict):
            return canonical_unavailable("missing accounting period")
        for field in ("gross_pnl_usd", "net_pnl_usd", "fees_usd"):
            try:
                if not isinstance(period.get(field), str) or not Decimal(period[field]).is_finite():
                    raise ValueError()
            except (InvalidOperation, ValueError):
                return canonical_unavailable("invalid monetary field")
        for field in ("closed_count", "win_count", "loss_count"):
            if type(period.get(field)) is not int or period[field] < 0:
                return canonical_unavailable("invalid count")
        if period["win_count"] + period["loss_count"] > period["closed_count"]:
            return canonical_unavailable("inconsistent counts")
    return value


def generate_report_data(ledger_path=DEFAULT_LEDGER_PATH, snapshot_file=None,
                         api_url=None, no_api=False, now_arg=None,
                         timezone_name=DEFAULT_TIMEZONE, legacy=False,
                         include_runtime=False, runtime_snapshot_file=None):
    if legacy:
        return generate_legacy_report_data(ledger_path, snapshot_file, api_url,
                                           no_api, now_arg, timezone_name)
    now = parse_timestamp_or_now(now_arg, zoneinfo.ZoneInfo(DEFAULT_TIMEZONE))
    path = snapshot_file or os.environ.get("PIRANA_ACCOUNTING_SNAPSHOT_PATH", DEFAULT_ACCOUNTING_PATH)
    source_metadata = {}
    try:
        with open(path, encoding="utf-8") as stream:
            value = json.load(stream)
        accounting = validate_accounting_projection(value, int(now.timestamp() * 1000))
        if (isinstance(value, dict) and type(value.get("schema_version")) is int
                and value["schema_version"] == 1 and value.get("source") == "authenticated_bitfinex_fills"
                and value.get("scope") == "account:tBTCUSD"):
            sync = value.get("sync")
            for key, timestamp in (("generated_at_ms", value.get("generated_at_ms")),
                                   ("cursor_ms", sync.get("cursor_ms") if isinstance(sync, dict) else None)):
                if type(timestamp) is int and 0 <= timestamp <= int(now.timestamp() * 1000):
                    source_metadata[key] = timestamp

    except (OSError, ValueError, TypeError, OverflowError):
        accounting = canonical_unavailable("missing or corrupt canonical projection")
    runtime, runtime_source = None, "NEOVĚŘENO"
    if include_runtime:
        runtime, runtime_source, _ = fetch_snapshot(
            snapshot_file=runtime_snapshot_file, no_api=no_api)
        if not isinstance(runtime, dict):
            runtime = None
    # Legacy summaries are diagnostic counts only; never add their estimates to PnL.
    _, integrity = load_ledger_data(ledger_path)
    return dict(accounting=accounting, snapshot_source=path, report_generated_at_ms=int(now.timestamp() * 1000),
                legacy_unverified_diagnostics=asdict(integrity), runtime=runtime, runtime_source=runtime_source,
                source_metadata=source_metadata)


def _runtime_balance(value, multiplier=1):
    if isinstance(value, bool) or not isinstance(value, (str, int, float)):
        return "NEOVĚŘENO"
    try:
        number = Decimal(str(value))
        if not number.is_finite() or number < 0 or number.adjusted() > 18:
            return "NEOVĚŘENO"
        return format(number * multiplier, ".0f" if multiplier != 1 else ".2f")
    except (InvalidOperation, ValueError):
        return "NEOVĚŘENO"


def _report_timestamp(value):
    if type(value) is not int:
        return "NEOVĚŘENO"
    try:
        return datetime.fromtimestamp(value / 1000, zoneinfo.ZoneInfo(DEFAULT_TIMEZONE)).isoformat(sep=" ")
    except (ValueError, OverflowError, OSError):
        return "NEOVĚŘENO"


def format_text_report(data) -> str:
    if not isinstance(data, dict):
        return "LEGACY — NEOVĚŘENÉ ODHADY, NIKOLI POTVRZENÝ ZISK\n" + format_legacy_text_report(data)
    a = data["accounting"]
    runtime = data.get("runtime") or {}
    sats = _runtime_balance(runtime.get("btc_balance"), 100_000_000)
    vault = _runtime_balance(runtime.get("locked_btc_reserve"), 100_000_000)
    usd = _runtime_balance(runtime.get("usd_balance"))
    lines = [f"₿ BTC podle bota: {sats} sats | chráněno: {vault} sats",
             f"USD podle bota: {usd} USD | Δsats: NEOVĚŘENO",
             "Rozsah: všechny obchody účtu Bitfinex BTC/USD (nikoli pouze Pirana)",
             "Režim podle bota: " + str(runtime.get("system_mode", "NEOVĚŘENO"))[:40]
             + "; Active samo nepotvrzuje uskutečněný obchod."]
    if runtime.get("execution_block_reason"):
        lines.append("Blokace obchodování: " + str(runtime["execution_block_reason"])[:180])
    periods = []
    active = a.get("active_period")
    if active:
        periods.append((active, "Nové období od " + _report_timestamp(active.get("start_ms")) + " Europe/Prague"))
    else:
        lines.append("Nové období: NEOVĚŘENO")
    periods.extend(((a["daily"], "Dnes Europe/Prague"), (a["lifetime"], "Celá pokrytá historie")))
    for period, label in periods:
        net, fees, count = period.get("net_pnl_usd"), period.get("fees_usd"), period.get("closed_count")
        if net is None or fees is None:
            lines.append(label + ": NEOVĚŘENO")
            continue
        count_text = str(count) if type(count) is int and 0 <= count <= 10**12 else "NEOVĚŘENO"
        lines.append(label + f": čistý PnL {str(net)[:60]} USD | poplatky {str(fees)[:60]} USD | prodejní plnění {count_text}")
    if active and active.get("closed_count") == 0:
        lines.append("Nula prodejních plnění nevylučuje BUY filly.")
    issues = a.get("upstream_issues", [])
    missing = set()
    if isinstance(issues, list):
        for issue in issues:
            if isinstance(issue, str) and issue.startswith("unmatched_sell_cost:"):
                trade_id = issue.partition(":")[2]
                if trade_id.isascii() and trade_id.isdecimal() and len(trade_id) <= 20:
                    missing.add(trade_id)
    if missing:
        lines.append(f"KONTROLA: U {len(missing)} prodejních plnění chybí pořizovací cena; doplnit nákupní historii.")
    elif a["status"] != "complete" or not active or active.get("status") != "complete":
        lines.append("KONTROLA: prověřit dostupnost a úplnost účetního snímku.")
    metadata = data.get("source_metadata") or {}
    generated = metadata.get("generated_at_ms", a.get("generated_at_ms"))
    cursor = metadata.get("cursor_ms", a.get("sync", {}).get("cursor_ms"))
    lines.extend(("PŮVOD DAT",
        "PnL: autentizované filly → FIFO; " + str(data["snapshot_source"])[:160],
        "Snímek: " + _report_timestamp(generated) + " | sync do: " + _report_timestamp(cursor)
        + "; časy nepotvrzují úplnost PnL.",
        "Zůstatky: " + str(data.get("runtime_source", "NEOVĚŘENO"))[:120]
        + "; čas ověření peněženky burzou není doložen. Δsats ani legacy odhady nejsou PnL."))
    return "\n".join(lines)


def format_telegram_html(data) -> str:
    # Formatting only; this module does not send messages.
    if not isinstance(data, dict):
        return "<b>LEGACY — NEOVĚŘENÉ ODHADY</b>\n" + format_legacy_telegram_html(data)
    return html.escape(format_text_report(data))


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Čáslav :: Pirana Institutional Performance & PnL Reporter",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--ledger",
        dest="ledger_path",
        default=DEFAULT_LEDGER_PATH,
        help="Path to trade_ledger.jsonl file",
    )
    parser.add_argument(
        "--snapshot-file",
        dest="snapshot_file",
        default=None,
        help="Path to JSON file containing system snapshot",
    )
    parser.add_argument(
        "--api-url",
        dest="api_url",
        default=None,
        help="Explicit URL for snapshot API (e.g. http://127.0.0.1:80/api/snapshot)",
    )
    parser.add_argument(
        "--no-api",
        action="store_true",
        help="Disable localhost API polling completely",
    )
    parser.add_argument(
        "--now",
        dest="now_arg",
        default=None,
        help="Reference current time (ISO string or unix timestamp) for reproducible tests",
    )
    parser.add_argument(
        "--timezone",
        dest="timezone_name",
        default=DEFAULT_TIMEZONE,
        help="Timezone for calendar day boundaries and report display",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Output structured JSON report to stdout",
    )
    parser.add_argument(
        "--html",
        action="store_true",
        help="Output formatted HTML report (for Telegram bots)",
    )
    parser.add_argument(
        "--output",
        "-o",
        dest="output_file",
        default=None,
        help="Optional file path to save the generated report",
    )

    parser.add_argument("--legacy", action="store_true", help="Explicit unverified legacy estimate report")
    args = parser.parse_args()

    try:
        report_data = generate_report_data(
            legacy=args.legacy,
            include_runtime=True,
            ledger_path=args.ledger_path,
            snapshot_file=args.snapshot_file,
            api_url=args.api_url,
            no_api=args.no_api,
            now_arg=args.now_arg,
            timezone_name=args.timezone_name,
        )
    except Exception as e:
        print(f"CRITICAL ERROR generating report: {e}", file=sys.stderr)
        return 1

    if args.json:
        output_str = format_json_report(report_data)
    elif args.html:
        output_str = format_telegram_html(report_data)
    else:
        output_str = format_text_report(report_data)

    print(output_str)

    if args.output_file:
        try:
            with open(args.output_file, "w", encoding="utf-8") as f:
                f.write(output_str)
        except Exception as e:
            print(f"ERROR saving report to {args.output_file}: {e}", file=sys.stderr)
            return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
