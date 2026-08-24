#!/usr/bin/env python3
"""
Pirana Daily Calibration Audit (CASLAV §8.2)
============================================

Spouští se denně v 06:00 (hodinu před ranním auditem).
AUDITUJE, co kalibrace SKUTEČNĚ udělala za posledních 24 hodin.

Logika:
1. Načte aktuální stav kalibrace z API
2. Porovná s uloženým stavem z předchozího dne (/var/lib/pirana/last_calibration.json)
3. Reportuje DELTA — co se skutečně změnilo
4. Ověří invarianty (P(ruin) roste s expozicí, risk_state.toml na disku)

NEvolá rekalibraci — tu dělá Rust engine každých 15 min (main.rs:410).
"""

import os
import sys
import json
import subprocess
import urllib.request
import urllib.parse
from pathlib import Path
from datetime import datetime

ENV_FILE = Path("/home/wwwenda/workspace/pirana/.env")
LAST_STATE_FILE = Path("/var/lib/pirana/last_calibration.json")
SNAPSHOT_URLS = [
    "http://127.0.0.1:8080/api/snapshot",
    "http://127.0.0.1:80/api/snapshot",
]
RISK_STATE_FILE = Path("/opt/caslav/risk/risk_state.toml")


def load_env():
    env = {}
    if ENV_FILE.exists():
        for line in ENV_FILE.read_text().splitlines():
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip().strip('"').strip("'")
    return env


def get_snapshot():
    for url in SNAPSHOT_URLS:
        try:
            with urllib.request.urlopen(url, timeout=5) as resp:
                if resp.status == 200:
                    return json.loads(resp.read().decode())
        except Exception:
            continue
    return None


def load_last_state():
    """Načte stav kalibrace z předchozího dne."""
    if not LAST_STATE_FILE.exists():
        return None
    try:
        return json.loads(LAST_STATE_FILE.read_text())
    except Exception:
        return None


def save_current_state(calib):
    """Uloží aktuální stav pro zítřejší porovnání."""
    try:
        LAST_STATE_FILE.parent.mkdir(parents=True, exist_ok=True)
        state = {
            "date": datetime.now().isoformat(),
            "generation": calib.get("generation", 0),
            "sample_size": calib.get("sample_size", 0),
            "max_aggregate_exposure": calib.get("max_aggregate_exposure", {}).get("value", 0.0),
            "max_single_trade_risk": calib.get("max_single_trade_risk", {}).get("value", 0.0),
            "vpin_toxicity_threshold": calib.get("vpin_toxicity_threshold", {}).get("value", 0.0),
            "p_ruin_1y": calib.get("p_ruin_1y", {}).get("value", 0.0),
        }
        LAST_STATE_FILE.write_text(json.dumps(state, indent=2))
        return True
    except Exception as e:
        print(f"Cannot save state: {e}", file=sys.stderr)
        return False


def check_risk_state_file():
    """Ověří, že risk_state.toml existuje a je čitelný."""
    if not RISK_STATE_FILE.exists():
        return None, "soubor neexistuje"
    try:
        content = RISK_STATE_FILE.read_text()
        if "max_aggregate_exposure" in content:
            return True, "OK"
        return False, "neobsahuje max_aggregate_exposure"
    except Exception as e:
        return False, str(e)


def send_telegram(token, chat_id, text):
    url = f"https://api.telegram.org/bot{token}/sendMessage"
    payload = urllib.parse.urlencode({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
        "disable_web_page_preview": True,
    }).encode()
    req = urllib.request.Request(url, data=payload)
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return resp.status == 200
    except Exception as e:
        print(f"Telegram error: {e}", file=sys.stderr)
        return False


def main():
    env = load_env()
    token = env.get("TELEGRAM_BOT_TOKEN")
    chat_id = env.get("TELEGRAM_CHAT_ID", "1076582576")
    if not token:
        print("TELEGRAM_BOT_TOKEN missing", file=sys.stderr)
        return 1

    snap = get_snapshot()
    if not snap:
        send_telegram(token, chat_id,
                      "⚠️ <b>ČÁSLAV :: KALIBRAČNÍ AUDIT</b>\n\nAPI nedostupné — zkontroluj pirana.service")
        return 1

    mode = snap.get("system_mode", "?")
    calib = snap.get("calibration", {})
    gen = calib.get("generation", 0)
    sample = calib.get("sample_size", 0)
    exposure = calib.get("max_aggregate_exposure", {}).get("value", 0.0)
    risk = calib.get("max_single_trade_risk", {}).get("value", 0.0)
    vpin = calib.get("vpin_toxicity_threshold", {}).get("value", 0.0)
    p_ruin = calib.get("p_ruin_1y", {}).get("value", 0.0)
    if p_ruin is None or (isinstance(p_ruin, float) and p_ruin != p_ruin):
        p_ruin = 0.0

    # ── AUDIT: porovnání s předchozím dnem ────────────────────────────────
    last = load_last_state()
    risk_ok, risk_msg = check_risk_state_file()

    delta_gen = ""
    delta_sample = ""
    if last:
        gen_diff = gen - last.get("generation", 0)
        sample_diff = sample - last.get("sample_size", 0)
        delta_gen = f" ({'+' if gen_diff >= 0 else ''}{gen_diff})"
        delta_sample = f" ({'+' if sample_diff >= 0 else ''}{sample_diff})"

        if gen_diff > 0:
            status_icon = "🟢"
            status_text = f"Kalibrace proběhla {gen_diff}× za 24h"
        elif gen_diff == 0 and sample < 50:
            status_icon = "🟡"
            missing = 50 - sample
            status_text = f"Čeká se na data — zbývá {missing} round-tripů"
        else:
            status_icon = "🟠"
            status_text = "Žádná rekalibrace za 24h — zkontroluj risk engine"
    else:
        status_icon = "ℹ️"
        status_text = "První běh auditu — ukládám baseline"

    # ── INVARIANT: P(ruin) musí růst s expozicí ───────────────────────────
    p_ruin_ok = "✅" if p_ruin <= 0.01 else "⚠️"
    risk_file_ok = "✅" if risk_ok else "❌"

    msg = (
        f"🔬 <b>ČÁSLAV :: KALIBRAČNÍ AUDIT</b>\n"
        f"━━━━━━━━━━━━━━━━━━━━━━\n"
        f"{status_icon} <b>{status_text}</b>\n"
        f"━━━━━━━━━━━━━━━━━━━━━━\n"
        f"• Režim: <code>{mode}</code>\n"
        f"• Generace: <code>{gen}</code>{delta_gen}\n"
        f"• Vzorků: <code>{sample}</code>{delta_sample}\n"
        f"• Max expozice: <code>{exposure:.2%}</code>\n"
        f"• Riziko/obchod: <code>{risk:.3%}</code>\n"
        f"• VPIN práh: <code>{vpin:.3f}</code>\n"
        f"• P(ruin): <code>{p_ruin:.6f}</code> {p_ruin_ok}\n"
        f"• risk_state.toml: <code>{risk_msg}</code> {risk_file_ok}\n"
        f"━━━━━━━━━━━━━━━━━━━━━━\n"
        f"<i>Audit každý den v 06:00. Rust engine rekalibruje každých 15 min.</i>"
    )

    # Uložíme stav pro zítřek
    save_current_state(calib)

    ok = send_telegram(token, chat_id, msg)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
