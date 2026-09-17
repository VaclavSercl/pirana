#!/usr/bin/env python3
"""
Unit and integration tests for scripts/pirana_report.py
Tests concatenated JSON recovery, shadow/rebalance exclusion, midnight/DST,
multiple restarts, missing fields, offline API fallback, and formatting.
"""

from datetime import datetime, timezone
import json
import os
import subprocess
import sys
import zoneinfo
import pytest

from scripts.pirana_report import (
    ClosedTradeRecord,
    DataIntegritySummary,
    decode_concatenated_json,
    format_json_report,
    format_telegram_html,
    format_text_report,
    generate_legacy_report_data as generate_report_data,
    load_ledger_data,
    validate_and_classify_trade,
)


@pytest.fixture
def tz_prague():
    return zoneinfo.ZoneInfo("Europe/Prague")


@pytest.mark.parametrize('field', ['pnl_sats', 'fill_price', 'qty', 'fee_sats', 'vpin_at_close', 'ts'])
@pytest.mark.parametrize('value', [float('nan'), float('inf'), float('-inf')])
def test_nonfinite_ledger_values_rejected(field, value):
    record = dict(ts=1700000000, pnl_sats=1, fill_price=80000, qty=.001)
    record[field] = value
    trade, category, error = validate_and_classify_trade(record)
    assert trade is None and category == 'invalid' and error


def test_concatenated_json_parsing(tmp_path):
    """Test decoding multiple JSON records concatenated on a single line without newlines."""
    content = (
        '{"pnl_sats": 25.5, "ts": 1700000000, "fill_price": 60000.0, "qty": 0.001, "side": "Buy", "cid": "live_1", "order_id": 101, "trade_id": 201}'
        '{"pnl_sats": -10.0, "ts": 1700000060, "fill_price": 60100.0, "qty": 0.001, "side": "Sell", "cid": "live_2", "order_id": 102, "trade_id": 202}'
    )
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    trades, integrity = load_ledger_data(str(ledger_file))

    assert len(trades) == 2
    assert integrity.legacy_estimate_records == 2
    assert integrity.malformed_segments_count == 0
    assert trades[0].cid == "live_1"
    assert trades[0].pnl_sats == 25.5
    assert trades[1].cid == "live_2"
    assert trades[1].pnl_sats == -10.0


def test_shadow_and_rebalance_filtering(tmp_path):
    """Test that shadow_ and rebalance records are excluded from metrics and tracked in integrity counts."""
    content = "\n".join([
        '{"pnl_sats": 50.0, "ts": 1700000100, "fill_price": 60000.0, "qty": 0.001, "side": "Buy", "cid": "live_order_1", "order_id": 11, "trade_id": 21}',
        '{"pnl_sats": 999.0, "ts": 1700000110, "fill_price": 60000.0, "qty": 0.001, "side": "Buy", "cid": "shadow_strategy_ab", "order_id": 12, "trade_id": 22}',
        '{"pnl_sats": -500.0, "ts": 1700000120, "fill_price": 60000.0, "qty": 0.001, "side": "Buy", "cid": "rebalance_reserve_lot", "order_id": 13, "trade_id": 23}',
        '{"pnl_sats": -20.0, "ts": 1700000130, "fill_price": 60000.0, "qty": 0.001, "side": "Sell", "cid": "live_order_2", "order_id": 14, "trade_id": 24}',
    ])
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    trades, integrity = load_ledger_data(str(ledger_file))

    assert len(trades) == 2
    assert integrity.legacy_estimate_records == 2
    assert integrity.shadow_trades_excluded == 1
    assert integrity.rebalance_trades_excluded == 1
    assert trades[0].cid == "live_order_1"
    assert trades[1].cid == "live_order_2"


def test_shadow_mixed_on_same_concatenated_line(tmp_path):
    """Test concatenated JSON where live trade, shadow trade, and rebalance trade are on the exact same line."""
    line = (
        '{"pnl_sats": 10.0, "ts": 1700000100, "fill_price": 65000.0, "qty": 0.001, "side": "Buy", "cid": "live_a", "order_id": 10, "trade_id": 20}'
        '{"pnl_sats": 99.0, "ts": 1700000101, "fill_price": 65000.0, "qty": 0.001, "side": "Buy", "cid": "shadow_b", "order_id": 11, "trade_id": 21}'
        '{"pnl_sats": -5.0, "ts": 1700000102, "fill_price": 65000.0, "qty": 0.001, "side": "Sell", "cid": "live_c", "order_id": 12, "trade_id": 22}'
    )
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(line)

    trades, integrity = load_ledger_data(str(ledger_file))

    assert len(trades) == 2
    assert integrity.shadow_trades_excluded == 1
    assert trades[0].cid == "live_a"
    assert trades[1].cid == "live_c"


def test_invalid_and_corrupt_segments_never_hidden(tmp_path):
    """Test that malformed JSON chunks and corrupt bytes are flagged and not silently hidden."""
    content = (
        'CORRUPTED_PREFIX_123\n'
        '{"pnl_sats": 30.0, "ts": 1700000200, "fill_price": 70000.0, "qty": 0.001, "side": "Buy", "cid": "valid_1", "order_id": 50, "trade_id": 60}\n'
        '{"pnl_sats": BROKEN_JSON, "ts": 1700000210}\n'
        '{"pnl_sats": -15.0, "ts": 1700000220, "fill_price": 70000.0, "qty": 0.001, "side": "Sell", "cid": "valid_2", "order_id": 51, "trade_id": 61}\n'
        'TRAILING_GARBAGE'
    )
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    trades, integrity = load_ledger_data(str(ledger_file))

    assert len(trades) == 2
    assert integrity.malformed_segments_count >= 2
    assert len(integrity.malformed_errors) >= 2


def test_missing_fields_and_invalid_schema(tmp_path):
    """Test records missing essential fields like pnl_sats or negative fill_price."""
    content = "\n".join([
        '{"ts": 1700000250, "fill_price": 70000.0}',  # missing pnl_sats
        '{"pnl_sats": 10.0, "fill_price": 70000.0}',  # missing ts
        '{"pnl_sats": 10.0, "ts": 1700000260, "fill_price": -500.0}',  # negative fill_price
        '{"pnl_sats": "not_a_number", "ts": 1700000270, "fill_price": 70000.0}',  # bad type
        '{"pnl_sats": 20.0, "ts": 1700000280, "fill_price": 70000.0, "cid": "ok_trade", "order_id": 1, "trade_id": 1}',  # valid
    ])
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    trades, integrity = load_ledger_data(str(ledger_file))

    assert len(trades) == 1
    assert trades[0].cid == "ok_trade"
    assert integrity.malformed_segments_count == 4


def test_missing_and_unverifiable_order_ids(tmp_path):
    """Test flagging of order_id=0, negative order_id, or missing order_id."""
    content = "\n".join([
        '{"pnl_sats": 10.0, "ts": 1700000300, "fill_price": 75000.0, "qty": 0.001, "side": "Buy", "cid": "unverif_1", "order_id": 0, "trade_id": 0}',
        '{"pnl_sats": -5.0, "ts": 1700000310, "fill_price": 75000.0, "qty": 0.001, "side": "Sell", "cid": "unverif_2", "order_id": -1, "trade_id": 1}',
        '{"pnl_sats": 20.0, "ts": 1700000320, "fill_price": 75000.0, "qty": 0.001, "side": "Buy", "cid": "verif_3", "order_id": 999, "trade_id": 888}',
    ])
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    trades, integrity = load_ledger_data(str(ledger_file))

    assert len(trades) == 3
    assert integrity.unverifiable_orders_count == 3
    assert trades[0].is_unverifiable_order is True
    assert trades[1].is_unverifiable_order is True
    assert trades[2].is_unverifiable_order is True


def test_usd_pnl_reconstructed_per_record(tmp_path):
    """Test that USD PnL is computed from each record's fill_price, not the final BTC price."""
    # Trade 1: +100,000,000 sats (+1 BTC) at fill_price 40,000 -> USD PnL = +$40,000
    # Trade 2: -50,000,000 sats (-0.5 BTC) at fill_price 80,000 -> USD PnL = -$40,000
    # Combined sats PnL = +50,000,000 sats (+0.5 BTC)
    # Combined USD PnL = $40,000 - $40,000 = $0.00 (whereas 0.5 BTC * $80,000 would be $40,000)
    content = "\n".join([
        '{"pnl_sats": 100000000.0, "ts": 1700000400, "fill_price": 40000.0, "qty": 1.0, "side": "Sell", "cid": "t1", "order_id": 1, "trade_id": 1}',
        '{"pnl_sats": -50000000.0, "ts": 1700000450, "fill_price": 80000.0, "qty": 1.0, "side": "Buy", "cid": "t2", "order_id": 2, "trade_id": 2}',
    ])
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    report = generate_report_data(ledger_path=str(ledger_file), no_api=True, now_arg="1700000500")
    lifetime = report.timeframes["ledger_lifetime"]

    assert lifetime.pnl_sats == 50000000.0
    assert pytest.approx(lifetime.pnl_usd_reconstructed, abs=1e-5) == 0.0


def test_timezone_aware_midnight_and_dst(tmp_path, tz_prague):
    """Test timezone-aware calendar day boundaries in Europe/Prague including DST transitions."""
    # Date: 2026-03-29 (DST spring forward in Europe/Prague: UTC+1 to UTC+2 at 02:00 -> 03:00)
    # Midnight Prague on 2026-03-29 is 2026-03-29 00:00:00 UTC+1 (timestamp 1774738800)
    # Previous day 23:55 Prague: 2026-03-28 23:55:00 UTC+1 (timestamp 1774738500)
    # Today 01:00 Prague: 2026-03-29 01:00:00 UTC+1 (timestamp 1774742400)
    # Today 04:00 Prague: 2026-03-29 04:00:00 UTC+2 (timestamp 1774749600)
    # Future 2026-03-30 01:00 Prague: timestamp 1774825200

    midnight_dt = datetime(2026, 3, 29, 0, 0, 0, tzinfo=tz_prague)
    midnight_ts = int(midnight_dt.timestamp())

    ts_yesterday = midnight_ts - 300  # 23:55 previous day
    ts_today_1 = midnight_ts + 3600   # 01:00 today
    ts_today_2 = midnight_ts + 10800  # 04:00 today (after DST switch)
    ts_tomorrow = midnight_ts + 90000 # tomorrow

    content = "\n".join([
        f'{{"pnl_sats": 10.0, "ts": {ts_yesterday}, "fill_price": 70000.0, "qty": 0.001, "side": "Buy", "cid": "yest", "order_id": 1, "trade_id": 1}}',
        f'{{"pnl_sats": 20.0, "ts": {ts_today_1}, "fill_price": 70000.0, "qty": 0.001, "side": "Buy", "cid": "today1", "order_id": 2, "trade_id": 2}}',
        f'{{"pnl_sats": 30.0, "ts": {ts_today_2}, "fill_price": 70000.0, "qty": 0.001, "side": "Buy", "cid": "today2", "order_id": 3, "trade_id": 3}}',
        f'{{"pnl_sats": 40.0, "ts": {ts_tomorrow}, "fill_price": 70000.0, "qty": 0.001, "side": "Buy", "cid": "tomorrow", "order_id": 4, "trade_id": 4}}',
    ])
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    now_arg = "2026-03-29T12:00:00+02:00"
    report = generate_report_data(
        ledger_path=str(ledger_file),
        no_api=True,
        now_arg=now_arg,
        timezone_name="Europe/Prague",
    )

    day_metrics = report.timeframes["calendar_day"]
    assert day_metrics.trade_count == 2
    assert day_metrics.pnl_sats == 50.0  # today1 + today2
    assert day_metrics.win_count == 2


def test_multiple_restarts_survival(tmp_path, tz_prague):
    """
    Test that calendar_day survives multiple runtime restarts,
    while since_restart accurately isolates only trades since the latest restart.
    """
    # Fixed day: 2026-09-12
    # Restart 1 at 02:00 (ts1) -> Trade 1 at 02:30 (+15 sats)
    # Restart 2 at 06:00 (ts2) -> Trade 2 at 06:30 (+25 sats)
    # Restart 3 at 10:00 (ts3) -> Trade 3 at 10:30 (-10 sats)
    # Reference now: 2026-09-12 11:00:00 CEST (uptime = 3600 seconds = 1 hour, since 10:00)

    now_dt = datetime(2026, 9, 12, 11, 0, 0, tzinfo=tz_prague)
    now_ts = int(now_dt.timestamp())

    ts_t1 = int(datetime(2026, 9, 12, 2, 30, 0, tzinfo=tz_prague).timestamp())
    ts_t2 = int(datetime(2026, 9, 12, 6, 30, 0, tzinfo=tz_prague).timestamp())
    ts_t3 = int(datetime(2026, 9, 12, 10, 30, 0, tzinfo=tz_prague).timestamp())

    content = "\n".join([
        f'{{"pnl_sats": 15.0, "ts": {ts_t1}, "fill_price": 75000.0, "qty": 0.001, "side": "Buy", "cid": "t1", "order_id": 1, "trade_id": 1}}',
        f'{{"pnl_sats": 25.0, "ts": {ts_t2}, "fill_price": 75000.0, "qty": 0.001, "side": "Buy", "cid": "t2", "order_id": 2, "trade_id": 2}}',
        f'{{"pnl_sats": -10.0, "ts": {ts_t3}, "fill_price": 75000.0, "qty": 0.001, "side": "Sell", "cid": "t3", "order_id": 3, "trade_id": 3}}',
    ])
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    snapshot_data = {
        "system_mode": "Active",
        "btc_price": 75000.0,
        "btc_balance": 0.01,
        "locked_btc_reserve": 0.002,
        "usd_balance": 150.0,
        "uptime_seconds": 3600,  # 1 hour uptime
        "consecutive_losses": 1,
    }
    snap_file = tmp_path / "snapshot.json"
    snap_file.write_text(json.dumps(snapshot_data))

    report = generate_report_data(
        ledger_path=str(ledger_file),
        snapshot_file=str(snap_file),
        now_arg=str(now_ts),
        timezone_name="Europe/Prague",
    )

    day_tf = report.timeframes["calendar_day"]
    restart_tf = report.timeframes["since_restart"]
    life_tf = report.timeframes["ledger_lifetime"]

    # Calendar day must see all 3 trades today
    assert day_tf.trade_count == 3
    assert day_tf.pnl_sats == 30.0  # 15 + 25 - 10

    # Since restart must see only trade 3 (after 10:00)
    assert restart_tf.is_available is True
    assert restart_tf.trade_count == 1
    assert restart_tf.pnl_sats == -10.0

    # Lifetime sees all 3 trades
    assert life_tf.trade_count == 3
    assert life_tf.pnl_sats == 30.0


def test_empty_ledger_file_and_missing_file(tmp_path):
    """Test handling of completely empty file or non-existent file."""
    empty_file = tmp_path / "empty_ledger.jsonl"
    empty_file.write_text("")

    report_empty = generate_report_data(ledger_path=str(empty_file), no_api=True)
    assert report_empty.timeframes["ledger_lifetime"].trade_count == 0
    assert report_empty.timeframes["ledger_lifetime"].pnl_sats == 0.0
    assert report_empty.integrity.legacy_estimate_records == 0

    non_existent = tmp_path / "does_not_exist.jsonl"
    report_missing = generate_report_data(ledger_path=str(non_existent), no_api=True)
    assert report_missing.timeframes["ledger_lifetime"].trade_count == 0
    assert any("not found" in err for err in report_missing.integrity.malformed_errors)


def test_single_trade_win_loss_breakeven_stats(tmp_path):
    """Test statistical metrics (PF, payoff, win rates) on pure win, pure loss, and breakeven trades."""
    # 1. Pure wins
    file_win = tmp_path / "ledger_win.jsonl"
    file_win.write_text('{"pnl_sats": 50.0, "ts": 1000, "fill_price": 60000.0, "cid": "w1", "order_id": 1}\n')
    rep_win = generate_report_data(ledger_path=str(file_win), no_api=True, now_arg="2000")
    tf_win = rep_win.timeframes["ledger_lifetime"]
    assert tf_win.win_count == 1
    assert tf_win.loss_count == 0
    assert tf_win.win_rate_closed_pct == 100.0
    assert tf_win.profit_factor == float("inf")
    assert tf_win.payoff_ratio is None

    # 2. Pure loss
    file_loss = tmp_path / "ledger_loss.jsonl"
    file_loss.write_text('{"pnl_sats": -30.0, "ts": 1000, "fill_price": 60000.0, "cid": "l1", "order_id": 1}\n')
    rep_loss = generate_report_data(ledger_path=str(file_loss), no_api=True, now_arg="2000")
    tf_loss = rep_loss.timeframes["ledger_lifetime"]
    assert tf_loss.win_count == 0
    assert tf_loss.loss_count == 1
    assert tf_loss.win_rate_closed_pct == 0.0
    assert tf_loss.profit_factor == 0.0
    assert tf_loss.payoff_ratio == 0.0

    # 3. Breakeven
    file_be = tmp_path / "ledger_be.jsonl"
    file_be.write_text('{"pnl_sats": 0.0, "ts": 1000, "fill_price": 60000.0, "cid": "be1", "order_id": 1}\n')
    rep_be = generate_report_data(ledger_path=str(file_be), no_api=True, now_arg="2000")
    tf_be = rep_be.timeframes["ledger_lifetime"]
    assert tf_be.zero_count == 1
    assert tf_be.win_rate_closed_pct == 0.0
    assert tf_be.win_rate_total_pct == 0.0


def test_offline_api_graceful_handling(tmp_path):
    """Test that when API is offline / disabled, report generates cleanly with unverified labels."""
    content = '{"pnl_sats": 5.0, "ts": 1700000000, "fill_price": 70000.0, "qty": 0.001, "side": "Buy", "cid": "t1", "order_id": 10, "trade_id": 20}\n'
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    report = generate_report_data(ledger_path=str(ledger_file), no_api=True, now_arg="1700000100")

    assert report.equity.is_available is False
    assert report.timeframes["since_restart"].is_available is False
    assert report.timeframes["calendar_day"].trade_count >= 0
    assert len(report.warnings) > 0

    text_rep = format_text_report(report)
    assert "👑 ČÁSLAV :: PIRANA INSTITUTIONAL PERFORMANCE REPORT" in text_rep
    assert "[API OFFLINE]" in text_rep
    assert "PŮVOD DAT" in text_rep


def test_telegram_html_and_json_formatters(tmp_path):
    """Test Telegram HTML and JSON serialization formatting."""
    content = '{"pnl_sats": 12.5, "ts": 1700000000, "fill_price": 80000.0, "qty": 0.001, "side": "Buy", "cid": "t1", "order_id": 10, "trade_id": 20}\n'
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    snapshot_data = {
        "system_mode": "Active",
        "btc_price": 80000.0,
        "btc_balance": 0.02,
        "locked_btc_reserve": 0.005,
        "usd_balance": 500.0,
        "uptime_seconds": 1800,
    }
    snap_file = tmp_path / "snapshot.json"
    snap_file.write_text(json.dumps(snapshot_data))

    report = generate_report_data(
        ledger_path=str(ledger_file),
        snapshot_file=str(snap_file),
        now_arg="1700000100",
    )

    # Test Telegram HTML
    html_rep = format_telegram_html(report)
    assert "👑 <b>ČÁSLAV :: PIRANA INSTITUTIONAL REPORT</b>" in html_rep
    assert "<code>Active</code>" in html_rep
    assert "sats" in html_rep

    # Test JSON
    json_rep = format_json_report(report)
    parsed = json.loads(json_rep)
    assert parsed["ledger_path"] == str(ledger_file)
    assert "timeframes" in parsed
    assert "calendar_day" in parsed["timeframes"]
    assert "equity" in parsed


def test_cli_execution_and_file_export(tmp_path):
    """Test running pirana_report.py via CLI with arguments and writing output to file."""
    content = '{"pnl_sats": 8.0, "ts": 1700000000, "fill_price": 75000.0, "qty": 0.001, "side": "Buy", "cid": "t1", "order_id": 5, "trade_id": 6}\n'
    ledger_file = tmp_path / "trade_ledger.jsonl"
    ledger_file.write_text(content)

    out_file = tmp_path / "output_report.txt"
    json_out_file = tmp_path / "output_report.json"

    cmd_text = [
        sys.executable,
        "scripts/pirana_report.py",
        "--legacy",
        "--ledger", str(ledger_file),
        "--no-api",
        "--now", "1700000050",
        "--timezone", "Europe/Prague",
        "--output", str(out_file),
    ]
    res_text = subprocess.run(cmd_text, capture_output=True, text=True)
    assert res_text.returncode == 0
    assert out_file.exists()
    assert "KAPITÁLOVÁ ROZVAHA" in out_file.read_text()

    cmd_json = [
        sys.executable,
        "scripts/pirana_report.py",
        "--legacy",
        "--ledger", str(ledger_file),
        "--no-api",
        "--now", "1700000050",
        "--json",
        "--output", str(json_out_file),
    ]
    res_json = subprocess.run(cmd_json, capture_output=True, text=True)
    assert res_json.returncode == 0
    assert json_out_file.exists()
    parsed = json.loads(json_out_file.read_text())
    assert "timeframes" in parsed


def canonical_fixture(now_ms):
    period = dict(gross_pnl_usd='12.2500', net_pnl_usd='10.1250', fees_usd='2.1250',
                  closed_count=2, win_count=1, loss_count=1)
    return dict(schema_version=1, source='authenticated_bitfinex_fills',
                scope='account:tBTCUSD', generated_at_ms=now_ms,
                sync={'complete': True, 'cursor_ms': now_ms}, status='complete', issues=[],
                daily=period.copy(), lifetime=period.copy())


@pytest.mark.parametrize('case', ['missing', 'corrupt', 'stale', 'schema', 'day', 'incomplete', 'nan', 'null', 'counts', 'cursor', 'cursor_day'])
def test_canonical_fails_closed(tmp_path, case):
    from scripts.pirana_report import generate_report_data as canonical_report
    now = int(datetime(2026, 9, 18, 0, 0, 10, tzinfo=zoneinfo.ZoneInfo('Europe/Prague')).timestamp() * 1000)
    value = canonical_fixture(now)
    if case == 'stale': value['generated_at_ms'] -= 120001
    if case == 'day': value['generated_at_ms'] -= 11000
    if case == 'schema': value['schema_version'] = 2
    if case == 'incomplete': value['sync']['complete'] = False
    if case == 'nan': value['daily']['net_pnl_usd'] = 'NaN'
    if case == 'null': value['daily']['net_pnl_usd'] = None
    if case == 'cursor': value['sync']['cursor_ms'] -= 120001
    if case == 'cursor_day': value['sync']['cursor_ms'] -= 11000
    if case == 'counts': value['daily']['win_count'] = None
    path = tmp_path / 'accounting.json'
    if case != 'missing': path.write_text('oops' if case == 'corrupt' else json.dumps(value))
    ledger = tmp_path / 'legacy.jsonl'
    ledger.write_text(json.dumps(dict(ts=now//1000, pnl_sats=999999999, fill_price=60000,
                                      cid='shadow_fake', order_id=1, trade_id=2)))
    report = canonical_report(snapshot_file=str(path), ledger_path=str(ledger), now_arg=str(now/1000), no_api=True)
    assert report['accounting']['status'] == 'incomplete'
    assert report['accounting']['daily']['net_pnl_usd'] is None
    assert report['accounting']['lifetime']['net_pnl_usd'] is None
    assert 'NEOVĚŘENO' in format_text_report(report)
    assert '999999999' not in format_json_report(report)
    assert report['legacy_unverified_diagnostics']['shadow_trades_excluded'] == 1


def test_canonical_keeps_decimal_strings_and_account_scope(tmp_path):
    from scripts.pirana_report import generate_report_data as canonical_report
    now = 1789640000000
    path = tmp_path / 'accounting.json'
    path.write_text(json.dumps(canonical_fixture(now)))
    report = canonical_report(snapshot_file=str(path), ledger_path=str(tmp_path/'absent'), now_arg=str(now/1000))
    assert report['accounting']['daily']['net_pnl_usd'] == '10.1250'
    assert 'všechny obchody účtu' in format_text_report(report)
    assert '10.1250 USD' in format_telegram_html(report)
    assert 'valid_live' not in format_json_report(report)


def test_legacy_missing_trade_id_and_fee_unverified():
    rec, _, _ = validate_and_classify_trade(dict(ts=1700000000, pnl_sats=1, fill_price=60000, order_id=1, cid='x'))
    assert rec.is_unverifiable_order
    assert any('trade_id=0' in r for r in rec.unverifiable_reasons)
    assert any('fee evidence' in r for r in rec.unverifiable_reasons)


def test_dashboard_nullable_rendering_and_failure_clears_profit():
    import pathlib
    import shutil
    if not shutil.which('node'):
        pytest.skip('Node unavailable for actual dashboard script execution')
    source = pathlib.Path('crates/pirana-dashboard/static/dashboard.html').read_text().split('<script>')[1].split('</script>')[0]
    harness = r'''
const vm = require('vm');
const assert = require('assert');
const elements = {};
const context = {document:{getElementById(id){return elements[id] ||= {textContent:''}}},
 setInterval(){}, AbortSignal:{timeout(){}}, fetch(){return new Promise(()=>{})}};
vm.createContext(context);
vm.runInContext(SOURCE, context);
assert.equal(context.accountingMoney(null), 'NEOVĚŘENO');
assert.equal(context.accountingMoney('0'), '0');
assert.equal(context.accountingMoney('NaN'), 'NEOVĚŘENO');
context.renderAccounting({status:'complete',daily:{net_pnl_usd:'10.1250'},lifetime:{net_pnl_usd:'20'}});
assert.equal(elements['daily-net'].textContent, '10.1250');
context.renderAccounting({status:'incomplete',issues:['stale'],daily:{net_pnl_usd:'999'}});
assert.equal(elements['daily-net'].textContent, 'NEOVĚŘENO');
context.renderAccounting(null);
assert.equal(elements['lifetime-net'].textContent, 'NEOVĚŘENO');
'''.replace('SOURCE', json.dumps(source))
    result = subprocess.run(['node', '-e', harness], capture_output=True, text=True)
    assert result.returncode == 0, result.stderr


def active_period_fixture(now_ms):
    value = canonical_fixture(now_ms)
    value['status'] = 'incomplete'
    value['sync']['coverage_start_ms'] = 0
    value['issues'] = ['historical basis missing']
    value['active_period'] = dict(id='period-1', start_ms=now_ms - 1000,
        status='complete', issues=[], gross_pnl_usd='0', net_pnl_usd='0', fees_usd='0',
        closed_count=0, win_count=0, loss_count=0)
    return value


def test_new_period_zero_preserves_unknown_history():
    from scripts.pirana_report import validate_accounting_projection
    now = 1789640000000
    result = validate_accounting_projection(active_period_fixture(now), now)
    assert result['active_period']['net_pnl_usd'] == '0'
    assert result['status'] == 'incomplete'
    assert result['daily']['net_pnl_usd'] is None
    assert result['lifetime']['net_pnl_usd'] is None
    text = format_text_report(dict(accounting=result, snapshot_source='fixture'))
    assert 'Nové období od ' in text and '+02:00 Europe/Prague' in text
    assert 'Celá pokrytá historie' in text
    assert 'Čistý realizovaný PnL: 0 USD' in text
    assert 'Čistý realizovaný PnL: NEOVĚŘENO' in text


@pytest.mark.parametrize('field,bad', [
    ('net_pnl_usd', True), ('net_pnl_usd', 'NaN'), ('net_pnl_usd', 'Infinity'),
    ('net_pnl_usd', None), ('start_ms', True), ('start_ms', 9999999999999),
    ('id', ''), ('closed_count', True), ('win_count', 1),
    ('status', 'incomplete'), ('issues', ['missing basis']),
])
def test_invalid_active_period_is_unknown(field, bad):
    from scripts.pirana_report import validate_accounting_projection
    now = 1789640000000
    value = active_period_fixture(now)
    value['active_period'][field] = bad
    result = validate_accounting_projection(value, now)['active_period']
    assert result is None or (result['status'] == 'incomplete' and result['net_pnl_usd'] is None)


@pytest.mark.parametrize('case', ['timestamp', 'cursor', 'before_start', 'sync', 'schema', 'scope', 'source'])
def test_active_period_requires_verified_capture(case):
    from scripts.pirana_report import validate_accounting_projection
    now = 1789640000000
    value = active_period_fixture(now)
    if case == 'timestamp': value['generated_at_ms'] -= 120001
    if case == 'cursor': value['sync']['cursor_ms'] -= 120001
    if case == 'before_start': value['sync']['cursor_ms'] -= 1001
    if case == 'sync': value['sync']['complete'] = False
    if case == 'schema': value['schema_version'] = True
    if case == 'scope': value['scope'] = 'strategy'
    if case == 'source': value['source'] = 'legacy'
    result = validate_accounting_projection(value, now)['active_period']
    assert result is None or (result['status'] == 'incomplete' and result['net_pnl_usd'] is None)


def test_real_backend_period_snapshot_consumed_without_losing_zero(tmp_path):
    from scripts import pirana_accounting as backend
    from scripts.pirana_report import generate_report_data as canonical_report
    now_ms = 1789640000000
    con = backend.connect(tmp_path / 'accounting.sqlite3', True)
    try:
        backend.ingest(con, dict(fills=[dict(trade_id=1, order_id=101, symbol='tBTCUSD',
            mts=now_ms - 1000, exec_amount='-1', exec_price='100', fee='0',
            fee_currency='USD', cid=None)],
            sync=dict(start_ms=0, end_ms=now_ms, complete=True)))
        backend.start_period(con, 'new-period', now_ms - 50)
        snapshot = backend.snapshot(con, datetime.fromtimestamp(now_ms / 1000, zoneinfo.ZoneInfo('UTC')))
    finally:
        con.close()
    assert snapshot['sync']['coverage_start_ms'] == 0
    assert 'coverage_start_ms' not in snapshot
    path = tmp_path / 'snapshot.json'
    path.write_text(json.dumps(snapshot))
    report = canonical_report(snapshot_file=str(path), ledger_path=str(tmp_path / 'absent'),
                              now_arg=str(now_ms / 1000), no_api=True)
    accounting = report['accounting']
    assert accounting['active_period'] == snapshot['active_period']
    assert accounting['active_period']['net_pnl_usd'] == '0'
    assert accounting['active_period']['status'] == 'complete'
    assert accounting['status'] == 'incomplete'
    assert accounting['lifetime']['net_pnl_usd'] is None
    assert accounting['daily']['net_pnl_usd'] is None
