#!/usr/bin/env python3
"""
Pirana Daily Light Recalibration Report (CASLAV §8.2)
=====================================================

Spouští se denně v 06:00 (hodinu před ranním auditem).
Provádí lehkou rekalibraci risk parametrů z uzavřených round-tripů
a odesílá výsledek na Telegram.
"""

import os
import sys
import json
import subprocess
import urllib.request
import urllib.parse
from pathlib import Path

ENV_FILE = Path("/home/wwwenda/workspace/pirana/.env")
SNAPSHOT_URLS = [
    "http://127.0.0.1:8080/api/snapshot",
    "http://127.0.0.1:80/api/snapshot",
]


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
                      "⚠️ <b>ČÁSLAV :: REKALIBRACE</b>\n\nAPI nedostupné — zkontroluj pirana.service")
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

    msg = (
        "🔬 <b>ČÁSLAV :: LEHKÁ REKALIBRACE</b>\n"
        "━━━━━━━━━━━━━━━━━━━━━━\n"
        f"• Režim: <code>{mode}</code>\n"
        f"• Generace kalibrace: <code>{gen}</code>\n"
        f"• Vzorků (round-trips): <code>{sample}</code>\n"
        f"• Max expozice: <code>{exposure:.2%}</code>\n"
        f"• Riziko/obchod: <code>{risk:.3%}</code>\n"
        f"• VPIN práh: <code>{vpin:.3f}</code>\n"
        f"• P(ruin): <code>{p_ruin:.6f}</code>\n"
        "━━━━━━━━━━━━━━━━━━━━━━\n"
        "<i>Lehká rekalibrace každý den v 06:00. Plná rekalibrace v pondělí 06:00.</i>"
    )

    ok = send_telegram(token, chat_id, msg)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
