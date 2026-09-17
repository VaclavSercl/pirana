"""Offline report-consumer contracts; no service calls or Telegram delivery."""
import importlib
import io
import os
from unittest.mock import patch

from scripts import pirana_report
import pirana_exporter


def unavailable():
    return {"accounting": pirana_report.canonical_unavailable("test incomplete"),
            "snapshot_source": "fixture"}


def test_exporter_unknown_is_nan():
    with patch.object(pirana_exporter, "generate_report_data", return_value=unavailable()):
        text = "\n".join(pirana_exporter.accounting_metrics())
    assert "pirana_daily_pnl_usd NaN" in text
    assert "pirana_accounting_complete 0" in text


def test_exporter_verified_zero_is_zero():
    data = unavailable()
    data["accounting"]["status"] = "complete"
    data["accounting"]["daily"]["net_pnl_usd"] = "0.0000"
    with patch.object(pirana_exporter, "generate_report_data", return_value=data):
        assert "pirana_daily_pnl_usd 0.0000" in pirana_exporter.accounting_metrics()


def test_scheduled_unknown():
    with patch.dict(os.environ, {"TELEGRAM_BOT_TOKEN": "test"}):
        module = importlib.import_module("scripts.send_scheduled_report")
    with patch.object(module, "generate_report_data", return_value=unavailable()):
        text = module.build_report()
    assert "NEOVĚŘENO" in text
    assert "$0.0000" not in text


def test_status_unknown_without_network_or_credentials():
    with patch("builtins.open", return_value=io.StringIO("TELEGRAM_BOT_TOKEN=test\n")), patch("os.path.exists", return_value=True):
        module = importlib.import_module("scripts.telegram_control_bot")
    with patch.object(module, "generate_report_data", return_value=unavailable()), patch.object(module, "send_telegram") as send:
        module.handle_status(123)
    text = send.call_args.args[1]
    assert "NEOVĚŘENO" in text
    assert "+0.0000" not in text


def test_yearly_does_not_invent_annual_return():
    module = importlib.import_module("scripts.send_yearly_report")
    with patch.object(module, "generate_report_data", return_value=unavailable()):
        text = module.build_yearly_report("2025", {"net_pnl": 999}, {"starting_equity": 393.56})
    assert "NEOVĚŘENO" in text
    assert "393.56" not in text
    assert "999" not in text
    assert "99.99%" not in text


def test_full_exporter_handler_unknown_samples_are_valid_prometheus():
    from types import SimpleNamespace
    import json
    import math
    from unittest.mock import Mock

    for invalid in (None, True, "invalid", "NaN", float("inf")):
        response = SimpleNamespace(read=lambda: json.dumps({
            "btc_price": invalid, "trades_today": invalid,
            "daily_pnl": invalid, "win_rate": invalid,
        }).encode())
        handler = SimpleNamespace(path="/metrics", wfile=io.BytesIO(),
                                  send_response=Mock(), send_header=Mock(), end_headers=Mock())
        with patch.object(pirana_exporter.urllib.request, "urlopen", return_value=response), patch.object(
                pirana_exporter, "generate_report_data", return_value=unavailable()):
            pirana_exporter.MetricsHandler.do_GET(handler)
        handler.send_response.assert_called_once_with(200)
        samples = dict(line.split() for line in handler.wfile.getvalue().decode().splitlines()
                       if line and not line.startswith("#"))
        assert math.isnan(float(samples["pirana_btc_price"]))
        assert math.isnan(float(samples["pirana_trades_today_total"]))
        assert math.isnan(float(samples["pirana_daily_pnl_usd"]))
        assert samples["pirana_accounting_complete"] == "0"


def test_exporter_sample_validation():
    assert pirana_exporter.metric_sample(0, count=True) == "0"
    assert pirana_exporter.metric_sample(-1, count=True) == "NaN"
    assert pirana_exporter.metric_sample(1.5, count=True) == "NaN"
    assert pirana_exporter.metric_sample("-1.50") == "-1.50"
    assert pirana_exporter.metric_sample({}) == "NaN"


def test_exporter_new_period_zero_with_unknown_history():
    data = unavailable()
    data['accounting']['active_period'] = dict(status='complete', net_pnl_usd='0')
    with patch.object(pirana_exporter, 'generate_report_data', return_value=data):
        metrics = pirana_exporter.accounting_metrics()
    assert 'pirana_period_complete 1' in metrics
    assert 'pirana_period_net_pnl_usd 0' in metrics
    assert 'pirana_daily_pnl_usd NaN' in metrics
    assert 'pirana_accounting_complete 0' in metrics


def test_exporter_period_unknown_is_nan():
    for period in (None, dict(status='incomplete', net_pnl_usd='0')):
        data = unavailable()
        data['accounting']['active_period'] = period
        with patch.object(pirana_exporter, 'generate_report_data', return_value=data):
            metrics = pirana_exporter.accounting_metrics()
        assert 'pirana_period_complete 0' in metrics
        assert 'pirana_period_net_pnl_usd NaN' in metrics
