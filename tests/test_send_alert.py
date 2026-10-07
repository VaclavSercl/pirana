"""Owner-approved decommissioning: no legacy sender or doctor reactivation.
Vlastníkem schválené odstranění: žádné obnovení původního odesílače ani doctoru.
"""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_shared_failure_sender_is_removed():
    assert not (ROOT / "scripts/send_alert.py").exists()
    assert not (ROOT / "deploy/systemd/notify-telegram-failure@.service").exists()


def test_deploy_graph_cannot_resurrect_removed_failure_sender():
    for service in (ROOT / "deploy/systemd").glob("*.service"):
        text = service.read_text(encoding="utf-8")
        assert "scripts/send_alert.py" not in text
        assert "OnFailure=notify-telegram-failure@" not in text


def test_ten_minute_doctor_is_not_a_deployable_unit():
    assert not (ROOT / "deploy/systemd/caslav-doctor.timer").exists()
    assert not (ROOT / "deploy/systemd/caslav-doctor.service").exists()
