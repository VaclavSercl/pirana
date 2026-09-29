"""No network: funding marks must remain separate from actual acquisition basis."""
import copy
import hashlib
import json
from unittest.mock import patch

import pytest
from scripts import pirana_funding_valuations as fv
from scripts import pirana_measurements as measurements


def fixture():
    mts = 1768687200000
    candles = [[mts-60000, 95270, 95271, 95272, 95270, 0.01],
               [mts, 95275, 95272, 95275, 95271, 0.08156498]]
    raw = json.dumps(candles)
    entry = dict(schema_version=1, classification=fv.CLASSIFICATION, actual_acquisition_basis="UNKNOWN",
        deposit_mts=mts+93000, quantity_btc="0.00490883", ledger_id=10200966013,
        pricing_method=fv.METHOD, price_usd="95272", candle=candles[-1],
        source_uri=f"https://api-pub.bitfinex.com/v2/candles/trade:1m:tBTCUSD/hist?start={mts-60000}&end={mts+59999}&limit=100&sort=1",
        response_utf8=raw, response_sha256=hashlib.sha256(raw.encode()).hexdigest(),
        fetched_at="2026-09-29T05:41:46+00:00", owner_approval="explicit fixture approval")
    return dict(schema_version=1, classification=fv.CLASSIFICATION,
                actual_acquisition_basis="UNKNOWN", entries=[entry])


def test_valid_valuation_is_not_acquisition_basis_or_fifo_repair(tmp_path):
    raw = fixture()
    before = copy.deepcopy(raw)
    result = fv.validate_envelope(raw)
    assert raw == before
    assert result["entries"][0]["starting_valuation_usd"] == "467.67405176"
    assert result["actual_acquisition_basis"] == "UNKNOWN"
    assert result["historical_fifo_status"] == "UNCHANGED"
    assert result["status"] == "VALUED_NOT_COST_BASIS"
    path = tmp_path/"funding.json"
    path.write_text(json.dumps(raw))
    assert fv.load_valuations(path)["artifact_sha256"] == hashlib.sha256(path.read_bytes()).hexdigest()


@pytest.mark.parametrize("key,value", [
    ("response_utf8", "[]"), ("response_sha256", "0"*64),
    ("price_usd", "95273"), ("price_usd", "NaN"), ("quantity_btc", "Infinity"),
    ("quantity_btc", True), ("quantity_btc", "-1"),
    ("actual_acquisition_basis", "KNOWN"), ("deposit_mts", 1768687200000),
    ("deposit_mts", 1768687500001), ("fetched_at", "2020-01-01T00:00:00+00:00"),
    ("owner_approval", ""), ("source_uri", "https://example.test/candle"),
])
def test_tamper_wrong_date_nonfinite_and_false_basis_rejected(key, value):
    raw = fixture()
    raw["entries"][0][key] = value
    with pytest.raises((ValueError, ArithmeticError)):
        fv.validate_envelope(raw)


def test_previous_or_unaligned_candle_not_accepted():
    raw = fixture()
    raw["entries"][0]["candle"][0] -= 60000
    with pytest.raises(ValueError): fv.validate_envelope(raw)
    raw = fixture()
    raw["entries"][0]["candle"][0] += 1
    with pytest.raises(ValueError): fv.validate_envelope(raw)


def test_duplicate_identity_and_duplicate_json_keys_rejected():
    raw = fixture()
    raw["entries"].append(copy.deepcopy(raw["entries"][0]))
    with pytest.raises(ValueError): fv.validate_envelope(raw)
    with pytest.raises(ValueError): fv.strict_json('{"x":1,"x":2}')
    with pytest.raises(ValueError): fv.strict_json('{"x":NaN}')


def test_measurement_optional_marks_never_clear_historical_gaps(tmp_path, capsys):
    path = tmp_path/"funding.json"
    path.write_text(json.dumps(fixture()))
    gap = dict(status="INCOMPLETE", gaps=[dict(reason="missing_acquisition_basis")])
    argv = ["measurements", "--db", "unused", "--positions", "unused", "--equity-dir", "unused",
            "--start-ms", "1", "--end-ms", "100", "--funding-valuations", str(path)]
    with patch("sys.argv", argv), patch.object(measurements, "canonical_fills", return_value=([], (100, 0, 1), dict(excluded_records=[]))), patch.object(
            measurements, "read_benchmarks", return_value={}), patch.object(measurements, "read_equity", return_value=([], [])), patch.object(
            measurements, "historical_gaps", return_value=gap):
        assert measurements.main() == 0
    data = json.loads(capsys.readouterr().out)
    assert data["history"] == gap and data["status"] == "INCOMPLETE"
    assert data["funding_valuations"]["actual_acquisition_basis"] == "UNKNOWN"


def owner_fixture():
    return dict(schema_version=1, classification=fv.OWNER_CLASSIFICATION,
        actual_acquisition_basis="UNKNOWN", entries=[dict(schema_version=1,
        classification=fv.OWNER_CLASSIFICATION, actual_acquisition_basis="UNKNOWN",
        ledger_id=10200966013, deposit_mts=1768687293000, quantity_btc="0.00490883",
        price_usd="99999", owner_reference="owner instruction 2026-09-29",
        owner_approval="explicit fixture approval", declared_at="2026-09-29T20:00:00+00:00")])


def test_owner_basis_distinct_from_market_and_fifo(tmp_path):
    raw = owner_fixture()
    before = copy.deepcopy(raw)
    result = fv.validate_envelope(raw)
    assert raw == before
    assert result["status"] == "OWNER_DECLARED_NOT_VENUE_VERIFIED"
    assert result["historical_fifo_status"] == "UNCHANGED"
    assert result["actual_acquisition_basis"] == "UNKNOWN"
    entry = result["entries"][0]
    assert entry["owner_declared_cost_basis_usd"] == "490.87809117"
    assert entry["starting_valuation_usd"] == "490.87809117"
    assert entry["source"] == "owner_declaration" and "candle_mts" not in entry
    path = tmp_path / "owner.json"
    path.write_text(json.dumps(raw))
    assert fv.load_valuations(path)["classification"] == fv.OWNER_CLASSIFICATION


@pytest.mark.parametrize("key,value", [
    ("price_usd", "NaN"), ("price_usd", "Infinity"), ("price_usd", 0),
    ("price_usd", True), ("quantity_btc", "-1"),
    ("owner_approval", ""), ("owner_reference", " "),
    ("declared_at", "2026-09-29T20:00:00"),
    ("declared_at", "2020-01-01T00:00:00+00:00"),
    ("source_uri", "https://api-pub.bitfinex.com/"), ("candle", []),
    ("actual_acquisition_basis", "KNOWN"),
])
def test_owner_declaration_invalid_or_misrepresented_rejected(key, value):
    raw = owner_fixture()
    raw["entries"][0][key] = value
    with pytest.raises((ValueError, ArithmeticError)):
        fv.validate_envelope(raw)


def test_owner_and_market_envelopes_cannot_mix_sources():
    market, owner = fixture(), owner_fixture()
    market["entries"].append(owner["entries"][0])
    owner["entries"].append(fixture()["entries"][0])
    for raw in [market, owner]:
        with pytest.raises(ValueError): fv.validate_envelope(raw)
