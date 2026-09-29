#!/usr/bin/env python3
"""Monthly evidence-first research hypothesis; no fabricated performance."""

import os
import sys
import json
import argparse
import urllib.request
import urllib.parse
from datetime import datetime, timezone

ENV_FILE = "/home/wwwenda/workspace/pirana/.env"

def load_env():
    """Loads environment variables from .env."""
    env = {}
    if os.path.exists(ENV_FILE):
        with open(ENV_FILE, "r") as f:
            for line in f:
                line = line.strip()
                if line and not line.startswith("#") and "=" in line:
                    k, v = line.split("=", 1)
                    env[k.strip()] = v.strip().strip('"').strip("'")
    return env

def send_part(token, chat_id, text):
    """Send once; uncertain delivery is reported, never automatically retried."""
    url = f"https://api.telegram.org/bot{token}/sendMessage"
    payload = {
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
        "disable_web_page_preview": True
    }
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            if resp.status == 200:
                return json.loads(resp.read(65536)).get("ok") is True
    except Exception:
        print("Telegram delivery failed or uncertain; no automatic retry", file=sys.stderr)
    return False

def send_telegram(token, chat_id, text, retries=1):
    # Compatibility argument retained but does not authorize repeated delivery.
    import html
    plain = html.unescape(text)
    for i in range(0, len(plain), 3000):
        if not send_part(token, chat_id, html.escape(plain[i:i+3000])):
            return False
    return bool(plain)


def generate_institutional_proposal_html(now=None):
    import html
    from zoneinfo import ZoneInfo
    try:
        from scripts.pirana_report import generate_report_data, format_text_report
    except ModuleNotFoundError:
        from pirana_report import generate_report_data, format_text_report
    now = now or datetime.now(timezone.utc)
    if now.tzinfo is None:
        now = now.replace(tzinfo=ZoneInfo("Europe/Prague"))
    local = now.astimezone(ZoneInfo("Europe/Prague"))
    end = local.replace(day=1, hour=0, minute=0, second=0, microsecond=0)
    start = end.replace(year=end.year-1, month=12) if end.month == 1 else end.replace(month=end.month-1)
    data = generate_report_data(include_runtime=True, now_arg=now.isoformat())
    text = "\n".join([
        "ČÁSLAV / PIRANA — měsíční výzkumný návrh",
        "Vytvořeno UTC: " + now.astimezone(timezone.utc).isoformat(),
        "Požadované období: [" + start.isoformat() + ", " + end.isoformat() + ") Europe/Prague",
        "Měsíční PnL, počet plnění, win rate, poplatky, markout a skluz: NEOVĚŘENO pro toto přesné období.",
        "Následuje aktuální kanonický přehled s vlastním rozsahem; nejde o měsíční statistiku:",
        format_text_report(data),
        "VÝZKUMNÁ HYPOTÉZA: kvalita exekuce může souviset se stářím signálu a tržním režimem.",
        "Experiment: ověřit úplnost CID benchmarků a časové pokrytí equity, spojit se skutečnými filly a poplatky, odděleně vyhodnotit BUY a SELL, uvést velikost vzorku i chybějící záznamy.",
        "Varianty porovnat offline na odděleném časovém vzorku včetně nákladů, nejistoty a rizika přeučení.",
        "Přínos, latence, úspora CPU a změna zisku: nezměřeno. Návrh není prokázané úzké hrdlo ani příslib výnosu.",
        "Změna strategie či nasazení vyžaduje samostatné schválení, testy a kontrolu."])
    return html.escape(text)

def main():
    parser = argparse.ArgumentParser(description="Čáslav Monthly Quantitative Innovation Proposal (v3.0)")
    parser.add_argument("--dry-run", action="store_true", help="Print HTML to stdout without sending to Telegram")
    parser.add_argument("--force-now", action="store_true", help="Generate and send proposal immediately to Telegram")
    args = parser.parse_args()

    env = load_env()
    token = os.environ.get("TELEGRAM_BOT_TOKEN", env.get("TELEGRAM_BOT_TOKEN"))
    chat_id = os.environ.get("TELEGRAM_CHAT_ID", env.get("TELEGRAM_CHAT_ID"))

    now = datetime.now(timezone.utc)
    html_msg = generate_institutional_proposal_html(now)

    if args.dry_run:
        print("=== [DRY RUN] INSTITUTIONAL MONTHLY QUANTITATIVE PROPOSAL ===")
        print(html_msg)
        print("=== [END DRY RUN] ===")
        return 0

    if not token or not chat_id:
        print("Telegram credentials unavailable", file=sys.stderr)
        return 1

    print(f"🚀 Generating and sending monthly innovation proposal for {now.strftime('%Y-%m')} to Telegram...")
    success = send_telegram(token, chat_id, html_msg)
    if success:
        print("✓ Monthly innovation proposal successfully delivered to Telegram.")
        return 0
    else:
        print("[ERROR] Failed to send monthly proposal to Telegram.", file=sys.stderr)
        return 1

if __name__ == "__main__":
    sys.exit(main())
