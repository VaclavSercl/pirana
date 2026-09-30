#!/usr/bin/env python3
"""Read-only calibration observations and deterministic morning report.

A generation counter is not a calibration event log. No risk changes, agents,
registry publication or exchange requests are performed by this entrypoint.
"""
import argparse
from datetime import datetime, timezone
import html
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

ENV_FILE = Path("/home/wwwenda/workspace/pirana/.env")
LAST_STATE_FILE = Path("/var/lib/pirana/last_calibration.json")
RISK_STATE_FILE = Path("/opt/caslav/risk/risk_state.json")
SNAPSHOT_URLS = ["http://127.0.0.1:8080/api/snapshot", "http://127.0.0.1:80/api/snapshot"]
FIELDS = ("max_aggregate_exposure", "max_single_trade_risk", "vpin_toxicity_threshold", "p_ruin_1y")
# Reporting freshness classification only; never an instruction to recalibrate.
FRESHNESS_SECONDS = 86400
UNKNOWN = "NEOVĚŘENO"


def strict_json(text):
    def reject(_):
        raise ValueError("non-finite JSON")
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("duplicate JSON key")
            result[key] = value
        return result
    return json.loads(text, parse_constant=reject, object_pairs_hook=unique)


def number(value, maximum=1.0):
    if type(value) not in (int, float) or not 0 <= value <= maximum or not math.isfinite(value):
        return None
    return value


def timestamp(value, now):
    return value if type(value) is int and 0 < value <= now else None


def iso(value):
    return datetime.fromtimestamp(value, timezone.utc).isoformat()


def load_env():
    keys = ("TELEGRAM_BOT_TOKEN", "TELEGRAM_CHAT_ID")
    result = {key: os.environ[key] for key in keys if os.environ.get(key)}
    if len(result) == len(keys):
        return result
    try:
        for line in ENV_FILE.read_text().splitlines():
            key, sep, value = line.partition("=")
            if sep and key.strip() in keys and key.strip() not in result:
                result[key.strip()] = value.strip().strip('"').strip("'")
    except OSError:
        pass  # Missing configuration is reported explicitly by main.
    return result


def get_snapshot():
    for url in SNAPSHOT_URLS:
        try:
            with urllib.request.urlopen(url, timeout=5) as response:
                raw = response.read(1024 * 1024 + 1)
                if len(raw) > 1024 * 1024:
                    continue
                value = strict_json(raw.decode())
                if response.status == 200 and isinstance(value, dict):
                    return value
        except (OSError, ValueError):
            continue
    return None


def load_last_state():
    try:
        value = strict_json(LAST_STATE_FILE.read_text())
        return value if isinstance(value, dict) else None
    except (OSError, ValueError):
        return None


def save_current_state(calib, now=None):
    now = int(time.time()) if now is None else now
    state = {"schema_version": 2, "observed_at": now,
             "generation": calib.get("generation"), "sample_size": calib.get("sample_size"),
             "calibrated_at": calib.get("calibrated_at")}
    temporary = None
    try:
        if LAST_STATE_FILE.is_symlink() or not LAST_STATE_FILE.parent.is_dir():
            return False
        with tempfile.NamedTemporaryFile(mode="w", dir=LAST_STATE_FILE.parent, delete=False) as out:
            temporary = Path(out.name)
            json.dump(state, out, allow_nan=False)
            out.flush()
            os.fsync(out.fileno())
        os.replace(temporary, LAST_STATE_FILE)
        return True
    except (OSError, ValueError, TypeError):
        if temporary is not None:
            temporary.unlink(missing_ok=True)
        print("Calibration baseline persistence failed", file=sys.stderr)
        return False


def check_risk_state_file(now=None):
    now = int(time.time()) if now is None else now
    try:
        with RISK_STATE_FILE.open("rb") as source:
            raw = source.read(1024 * 1024 + 1)
        if len(raw) > 1024 * 1024:
            return False, "NEOVĚŘENO: soubor překračuje limit"
        value = strict_json(raw.decode())
        if not isinstance(value, dict):
            raise ValueError("object required")
        calibrated = timestamp(value.get("calibrated_at"), now)
        generation = value.get("calibration_generation")
        if calibrated is None or type(generation) is not int or generation < 0:
            raise ValueError("invalid calibration identity")
        for key in FIELDS:
            entry = value.get(key)
            if not isinstance(entry, dict) or number(entry.get("value")) is None:
                raise ValueError("invalid parameter")
            if timestamp(entry.get("computed_at"), now) != calibrated:
                raise ValueError("parameter age differs")
            if not all(isinstance(entry.get(k), str) and entry[k].strip() for k in ("formula", "inputs")):
                raise ValueError("missing provenance")
        age = now - calibrated
        label = f"JSON a vybrané parametry validní; generace {generation}; stáří {age // 3600} h"
        if age > FRESHNESS_SECONDS:
            return False, label + "; STARÉ (více než 24 h)"
        return True, label + "; nejde o důkaz správnosti vzorců ani shody runtime"
    except (OSError, ValueError, TypeError, UnicodeError):
        return False, "NEOVĚŘENO: chybějící, nečitelný nebo neplatný JSON/schema/čas"


def build_calibration_report(snap, last, risk_result, now, *, now_ms=None):
    observed_ms = now * 1000 if now_ms is None else now_ms
    precise_clock = type(observed_ms) is int and now * 1000 <= observed_ms < (now + 1) * 1000
    calib = snap.get("calibration") if isinstance(snap, dict) else None
    if not isinstance(calib, dict):
        return "KALIBRAČNÍ POZOROVÁNÍ — NEOVĚŘENO: API/kalibrace nedostupná", 2
    gen = calib.get("generation")
    sample = calib.get("sample_size")
    valid_counts = all(type(v) is int and 0 <= v <= 10**12 for v in (gen, sample))
    calibrated = timestamp(calib.get("calibrated_at"), now)
    age = now - calibrated if calibrated is not None else None
    lines = ["KALIBRAČNÍ POZOROVÁNÍ", "Pozorováno UTC: " + iso(now),
             "Generace v runtime: " + (str(gen) if valid_counts else UNKNOWN),
             "Vzorků v runtime: " + (str(sample) if valid_counts else UNKNOWN)]
    uptime = number(snap.get("uptime_seconds"), 10**12)
    if calibrated is not None:
        lines.append(f"Výpočet UTC: {iso(calibrated)}; stáří {age // 3600} h {age % 3600 // 60} min")
        if uptime is not None and calibrated < now - uptime:
            lines.append("Generace předchází startu procesu: obnovený historický stav, nikoli nový výpočet.")
        if age > FRESHNESS_SECONDS:
            lines.append("STARÁ KALIBRACE: více než 24 h; frekvence pokusů není důkaz nového výpočtu.")
    else:
        lines.append("Čas a stáří výpočtu: " + UNKNOWN)
    evidence = calib.get("evidence")
    evidence_healthy = False
    if isinstance(evidence, dict):
        state = evidence.get("status")
        counts = [evidence.get(k) for k in ("roundtrip_count", "complete_day_count")]
        times = [evidence.get(k) for k in ("generated_at_ms", "sync_cursor_ms")]
        valid_evidence = (precise_clock and type(evidence.get("schema_version")) is int
            and evidence["schema_version"] == 1
            and evidence.get("source") == "authenticated_strategy_position_roundtrips"
            and state in ("READY", "WARMUP", "BLOCKED")
            and all(type(v) is int and 0 <= v <= limit for v, limit in zip(counts, (1000, 365)))
            and all(type(v) is int and 0 < v <= observed_ms and observed_ms - v <= 120000 for v in times)
            and times[1] <= times[0])
        evidence_healthy = valid_evidence and (state == "WARMUP" or (state == "READY" and counts[0] >= 50 and counts[1] >= 5))
        lines.append("Podklady kalibrace: " + (state if valid_evidence else UNKNOWN))
        if valid_evidence:
            lines.append(f"Ověřené roundtripy / úplné dny: {counts[0]} / {counts[1]}")
            if state == "WARMUP":
                lines.append("Čekání na dostatek skutečných dat; samo o sobě neznamená zastavené obchodování.")
        reasons = evidence.get("reasons", [])
        if isinstance(reasons, list) and len(reasons) <= 1000 and all(isinstance(x, str) and len(x) <= 512 for x in reasons):
            lines.append("Důvod: " + "; ".join(reasons[:20]))
    else:
        lines.append("Podklady kalibrace: NEOVĚŘENO — chybí původ a úplnost vzorku.")
    old_time = timestamp(last.get("observed_at"), now) if isinstance(last, dict) else None
    if old_time is not None:
        lines.append(f"Interval pozorování: {iso(old_time)} až {iso(now)} ({now - old_time} s)")
    lines.append("Počet rekalibrací a změna za 24 h: NEOVĚŘENO — chybí úplný deník událostí v okně.")
    valid_values = True
    for key in FIELDS:
        entry = calib.get(key)
        value = number(entry.get("value")) if isinstance(entry, dict) else None
        computed = timestamp(entry.get("computed_at"), now) if isinstance(entry, dict) else None
        valid = value is not None and computed is not None and computed == calibrated
        valid_values &= valid
        lines.append(key + ": " + (format(value, ".8g") if valid else UNKNOWN))
    lines.extend(["P(ruin) je modelový bodový odhad, ne naměřená pravděpodobnost ani bezpečnostní certifikát.",
                  "Monotonicita P(ruin) vůči expozici: NEOVĚŘENO — jeden skalár ji nedokazuje.",
                  "risk_state.json: " + risk_result[1],
                  "PŮVOD DAT: místní runtime API a disk; žádná změna parametrů."])
    healthy = evidence_healthy and valid_counts and valid_values and age is not None and age <= FRESHNESS_SECONDS and risk_result[0]
    return "\n".join(lines), 0 if healthy else 2


def send_telegram(token, chat_id, text):
    # Never print exception strings: urllib errors may contain the credential URL.
    request = urllib.request.Request(f"https://api.telegram.org/bot{token}/sendMessage",
        data=urllib.parse.urlencode({"chat_id": chat_id, "text": html.escape(text),
                                   "parse_mode": "HTML", "disable_web_page_preview": True}).encode())
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            result = strict_json(response.read(65537).decode())
            return response.status == 200 and isinstance(result, dict) and result.get("ok") is True
    except (OSError, ValueError):
        print("Telegram delivery failed", file=sys.stderr)
        return False


def daily_observation():
    try:
        from scripts.pirana_report import generate_report_data, format_text_report
    except ImportError:
        from pirana_report import generate_report_data, format_text_report
    status = 0
    try:
        result = subprocess.run(["systemctl", "is-active", "pirana.service"],
                                capture_output=True, text=True, timeout=10, check=False)
        active = result.returncode == 0 and result.stdout.strip() == "active"
        service = "active (samo nepotvrzuje obchodování)" if active else "NEOVĚŘENO / není active"
        if not active:
            status = 2
    except (OSError, subprocess.TimeoutExpired):
        service, status = "NEOVĚŘENO: kontrola služby selhala", 2
    try:
        data = generate_report_data(include_runtime=True)
        financial = format_text_report(data)
        accounting = data.get("accounting", {})
        runtime = data.get("runtime") or {}
        if (accounting.get("status") != "complete" or not runtime
                or runtime.get("execution_block_reason")
                or runtime.get("market_data_available") is not True):
            status = 2
    except Exception:
        # Preserve failure as status, not raw diagnostics containing private data.
        financial, status = "Finanční report NEOVĚŘENO: sestavení selhalo.", 2
    text = (financial + "\n\nPROVOZNÍ POZOROVÁNÍ (pouze čtení)\nSlužba: " + service
            + "\nNejde o kompletní audit strategie ani důkaz zisku. Žádné zásahy, změny rizika ani publikace registru.")
    return text, status


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--daily-audit", action="store_true")
    parser.add_argument("--delivery-status", action="store_true",
                        help="For report delivery units only: exit reflects delivery/persistence; observation status remains in report and journal")
    args = parser.parse_args(argv)
    now = int(time.time())
    snap = None
    if args.daily_audit:
        text, status = daily_observation()
    else:
        snap = get_snapshot()
        observed_ms = int(time.time() * 1000)  # sampled after API fetch, no whole-second truncation
        now = observed_ms // 1000
        text, status = build_calibration_report(snap, load_last_state(), check_risk_state_file(now), now, now_ms=observed_ms)
    if args.dry_run:
        print(text)
        print(f"OBSERVATION_STATUS={status}; DELIVERY=NOT_REQUESTED")
        return status
    env = load_env()
    token, chat_id = env.get("TELEGRAM_BOT_TOKEN"), env.get("TELEGRAM_CHAT_ID")
    if not token or not chat_id:
        print("Telegram configuration missing; observation_status=" + str(status), file=sys.stderr)
        return 1
    # Split at safe plain-text boundaries before HTML escaping. Preserve all evidence.
    delivered = all(send_telegram(token, chat_id, text[i:i+3500]) for i in range(0, len(text), 3500))
    saved = True
    if snap is not None and isinstance(snap.get("calibration"), dict):
        saved = save_current_state(snap["calibration"], now)
    print(f"OBSERVATION_STATUS={status}; DELIVERY={'OK' if delivered else 'FAILED'}; BASELINE={'OK' if saved else 'FAILED'}")
    if not delivered:
        return 1
    if not saved:
        return 2
    # Only delivery-unit callers select this contract. Default and dry-run remain
    # health-sensitive; successful notification is not a healthy trading verdict.
    return 0 if args.delivery_status else status


if __name__ == "__main__":
    sys.exit(main())
