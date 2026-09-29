"""Offline calibration/daily status regressions: no live sends or service changes."""
import io
import json
import subprocess
from pathlib import Path
from unittest.mock import patch

from scripts import send_recalibration_report as report
from scripts import pirana_report

NOW = 1790668800


def calibration():
    data = dict(generation=48, sample_size=1000, calibrated_at=NOW-100)
    for name in report.FIELDS:
        data[name] = dict(value=0.1, computed_at=NOW-100, formula="fixture", inputs="fixture")
    return data


def snapshot():
    return dict(calibration=calibration(), uptime_seconds=1000)


def test_unknown_nan_and_range_never_become_zero():
    for value in (None, float("nan"), float("inf"), True, -1, 1.1, "0"):
        snap = snapshot()
        snap["calibration"]["p_ruin_1y"]["value"] = value
        text, status = report.build_calibration_report(snap, None, (True, "fixture"), NOW)
        assert "p_ruin_1y: NEOVĚŘENO" in text
        assert status == 2
    text, status = report.build_calibration_report({}, None, (False, "missing"), NOW)
    assert status == 2 and "NEOVĚŘENO" in text


def test_actual_interval_restored_generation_not_event_count():
    snap = snapshot()
    snap["uptime_seconds"] = 20
    text, _ = report.build_calibration_report(snap, dict(observed_at=NOW-900, generation=1),
                                               (True, "fixture"), NOW)
    assert "(900 s)" in text and "obnovený historický stav" in text
    assert "Počet rekalibrací a změna za 24 h: NEOVĚŘENO" in text
    assert "proběhla" not in text and "Monotonicita" in text


def test_old_future_or_missing_computation_time_is_visible():
    for ts in (NOW-90000, NOW+1, None, True):
        snap = snapshot()
        snap["calibration"]["calibrated_at"] = ts
        text, status = report.build_calibration_report(snap, None, (True, "fixture"), NOW)
        assert status == 2
        assert "STARÁ KALIBRACE" in text or "Čas a stáří výpočtu: NEOVĚŘENO" in text


def test_risk_file_strict_schema_and_freshness(tmp_path):
    path = tmp_path/"risk.json"
    data = calibration()
    data["calibration_generation"] = data.pop("generation")
    with patch.object(report, "RISK_STATE_FILE", path):
        for content in ('{"max_aggregate_exposure": 0}', '{"x": NaN}', '{"x":1,"x":2}', 'garbage'):
            path.write_text(content)
            assert report.check_risk_state_file(NOW)[0] is False
        path.write_text(json.dumps(data))
        assert report.check_risk_state_file(NOW)[0] is True
        assert report.check_risk_state_file(NOW+90000)[0] is False
        data["p_ruin_1y"]["computed_at"] += 1
        path.write_text(json.dumps(data))
        assert report.check_risk_state_file(NOW)[0] is False


def test_baseline_persistence_is_atomic_and_dry_run_has_no_write(tmp_path):
    path = tmp_path/"last.json"
    with patch.object(report, "LAST_STATE_FILE", path):
        assert report.save_current_state(calibration(), NOW)
        assert json.loads(path.read_text())["observed_at"] == NOW
    with patch.object(report, "get_snapshot", return_value=snapshot()), patch.object(
            report, "load_last_state", return_value=None), patch.object(report, "check_risk_state_file",
            return_value=(True, "fixture")), patch.object(report.time, "time", return_value=NOW), patch.object(
            report, "save_current_state", side_effect=AssertionError("write")), patch.object(
            report, "send_telegram", side_effect=AssertionError("network")):
        assert report.main(["--dry-run"]) == 0


def test_delivery_success_does_not_hide_observation_failure():
    with patch.object(report, "daily_observation", return_value=("fixture", 2)), patch.object(
            report, "load_env", return_value=dict(TELEGRAM_BOT_TOKEN="fake", TELEGRAM_CHAT_ID="fake")), patch.object(
            report, "send_telegram", return_value=True), patch("sys.stdout", new_callable=io.StringIO) as out:
        assert report.main(["--daily-audit"]) == 2
        assert "OBSERVATION_STATUS=2; DELIVERY=OK" in out.getvalue()
    with patch.object(report, "daily_observation", return_value=("fixture", 0)), patch.object(
            report, "load_env", return_value=dict(TELEGRAM_BOT_TOKEN="fake", TELEGRAM_CHAT_ID="fake")), patch.object(
            report, "send_telegram", return_value=False):
        assert report.main(["--daily-audit"]) == 1


def test_daily_is_read_only_and_preserves_canonical_failure():
    completed = subprocess.CompletedProcess(["systemctl"], 0, "active\n", "")
    with patch("urllib.request.urlopen", side_effect=AssertionError("unexpected network in read-only fixture")), patch.object(
            report.subprocess, "run", return_value=completed) as run, patch.object(
            pirana_report, "generate_report_data", return_value={"accounting": {"status": "incomplete"}, "runtime": {"system_mode": "Active"}}), patch.object(
            pirana_report, "format_text_report", return_value="CANONICAL NEOVĚŘENO"):
        text, status = report.daily_observation()
        assert status == 2 and "CANONICAL NEOVĚŘENO" in text
        assert run.call_args.args[0] == ["systemctl", "is-active", "pirana.service"]
    script = Path("scripts/daily_check.sh").read_text()
    assert "exec python3" in script and "--daily-audit" in script
    assert "sync_ai_trader_strategy.py" not in script
    assert "timeout -k" not in script


def test_telegram_api_error_body_and_url_are_not_disclosed():
    class Response:
        status = 200
        def __enter__(self): return self
        def __exit__(self, *args): pass
        def read(self, limit): return b'{"ok":false}'
    with patch.object(report.urllib.request, "urlopen", return_value=Response()):
        assert report.send_telegram("fake", "fake", "<report>") is False
    with patch.object(report.urllib.request, "urlopen", side_effect=OSError("SECRET URL")), patch(
            "sys.stderr", new_callable=io.StringIO) as out:
        assert report.send_telegram("fake", "fake", "fixture") is False
        assert "SECRET" not in out.getvalue()
