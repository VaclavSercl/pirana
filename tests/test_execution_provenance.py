"""Reporting-only classification; synthetic inventory remains operationally intact."""
import copy
import datetime as dt
import json
from unittest.mock import patch

from scripts import pirana_accounting as accounting
from scripts import pirana_report as reporting
from scripts import pirana_measurements as measurements

NOW = 1790668800000


def synthetic():
    return dict(trade_id=1978200001, order_id=244505000001, cid="28638000000001",
                exec_amount="0.00051", exec_price="81500", fee="0", fee_currency="USD",
                symbol="tBTCUSD", mts=NOW-2000)


def test_known_adjustment_and_any_identity_collision_are_unverified():
    f = synthetic()
    before = copy.deepcopy(f)
    provenance = accounting.execution_provenance([f])
    assert f == before
    entry = provenance["excluded_records"][0]
    assert entry["classification"] == "legacy_reserve_activation"
    assert entry["actual_acquisition_basis"] == "UNKNOWN"
    assert entry["owner_authorization"] == "NOT_ESTABLISHED"
    for key, value in (("exec_price", "1"), ("exec_amount", "0.1"), ("cid", "other"),
                       ("trade_id", 42), ("order_id", 43)):
        altered = dict(f, **{key: value})
        assert accounting.execution_provenance([altered])["excluded_records"][0]["classification"] == "reserved_identity_collision"
    ordinary = dict(f, trade_id=7, order_id=8, cid="9")
    assert accounting.execution_provenance([ordinary])["excluded_records"] == []


def test_snapshot_excludes_synthetic_execution_and_preserves_database_rows(tmp_path):
    con = accounting.connect(tmp_path/"fixture.sqlite3", True)
    try:
        accounting.ingest(con, dict(fills=[synthetic()], sync=dict(start_ms=0, end_ms=NOW, complete=True)))
        con.execute(accounting.EPOCH_DDL)
        con.execute("INSERT INTO trading_epoch VALUES (1, 'fixture', ?, '0')", (NOW-3000,))
        accounting.start_period(con, "fixture", NOW-3000)
        before = con.execute("SELECT * FROM fills").fetchall()
        actual = accounting.snapshot(con, dt.datetime.fromtimestamp(NOW/1000, dt.timezone.utc))
        assert actual['fill_count']==0
        assert actual['operational']['status']=='incomplete'
        assert 'operational_opening_lot_missing' in actual['operational']['issues']
        assert actual['operational']['open_lots']==[]
        assert actual['active_period']['net_pnl_usd'] is None
        assert con.execute("SELECT * FROM fills").fetchall()==before
    finally:
        con.close()


def test_report_cannot_claim_verified_active_profit_for_mixed_source():
    period = dict(id="fixture", start_ms=NOW-3000, status="complete", issues=[],
                  gross_pnl_usd="999", net_pnl_usd="999", fees_usd="0",
                  closed_count=1, win_count=1, loss_count=0)
    raw = dict(schema_version=1, source="authenticated_bitfinex_fills", scope="account:tBTCUSD",
               generated_at_ms=NOW, status="complete", issues=[],
               sync=dict(cursor_ms=NOW, coverage_start_ms=0, complete=True),
               active_period=period, daily=period, lifetime=period,
               execution_provenance=accounting.execution_provenance([synthetic()]))
    validated = reporting.validate_accounting_projection(raw, NOW)
    assert validated["active_period"]["net_pnl_usd"] is None
    assert validated["daily"]["net_pnl_usd"] is None
    text = reporting.format_text_report(dict(accounting=validated, snapshot_source="fixture"))
    assert "999" not in text and "51 000 sats" in text
    assert "autentizované filly → FIFO" not in text
    assert "smíšený původ" in text


def test_measurements_lists_exclusion_without_counting_it_as_execution(capsys):
    real = dict(synthetic(), trade_id=7, order_id=8, cid="9", exec_amount="-0.0001", mts=NOW-1000)
    fills = [synthetic(), real]
    with patch("sys.argv", ["measurements", "--db", "unused", "--positions", "unused",
            "--equity-dir", "unused", "--start-ms", str(NOW-5000), "--end-ms", str(NOW)]), patch.object(
            measurements, "canonical_fills", return_value=([real], (NOW, 0, 1), accounting.execution_provenance(fills))), patch.object(
            measurements, "read_benchmarks", return_value={}), patch.object(measurements, "read_equity", return_value=([], [])):
        assert measurements.main() == 0
    result = json.loads(capsys.readouterr().out)
    assert result["status"] == "INCOMPLETE"
    assert result["slippage"]["fill_count"] == 1
    assert result["slippage"]["missing"] == [dict(trade_id=7, order_id=8)]
    assert result["history"]["gaps"][0]["reason"] == "missing_acquisition_basis"
    assert result["execution_provenance"]["excluded_records"][0]["trade_id"] == 1978200001
