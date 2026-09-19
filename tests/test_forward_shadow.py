import importlib.util
from pathlib import Path

import pytest

spec = importlib.util.spec_from_file_location("forward_shadow", Path(__file__).parents[1] / "scripts/forward_shadow.py")
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)


def book():
    return {"bids": [{"price": 100., "quantity": 1.}],
            "asks": [{"price": 101., "quantity": 1.}]}


def test_observation_cannot_be_mistaken_for_promotion_evidence():
    snap = {"order_book": book(), "api_key": "must-not-copy",
            "recent_signals": [{"id": "s1", "secret": "must-not-copy"}]}
    row = collector.observation(snap, 10., 123, "revision")
    assert not row["promotion_eligible"]
    assert row["exchange_quote_timestamp"] is None
    assert "must-not-copy" not in str(row)
    assert row["monotonic_ns"] == 123


@pytest.mark.parametrize("price", [0., -1., float("nan"), float("inf"), True, "100"])
def test_invalid_quote_rejected(price):
    levels = book()
    levels["bids"][0]["price"] = price
    with pytest.raises(ValueError):
        collector.observation({"order_book": levels}, 10., 1, "rev")


def test_crossed_book_rejected():
    levels = book()
    levels["bids"][0]["price"] = 102.
    with pytest.raises(ValueError):
        collector.observation({"order_book": levels}, 10., 1, "rev")


def test_revision_changes_with_artifact(tmp_path):
    source = tmp_path / "source"
    source.write_text("before")
    old = collector.fingerprint([source])
    source.write_text("after")
    assert collector.fingerprint([source]) != old
