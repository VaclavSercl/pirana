#!/usr/bin/env python3
"""Čáslav :: failure alert dispatcher for systemd units."""

import html
import json
import os
import subprocess
import sys
import urllib.parse
import urllib.request
from datetime import datetime
from zoneinfo import ZoneInfo

ENV_FILE = "/home/wwwenda/workspace/pirana/.env"


def load_env():
    env = {}
    if os.path.exists(ENV_FILE):
        with open(ENV_FILE, "r", encoding="utf-8") as stream:
            for line in stream:
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    key, value = line.split("=", 1)
                    env[key.strip()] = value.strip().strip('"').strip("'")
    return env


def get_journal_snippet(unit_name):
    try:
        result = subprocess.run(
            ["journalctl", "-u", unit_name, "-n", "15", "--no-pager"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=5,
        )
        if result.returncode == 0 and result.stdout.strip():
            return result.stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return "Nepodařilo se získat logy; podrobnosti zůstávají lokálně."
    return "Žádné záznamy v journalctl."


def send_alert():
    # systemd supplies credentials via EnvironmentFile while deliberately
    # hiding the repository .env. Do not access that fallback unnecessarily.
    token = os.environ.get("TELEGRAM_BOT_TOKEN")
    chat_id = os.environ.get("TELEGRAM_CHAT_ID")
    if not token or not chat_id:
        try:
            file_env = load_env()
        except (OSError, UnicodeError):
            print("[ERROR] Optional credential fallback is inaccessible", file=sys.stderr)
            return 2
        token = token or file_env.get("TELEGRAM_BOT_TOKEN")
        chat_id = chat_id or file_env.get("TELEGRAM_CHAT_ID")
    if not token or not chat_id:
        print("[ERROR] TELEGRAM_BOT_TOKEN/TELEGRAM_CHAT_ID are not configured", file=sys.stderr)
        return 2

    unit = sys.argv[1] if len(sys.argv) > 1 else "unknown.service"
    now_str = datetime.now(ZoneInfo("Europe/Prague")).strftime("%Y-%m-%d %H:%M:%S %Z")
    logs = get_journal_snippet(unit)
    escaped_unit = html.escape(unit)
    # Bound raw text before escaping so an HTML entity cannot be truncated.
    escaped_logs = html.escape(logs[-400:])

    message = (
        f"🚨 <b>KRITICKÁ CHYBA: <code>{escaped_unit}</code></b>\n"
        f"📅 <code>{now_str}</code>\n"
        "🖥️ <b>Hostitel:</b> <code>Server Čáslav</code>\n"
        "──────────────────────────\n"
        "📋 <b>Poslední záznamy z journalctl:</b>\n"
        f"<pre>{escaped_logs}</pre>\n"
        "──────────────────────────\n"
        "⚠️ <i>Ověř stav jednotky a její restartovací politiku; alert sám nepotvrzuje obnovu služby.</i>"
    )

    url = f"https://api.telegram.org/bot{token}/sendMessage"
    payload = urllib.parse.urlencode({
        "chat_id": chat_id, "text": message, "parse_mode": "HTML"
    }).encode("utf-8")

    try:
        request = urllib.request.Request(url, data=payload, method="POST")
        with urllib.request.urlopen(request, timeout=10) as response:
            raw = response.read(65537)
            if len(raw) <= 65536 and response.status == 200:
                reply = json.loads(raw)
                if isinstance(reply, dict) and reply.get("ok") is True:
                    print(f"[OK] Alert pro {unit} úspěšně odeslán.")
                    return 0
        print("[ERROR] Alert delivery was not confirmed", file=sys.stderr)
    except Exception:
        # A timeout may follow a successful send. Never retry an uncertain POST
        # or print the credential-bearing request URL / response body.
        print("[ERROR] Alert delivery failed or is uncertain; no retry attempted", file=sys.stderr)
    return 1



if __name__ == "__main__":
    sys.exit(send_alert())
