#!/usr/bin/env python3
"""
Čáslav :: Instant Failure Alert Dispatcher for Systemd Units
Sends real-time crash notifications with diagnostic journalctl context to Telegram.
"""

import os
import sys
import html
import subprocess
import urllib.request
import urllib.parse
from datetime import datetime

ENV_FILE = "/home/wwwenda/workspace/pirana/.env"

def load_env():
    env = {}
    if os.path.exists(ENV_FILE):
        with open(ENV_FILE, "r") as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    k, v = line.split("=", 1)
                    env[k.strip()] = v.strip().strip('"').strip("'")
    return env

def get_journal_snippet(unit_name):
    try:
        cmd = ["journalctl", "-u", unit_name, "-n", "15", "--no-pager"]
        res = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=5)
        if res.returncode == 0 and res.stdout.strip():
            return res.stdout.strip()
    except Exception as e:
        return f"Nepodařilo se získat logy: {e}"
    return "Žádné záznamy v journalctl."

def send_alert():
    env = load_env()
    token = os.environ["TELEGRAM_BOT_TOKEN"]
    chat_id = os.environ["TELEGRAM_CHAT_ID"]

    unit = sys.argv[1] if len(sys.argv) > 1 else "unknown.service"
    now_str = datetime.now().strftime("%Y-%m-%d %H:%M:%S CEST")
    logs = get_journal_snippet(unit)
    
    # Escapování pro HTML
    escaped_unit = html.escape(unit)
    escaped_logs = html.escape(logs)
    
    # Omezení délky logů pro Telegram (max 2500 znaků v pre)
    if len(escaped_logs) > 2500:
        escaped_logs = escaped_logs[-2500:]

    message = (
        f"🚨 <b>KRITICKÁ CHYBA: Služba <code>{escaped_unit}</code> spadla!</b>\n"
        f"📅 <code>{now_str}</code>\n"
        f"🖥️ <b>Hostitel:</b> <code>Server Čáslav</code>\n"
        f"──────────────────────────\n"
        f"📋 <b>Poslední záznamy z journalctl:</b>\n"
        f"<pre>{escaped_logs}</pre>\n"
        f"──────────────────────────\n"
        f"⚡ <i>Systemd provádí automatický restart podle restartovací politiky...</i>"
    )

    url = f"https://api.telegram.org/bot{token}/sendMessage"
    payload = urllib.parse.urlencode({
        "chat_id": chat_id,
        "text": message,
        "parse_mode": "HTML"
    }).encode("utf-8")

    try:
        req = urllib.request.Request(url, data=payload, method="POST")
        with urllib.request.urlopen(req, timeout=10) as resp:
            if resp.status == 200:
                print(f"[OK] Alert pro {unit} úspěšně odeslán na Telegram.")
                return 0
    except Exception as e:
        print(f"[ERROR] Odeslání alertu selhalo: {e}", file=sys.stderr)
        # Fallback odeslání čistého textu
        try:
            fallback_text = f"🚨 KRITICKÁ CHYBA: Služba {unit} na serveru Čáslav spadla v {now_str}!\nLogy:\n{logs[-500:]}"
            fb_payload = urllib.parse.urlencode({
                "chat_id": chat_id,
                "text": fallback_text
            }).encode("utf-8")
            fb_req = urllib.request.Request(url, data=fb_payload, method="POST")
            urllib.request.urlopen(fb_req, timeout=10)
        except Exception:
            pass
        return 1
    return 0

if __name__ == "__main__":
    sys.exit(send_alert())
