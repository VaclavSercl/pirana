"""Offline report contracts: no exchange, Telegram, or model calls."""
from datetime import datetime, timezone
import html
import io
import subprocess
from unittest.mock import patch
from scripts import pirana_report as canonical
from scripts import send_audit_report as audit
from scripts import send_monthly_proposal as proposal


def fixture():
    return dict(accounting=canonical.canonical_unavailable("fixture"), snapshot_source="fixture", report_generated_at_ms=1)


def test_audit_uses_current_canonical_data_and_explicit_limits():
    with patch.object(canonical,"generate_report_data",return_value=fixture()) as generate, patch.object(audit,"runtime_evidence",return_value="HEAD fixture"):
        text=audit.build_report()
    generate.assert_called_once()
    assert generate.call_args.kwargs["include_runtime"] is True
    assert "NEOVĚŘENO" in text and "HEAD fixture" in text
    assert "23. 08. 2026" not in text and "59/59" not in text
    assert "Testy a audit kódu nebyly" in text


def test_probe_timeout_is_unknown_not_success():
    with patch.object(audit.subprocess,"run",side_effect=subprocess.TimeoutExpired("fixture",8)):
        assert audit.probe(["fixture"]) is None
        text=audit.runtime_evidence()
    assert "NEOVĚŘENO" in text


def test_process_movement_invalidates_binary_digest():
    first="MainPID=123\nActiveState=active\nNRestarts=0\nExecMainStartTimestamp=before"
    with patch.object(audit,"probe",side_effect=["a"*40,first,"b"*64+" /proc/123/exe",first.replace("123","456")]):
        text=audit.runtime_evidence()
    assert "Binárka SHA-256: NEOVĚŘENO" in text
    assert "shoda sestavení není doložena" in text


def test_monthly_does_not_invent_returns_or_mislabel_current_scope():
    with patch.object(canonical,"generate_report_data",return_value=fixture()):
        text=html.unescape(proposal.generate_institutional_proposal_html(datetime(2027,1,1,tzinfo=timezone.utc)))
    assert "2026-12-01T00:00:00+01:00" in text and "2027-01-01T00:00:00+01:00" in text
    assert "nejde o měsíční statistiku" in text
    assert "VÝZKUMNÁ HYPOTÉZA" in text and "nezměřeno" in text
    for fabricated in ("47.13", "-4.37", "+35 %", "--yolo", "100% Zero-Fee"):
        assert fabricated not in text


def test_dry_runs_need_no_credentials_or_delivery():
    for module, generator in ((audit,"build_report"),(proposal,"generate_institutional_proposal_html")):
        with patch.object(module,generator,return_value="fixture"),patch.object(module,"load_env",return_value={}),patch.object(module.urllib.request,"urlopen",side_effect=AssertionError("network")),patch("sys.argv",["report","--dry-run"]),patch("sys.stdout",new_callable=io.StringIO):
            assert module.main()==0


def test_chunks_preserve_html_text_and_monthly_failure_not_retried():
    original="<unsafe>&"*1000
    pieces=audit.chunks(original)
    assert "".join(html.unescape(x) for x in pieces)==original
    assert all(len(html.unescape(x))<=3000 for x in pieces)
    with patch.object(proposal,"send_part",return_value=False) as send:
        assert proposal.send_telegram("fixture","1",html.escape(original)) is False
    assert send.call_count==1


def test_weekly_delivery_failure_is_nonzero_and_does_not_leak_url():
    with patch.object(audit,"build_report",return_value="fixture"),patch.object(audit,"load_env",return_value={"TELEGRAM_BOT_TOKEN":"fixture","TELEGRAM_CHAT_ID":"1"}),patch.dict(audit.os.environ,{},clear=True),patch.object(audit,"send",side_effect=OSError("secret URL")),patch("sys.argv",["audit"]),patch("sys.stderr",new_callable=io.StringIO) as err:
        assert audit.main()==1
    assert "secret URL" not in err.getvalue()


def test_audit_plain_format_is_escaped_once_without_generated_markup():
    with patch.object(canonical,"generate_report_data",return_value=fixture()),patch.object(canonical,"format_text_report",return_value="Plain BTC & USD"),patch.object(canonical,"format_telegram_html",side_effect=AssertionError("HTML formatter must not be used")),patch.object(audit,"runtime_evidence",return_value="HEAD fixture"):
        text=audit.build_report()
    rendered="".join(html.unescape(x) for x in audit.chunks(text))
    assert rendered==text
    assert "Plain BTC & USD" in rendered
    assert "<b>" not in rendered and "<code>" not in rendered
    literal="literal &lt;not-markup&gt;"
    assert html.unescape(audit.chunks(literal)[0])==literal


def test_monthly_compatibility_retries_never_repeat_failed_send():
    with patch.object(proposal,"send_part",return_value=False) as send:
        assert proposal.send_telegram("fixture","1","fixture",retries=99) is False
    assert send.call_count==1
