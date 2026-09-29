#!/usr/bin/env python3
"""Validate owner-approved funding-date marks without changing FIFO acquisition basis."""
import argparse
from datetime import datetime
from decimal import Decimal
import hashlib
import json
from pathlib import Path
import re
from urllib.parse import parse_qs, urlsplit

CLASSIFICATION = "funding_date_market_valuation"
METHOD = "last completed 1m BTCUSD candle close before deposit"
MAX_BYTES = 2 * 1024 * 1024


def strict_json(text):
    def reject(_): raise ValueError("non-finite JSON")
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result: raise ValueError("duplicate JSON key")
            result[key] = value
        return result
    return json.loads(text, parse_float=Decimal, parse_constant=reject, object_pairs_hook=unique)


def decimal(value, positive=True):
    if isinstance(value, bool) or not isinstance(value, (str, int, float, Decimal)):
        raise ValueError("invalid decimal type")
    result = Decimal(str(value))
    if not result.is_finite() or abs(result.adjusted()) > 30 or (result <= 0 if positive else result < 0):
        raise ValueError("invalid decimal range")
    return result


def integer(value):
    if type(value) is not int or value <= 0: raise ValueError("invalid timestamp/identity")
    return value


def candle(value):
    if not isinstance(value, list) or len(value) != 6: raise ValueError("invalid candle")
    mts = integer(value[0])
    if mts % 60000: raise ValueError("candle not aligned to UTC minute")
    opened, close, high, low = [decimal(x) for x in value[1:5]]
    volume = decimal(value[5], positive=False)
    if low > min(opened, close) or high < max(opened, close) or low > high:
        raise ValueError("invalid OHLC range")
    return [mts, opened, close, high, low, volume]


def validate_entry(entry):
    if not isinstance(entry, dict) or type(entry.get("schema_version")) is not int or entry["schema_version"] != 1:
        raise ValueError("invalid valuation schema")
    if entry.get("classification") != CLASSIFICATION or entry.get("actual_acquisition_basis") != "UNKNOWN":
        raise ValueError("valuation must not claim actual acquisition basis")
    if entry.get("pricing_method") != METHOD:
        raise ValueError("unsupported pricing method")
    if not isinstance(entry.get("owner_approval"), str) or not entry["owner_approval"].strip():
        raise ValueError("missing owner approval reference")
    deposited, ledger = integer(entry["deposit_mts"]), integer(entry["ledger_id"])
    qty, price = decimal(entry["quantity_btc"]), decimal(entry["price_usd"])
    uri = urlsplit(entry["source_uri"])
    if (uri.scheme != "https" or uri.netloc != "api-pub.bitfinex.com"
            or uri.path != "/v2/candles/trade:1m:tBTCUSD/hist" or uri.fragment):
        raise ValueError("source is not official BTCUSD one-minute candle endpoint")
    query = parse_qs(uri.query, strict_parsing=True)
    if set(query) != {"start", "end", "limit", "sort"} or any(len(x) != 1 for x in query.values()):
        raise ValueError("ambiguous source query")
    start, end, limit, sort = (int(query[x][0]) for x in ("start", "end", "limit", "sort"))
    if not 0 <= start <= end < deposited or not 1 <= limit <= 10000 or sort != 1:
        raise ValueError("invalid source interval")
    raw = entry.get("response_utf8")
    if not isinstance(raw, str) or len(raw.encode("utf-8")) > MAX_BYTES:
        raise ValueError("missing or oversized embedded source bytes")
    digest = entry.get("response_sha256")
    if not isinstance(digest, str) or not re.fullmatch("[0-9a-f]{64}", digest):
        raise ValueError("invalid evidence digest")
    if hashlib.sha256(raw.encode("utf-8")).hexdigest() != digest:
        raise ValueError("source evidence digest mismatch")
    response = strict_json(raw)
    if not isinstance(response, list) or not response or len(response) >= limit:
        raise ValueError("empty or possibly truncated candle response")
    candles = [candle(x) for x in response]
    times = [x[0] for x in candles]
    if times != sorted(set(times)) or any(not start <= t <= end for t in times):
        raise ValueError("candle ordering or source interval mismatch")
    completed = [x for x in candles if x[0] + 60000 <= deposited]
    if not completed: raise ValueError("no completed candle before deposit")
    selected = completed[-1]
    if deposited - (selected[0] + 60000) > 120000:
        raise ValueError("last completed candle is too old")
    if candle(entry["candle"]) != selected or price != selected[2]:
        raise ValueError("selected price/candle does not match source")
    fetched = datetime.fromisoformat(entry["fetched_at"])
    if fetched.tzinfo is None or fetched.timestamp() * 1000 < deposited:
        raise ValueError("invalid source fetch timestamp")
    return dict(ledger_id=ledger, deposit_mts=deposited, quantity_btc=format(qty, "f"),
                quantity_sats=format(qty * 100000000, "f"), price_usd=format(price, "f"),
                starting_valuation_usd=format(qty * price, "f"), candle_mts=selected[0],
                source_uri=entry["source_uri"], response_sha256=digest,
                classification=CLASSIFICATION, actual_acquisition_basis="UNKNOWN",
                label="Owner-approved starting market valuation at funding date; not purchase cost or PnL")


def validate_envelope(value):
    if (not isinstance(value, dict) or type(value.get("schema_version")) is not int
            or value["schema_version"] != 1 or value.get("classification") != CLASSIFICATION
            or value.get("actual_acquisition_basis") != "UNKNOWN"):
        raise ValueError("invalid funding valuation envelope")
    entries = value.get("entries")
    if not isinstance(entries, list) or not 1 <= len(entries) <= 1000:
        raise ValueError("invalid entries")
    validated = [validate_entry(x) for x in entries]
    if len({x["ledger_id"] for x in validated}) != len(validated):
        raise ValueError("duplicate ledger identity")
    return dict(status="VALUED_NOT_COST_BASIS", classification=CLASSIFICATION,
                actual_acquisition_basis="UNKNOWN", entries=validated,
                total_funding_btc=format(sum((Decimal(x["quantity_btc"]) for x in validated), Decimal(0)), "f"),
                total_starting_valuation_usd=format(sum((Decimal(x["starting_valuation_usd"]) for x in validated), Decimal(0)), "f"),
                historical_fifo_status="UNCHANGED",
                limitation="Funding-date market marks are owner-approved starting valuations, not actual acquisition costs. Full historical performance still requires every BTC/UST/USD flow, other-pair trade, withdrawal, fee and opening balance; these marks never fill canonical FIFO gaps.")


def load_valuations(path):
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > MAX_BYTES:
        raise ValueError("unsafe, missing or oversized funding valuation file")
    raw = path.read_bytes()
    result = validate_envelope(strict_json(raw.decode("utf-8")))
    result["artifact_sha256"] = hashlib.sha256(raw).hexdigest()
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("path")
    args = parser.parse_args()
    try:
        result = load_valuations(args.path)
    except (OSError, ValueError, KeyError, TypeError, ArithmeticError):
        result = dict(status="UNVERIFIED", actual_acquisition_basis="UNKNOWN", reason="invalid funding valuation evidence")
        print(json.dumps(result)); return 2
    print(json.dumps(result, sort_keys=True)); return 0


if __name__ == "__main__":
    raise SystemExit(main())
