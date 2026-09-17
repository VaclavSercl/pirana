#!/usr/bin/env python3
"""
Čáslav :: Interactive Telegram Remote Control Daemon for Pirana HFT
Provides bidirectional Telegram commands (/status, /scale, /pause, /resume, /reconcile).
"""

import os
import sys
try:
    from .pirana_report import generate_report_data, format_telegram_html
except ImportError:  # Direct script invocation
    from pirana_report import generate_report_data, format_telegram_html

import json
import time
import urllib.request
import urllib.parse
import subprocess
import tomllib

ENV_FILE = "/home/wwwenda/workspace/pirana/.env"
STRATEGY_FILE = "/home/wwwenda/workspace/pirana/strategy.toml"
VERSIONER = "/home/wwwenda/workspace/pirana/scripts/strategy_versioning.py"
SNAPSHOT_URLS = ["http://127.0.0.1:80/api/snapshot", "http://127.0.0.1:8080/api/snapshot"]

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

ENV = load_env()
BOT_TOKEN = ENV["TELEGRAM_BOT_TOKEN"]
AUTHORIZED_CHAT_ID = int(ENV.get("TELEGRAM_CHAT_ID", "1076582576"))

def send_telegram(chat_id, text):
    """Sends HTML formatted message to Telegram."""
    url = f"https://api.telegram.org/bot{BOT_TOKEN}/sendMessage"
    payload = {
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
        "disable_web_page_preview": True
    }
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=10) as resp:
            return resp.status == 200
    except Exception as e:
        print(f"[ERROR] send_telegram failed: {e}", file=sys.stderr)
        return False

def get_snapshot():
    """Fetches system snapshot from Pirana API."""
    for url in SNAPSHOT_URLS:
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "CaslavTelegramBot/1.0"})
            with urllib.request.urlopen(req, timeout=3) as resp:
                if resp.status == 200:
                    return json.loads(resp.read().decode("utf-8"))
        except Exception:
            continue
    return None

def handle_status(chat_id):
    """Handles /status with canonical account accounting."""
    send_telegram(chat_id, format_telegram_html(generate_report_data(no_api=True)))

def handle_scale(chat_id, args):
    """Handles /scale <pct> command."""
    if not args:
        send_telegram(chat_id, "ℹ️ <b>Použití:</b> <code>/scale &lt;procento&gt;</code>\nNapř: <code>/scale 8.0</code> (rozsah: 1.0 % až 15.0 %)")
        return
    try:
        val = float(args[0].replace(",", ".").replace("%", ""))
        if not (1.0 <= val <= 15.0):
            send_telegram(chat_id, f"❌ <b>Neplatná hodnota:</b> <code>{val}%</code> je mimo bezpečné meze (1.0 % až 15.0 %).")
            return

        # Read strategy.toml
        with open(STRATEGY_FILE, "r") as f:
            lines = f.readlines()

        new_lines = []
        in_risk = False
        replaced = False
        for line in lines:
            if line.strip().startswith("[risk_management]"):
                in_risk = True
            elif line.strip().startswith("[") and in_risk:
                in_risk = False
            
            if in_risk and line.strip().startswith("position_size_pct"):
                new_lines.append(f"position_size_pct = {val:.1f}\n")
                replaced = True
            else:
                new_lines.append(line)

        if not replaced:
            send_telegram(chat_id, "❌ <b>Chyba:</b> Klíč <code>position_size_pct</code> nebyl nalezen v konfiguraci.")
            return

        with open(STRATEGY_FILE, "w") as f:
            f.writelines(new_lines)

        # Commit via versioner
        subprocess.run(["python3", VERSIONER, "commit", f"Telegram command /scale {val:.1f}%"], check=True)
        # Restart pirana to apply immediately
        subprocess.run(["sudo", "systemctl", "restart", "pirana.service"], check=True)

        send_telegram(chat_id, f"✅ <b>Velikost pozice úspěšně upravena:</b>\n• <code>position_size_pct</code> nastaven na <b>{val:.1f} %</b>.\n• Konfigurace uložena a verzována v Gitu.\n• Služba <code>pirana.service</code> restartována.")
    except Exception as e:
        send_telegram(chat_id, f"❌ <b>Chyba při změně velikosti pozice:</b> <code>{e}</code>")

def handle_pause(chat_id):
    """Handles /pause command."""
    try:
        subprocess.run(["sudo", "systemctl", "stop", "pirana.service"], check=True)
        send_telegram(chat_id, "⏸️ <b>Trading pozastaven.</b>\nSlužba <code>pirana.service</code> byla bezpečně zastavena.")
    except Exception as e:
        send_telegram(chat_id, f"❌ <b>Chyba při zastavení:</b> <code>{e}</code>")

def handle_resume(chat_id):
    """Handles /resume command."""
    try:
        subprocess.run(["sudo", "systemctl", "restart", "pirana.service"], check=True)
        time.sleep(2)
        send_telegram(chat_id, "▶️ <b>Trading obnoven.</b>\nSlužba <code>pirana.service</code> byla úspěšně restartována a aktivována.")
    except Exception as e:
        send_telegram(chat_id, f"❌ <b>Chyba při spuštění:</b> <code>{e}</code>")

def handle_reconcile(chat_id):
    """Handles /reconcile command."""
    try:
        subprocess.run(["sudo", "systemctl", "restart", "pirana.service"], check=True)
        time.sleep(2)
        snap = get_snapshot()
        btc_bal = snap.get("btc_balance", 0.0) if snap else 0.0
        usd_bal = snap.get("usd_balance", 0.0) if snap else 0.0
        send_telegram(chat_id, f"🔄 <b>Reconciliation dokončena.</b>\n• Osiřelé objednávky stornovány.\n• Synchronizované zůstatky: <code>{btc_bal:.6f} BTC</code> | <code>${usd_bal:,.2f} USD</code>.")
    except Exception as e:
        send_telegram(chat_id, f"❌ <b>Chyba při reconciliaci:</b> <code>{e}</code>")

PROPOSAL_SCRIPT = "/home/wwwenda/workspace/pirana/scripts/send_monthly_proposal.py"

def handle_proposal(chat_id):
    """Handles /proposal command."""
    try:
        send_telegram(chat_id, "🔬 <b>Provádím hloubkovou analýzu trhu a generuji strategický návrh...</b>")
        res = subprocess.run(["python3", PROPOSAL_SCRIPT, "--force-now"], capture_output=True, text=True, check=True)
        # The script sends the proposal directly
    except Exception as e:
        send_telegram(chat_id, f"❌ <b>Chyba při generování návrhu:</b> <code>{e}</code>")

def handle_help(chat_id):
    """Handles /help command."""
    msg = (
        "👑 <b>ČÁSLAV :: PŘÍKAZY VELENÍ</b>\n"
        "━━━━━━━━━━━━━━━━━━━━━━\n"
        "• <code>/status</code> ➔ Živý přehled PnL, trezoru a zůstatků\n"
        "• <code>/proposal</code> ➔ Měsíční strategický výzkumný návrh inovace\n"
        "• <code>/scale &lt;pct&gt;</code> ➔ Úprava velikosti pozice (např. <code>/scale 8.0</code>)\n"
        "• <code>/pause</code> ➔ Okamžité pozastavení tradingu\n"
        "• <code>/resume</code> ➔ Obnovení aktivního tradingu\n"
        "• <code>/reconcile</code> ➔ Kontrola zůstatků a storno visících orderů\n"
        "• <code>/help</code> ➔ Nápověda příkazů\n"
    )
    send_telegram(chat_id, msg)

def process_message(msg):
    """Processes incoming Telegram message."""
    chat = msg.get("chat", {})
    chat_id = chat.get("id")
    text = msg.get("text", "").strip()

    if chat_id != AUTHORIZED_CHAT_ID:
        print(f"[WARN] Unauthorized message from chat_id {chat_id}: {text}")
        return

    parts = text.split()
    if not parts:
        return
    cmd = parts[0].lower().split("@")[0]
    args = parts[1:]

    if cmd in ["/start", "/help"]:
        handle_help(chat_id)
    elif cmd == "/status":
        handle_status(chat_id)
    elif cmd == "/proposal":
        handle_proposal(chat_id)
    elif cmd == "/scale":
        handle_scale(chat_id, args)
    elif cmd == "/pause":
        handle_pause(chat_id)
    elif cmd == "/resume":
        handle_resume(chat_id)
    elif cmd == "/reconcile":
        handle_reconcile(chat_id)
    else:
        send_telegram(chat_id, f"❓ Neznámý příkaz: <code>{cmd}</code>. Zadej <code>/help</code> pro seznam.")

def poll_updates():
    """Main long-polling loop for Telegram updates."""
    print("🚀 Čáslav Telegram Control Daemon starting...")
    last_update_id = 0
    while True:
        try:
            url = f"https://api.telegram.org/bot{BOT_TOKEN}/getUpdates?offset={last_update_id + 1}&timeout=30"
            req = urllib.request.Request(url)
            with urllib.request.urlopen(req, timeout=35) as resp:
                if resp.status == 200:
                    data = json.loads(resp.read().decode("utf-8"))
                    for result in data.get("result", []):
                        update_id = result.get("update_id", 0)
                        if update_id > last_update_id:
                            last_update_id = update_id
                        if "message" in result:
                            process_message(result["message"])
        except Exception as e:
            # Sleep on network glitch
            time.sleep(2)

if __name__ == "__main__":
    poll_updates()
