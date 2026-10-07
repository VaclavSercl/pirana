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
    data = dict(generation=48, sample_size=1000, calibrated_at=NOW-100,
                evidence=dict(schema_version=1, source="authenticated_strategy_position_roundtrips", status="READY",
                    roundtrip_count=50, complete_day_count=5, generated_at_ms=NOW*1000, sync_cursor_ms=NOW*1000, reasons=[]))
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


def test_calibration_evidence_cannot_claim_readiness_from_old_or_missing_data():
    for change in (None, {}, dict(status="READY"), dict(generated_at_ms=(NOW-121)*1000),
                   dict(sync_cursor_ms=(NOW+1)*1000), dict(roundtrip_count=49),
                   dict(complete_day_count=4), dict(source="legacy_runtime"), dict(schema_version=True)):
        snap = snapshot()
        if change is None: snap["calibration"].pop("evidence")
        elif change == {}: snap["calibration"]["evidence"] = {}
        elif change == dict(status="READY"): snap["calibration"]["evidence"] = change
        else: snap["calibration"]["evidence"].update(change)
        _, status = report.build_calibration_report(snap, None, (True, "fixture"), NOW)
        assert status == 2
    _, status = report.build_calibration_report(snapshot(), None, (True, "fixture"), NOW)
    assert status == 0


def test_calibration_warmup_is_explicit_without_claiming_trading_stopped():
    snap = snapshot()
    snap["calibration"]["evidence"].update(status="WARMUP", complete_day_count=0, reasons=["missing_daily_equity"])
    text, status = report.build_calibration_report(snap, None, (True, "fixture"), NOW)
    assert status == 0 and "WARMUP" in text and "missing_daily_equity" in text
    assert "samo o sobě neznamená zastavené obchodování" in text


def test_subsecond_evidence_uses_precise_postfetch_time_without_future_tolerance():
    snap = snapshot()
    snap["calibration"]["evidence"].update(generated_at_ms=NOW*1000+300, sync_cursor_ms=NOW*1000+299)
    assert report.build_calibration_report(snap, None, (True, "fixture"), NOW, now_ms=NOW*1000+400)[1] == 0
    assert report.build_calibration_report(snap, None, (True, "fixture"), NOW, now_ms=NOW*1000+200)[1] == 2
    assert report.build_calibration_report(snap, None, (True, "fixture"), NOW, now_ms=NOW*1000+1000)[1] == 2


def test_delivery_unit_contract_keeps_observation_and_all_delivery_failures_visible():
    with patch.object(report, "get_snapshot", return_value=snapshot()), patch.object(
            report, "load_last_state", return_value=None), patch.object(report, "check_risk_state_file", return_value=(False, "stale")), patch.object(
            report, "load_env", return_value=dict(TELEGRAM_BOT_TOKEN="fake", TELEGRAM_CHAT_ID="fake")), patch.object(
            report, "send_telegram", return_value=True) as send, patch.object(report, "save_current_state", return_value=True) as save, patch(
            "sys.stdout", new_callable=io.StringIO) as out:
        assert report.main(["--delivery-status"]) == 0
        assert "OBSERVATION_STATUS=2; DELIVERY=OK; BASELINE=OK" in out.getvalue()
        assert "NEOVĚŘENO" in send.call_args.args[2] or "STAR" in send.call_args.args[2]
        save.return_value = False
        assert report.main(["--delivery-status"]) == 2
        save.return_value = True
        send.return_value = False
        assert report.main(["--delivery-status"]) == 1


def test_delivery_flag_never_turns_dry_run_health_check_green():
    with patch.object(report, "daily_observation", return_value=("NEOVĚŘENO", 2)), patch.object(
            report, "send_telegram", side_effect=AssertionError("must not send")):
        assert report.main(["--daily-audit", "--dry-run", "--delivery-status"]) == 2
    unit = Path("deploy/systemd/pirana-recalib.service").read_text()
    assert "--delivery-status" in unit
    # The owner removed the shared failure sender; delivery status stays checked.
    # Vlastník odstranil společný sender selhání; stav doručení se dále kontroluje.
    assert "OnFailure=notify-telegram-failure@%n.service" not in unit


def test_daily_installed_delivery_arguments_preserve_health_and_delivery_status():
    import shlex
    unit = Path("deploy/systemd/pirana-daily-check.service").read_text()
    configured = shlex.split(next(line.split("=", 1)[1] for line in unit.splitlines()
                                 if line.startswith("ExecStart=")))
    assert configured[0].endswith("/scripts/daily_check.sh")
    args = ["--daily-audit", *configured[1:]]
    with patch.object(report, "daily_observation", return_value=("NEOVĚŘENO: WARMUP", 2)), patch.object(
            report, "load_env", return_value=dict(TELEGRAM_BOT_TOKEN="fake", TELEGRAM_CHAT_ID="fake")), patch.object(
            report, "send_telegram", return_value=True) as send, patch("sys.stdout", new_callable=io.StringIO) as output:
        assert report.main(args) == 0
        assert "OBSERVATION_STATUS=2; DELIVERY=OK" in output.getvalue()
        assert "NEOVĚŘENO" in send.call_args.args[2]
        send.return_value = False
        assert report.main(args) == 1
        send.reset_mock()
        assert report.main([*args, "--dry-run"]) == 2
        send.assert_not_called()
