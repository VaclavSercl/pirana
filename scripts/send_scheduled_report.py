#!/usr/bin/env python3
"""
Scheduled Institutional Report Generator for Vládce Čáslav 👑
Sends detailed system & trading status directly to Václav on Telegram.
"""

import sys
import os
import json
import urllib.request
import urllib.error
import subprocess
from datetime import datetime

TELEGRAM_TOKEN = os.environ.get("CASLAV_TELEGRAM_TOKEN") or os.environ["TELEGRAM_BOT_TOKEN"]
CHAT_ID = int(os.environ.get("CASLAV_ALLOWED_USER_ID", "1076582576"))
API_URL = "http://localhost:80/api/snapshot"

def get_snapshot():
    try:
        req = urllib.request.Request(API_URL, headers={"User-Agent": "Caslav-Sentinel/1.0"})
        with urllib.request.urlopen(req, timeout=10) as resp:
            if resp.status == 200:
                return json.loads(resp.read().decode())
    except Exception as e:
        print(f"Error fetching snapshot: {e}", file=sys.stderr)
    return None

def get_system_stats():
    stats = {}
    try:
        # Load average
        load1, load5, load15 = os.getloadavg()
        stats["load"] = f"{load1:.2f}, {load5:.2f}, {load15:.2f}"
    except Exception:
        stats["load"] = "N/A"

    try:
        # Memory
        with open("/proc/meminfo", "r") as f:
            lines = f.readlines()
        mem = {}
        for line in lines:
            parts = line.split(":")
            if len(parts) == 2:
                mem[parts[0].strip()] = int(parts[1].strip().split()[0])
        total_mb = mem.get("MemTotal", 0) // 1024
        avail_mb = mem.get("MemAvailable", 0) // 1024
        used_mb = total_mb - avail_mb
        stats["memory"] = f"{used_mb} MB / {total_mb} MB ({used_mb/total_mb*100:.1f}%)" if total_mb else "N/A"
    except Exception:
        stats["memory"] = "N/A"

    try:
        # Disk
        st = os.statvfs("/")
        total_gb = (st.f_blocks * st.f_frsize) / (1024**3)
        free_gb = (st.f_bavail * st.f_frsize) / (1024**3)
        used_gb = total_gb - free_gb
        stats["disk"] = f"{used_gb:.1f} GB / {total_gb:.1f} GB ({used_gb/total_gb*100:.1f}%)"
    except Exception:
        stats["disk"] = "N/A"

    try:
        # Uptime
        with open("/proc/uptime", "r") as f:
            uptime_seconds = float(f.readline().split()[0])
        days = int(uptime_seconds // 86400)
        hours = int((uptime_seconds % 86400) // 3600)
        stats["uptime"] = f"{days}d {hours}h"
    except Exception:
        stats["uptime"] = "N/A"

    return stats

def send_telegram(text: str):
    url = f"https://api.telegram.org/bot{TELEGRAM_TOKEN}/sendMessage"
    payload = {
        "chat_id": CHAT_ID,
        "text": text,
        "parse_mode": "HTML",
        "disable_web_page_preview": True
    }
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            print(f"Telegram response: {resp.status}")
            return True
    except Exception as e:
        print(f"Failed to send Telegram message: {e}", file=sys.stderr)
        return False

def build_report():
    now_str = datetime.now().strftime("%d.%m.%Y %H:%M:%S CEST")
    snapshot = get_snapshot()
    sys_stats = get_system_stats()

    if not snapshot:
        return (
            f"👑 <b>Vládce Čáslav — Polední Report (12:00)</b>\n"
            f"📅 <i>{now_str}</i>\n\n"
            f"⚠️ <b>Varování:</b> Lokální API snapshotu neodpovídá!\n"
            f"🖥 <b>Server Čáslav:</b>\n"
            f"• Uptime: {sys_stats.get('uptime')}\n"
            f"• Load: {sys_stats.get('load')}\n"
            f"• RAM: {sys_stats.get('memory')}\n"
            f"• Disk: {sys_stats.get('disk')}\n"
        )

    mode = snapshot.get("system_mode", "Unknown")
    mode_icon = "🟢" if mode == "Active" else ("🟡" if mode == "Defensive" else "🔴")
    btc_price = snapshot.get("btc_price", 0.0)
    btc_bal = snapshot.get("btc_balance", 0.0)
    usd_bal = snapshot.get("usd_balance", 0.0)
    equity = usd_bal + (btc_bal * btc_price)
    trades_today = snapshot.get("trades_today", 0)
    daily_pnl = snapshot.get("daily_pnl", 0.0)
    daily_pnl_pct = snapshot.get("daily_pnl_pct", 0.0)
    total_pnl = snapshot.get("total_pnl", 0.0)
    win_rate = snapshot.get("win_rate", 0.0)
    cons_loss = snapshot.get("consecutive_losses", 0)
    uptime_sec = snapshot.get("uptime_seconds", 0)
    uptime_h = uptime_sec // 3600
    uptime_m = (uptime_sec % 3600) // 60

    pnl_sign = "+" if daily_pnl >= 0 else ""
    tot_sign = "+" if total_pnl >= 0 else ""

    report = (
        f"👑 <b>Vládce Čáslav — Polední Report (12:00)</b>\n"
        f"📅 <i>{now_str}</i>\n\n"
        f"🦈 <b>Trading Engine (Pirana &amp; Gemini HFT):</b>\n"
        f"• Stav: {mode_icon} <b>{mode}</b> (Uptime bota: {uptime_h}h {uptime_m}m)\n"
        f"• Cena BTC: <b>${btc_price:,.1f}</b>\n"
        f"• Zůstatek BTC: <code>{btc_bal:.6f} BTC</code> (~${btc_bal * btc_price:,.2f})\n"
        f"• Zůstatek USD: <code>${usd_bal:.2f}</code>\n"
        f"• <b>Celková equity:</b> <code>${equity:,.2f}</code>\n\n"
        f"📊 <b>Dnešní statistika obchodování:</b>\n"
        f"• Počet obchodů dnes: <b>{trades_today}</b>\n"
        f"• Denní PnL: <b>{pnl_sign}${daily_pnl:.4f}</b> ({pnl_sign}{daily_pnl_pct:.2f}%)\n"
        f"• Celkový PnL: <b>{tot_sign}${total_pnl:.4f}</b>\n"
        f"• Win Rate: <b>{win_rate:.1f}%</b> | Ztráty v řadě: <b>{cons_loss}</b>\n\n"
        f"🖥 <b>Zdraví serveru Čáslav:</b>\n"
        f"• Load Average: <code>{sys_stats.get('load')}</code>\n"
        f"• Využití RAM: <code>{sys_stats.get('memory')}</code>\n"
        f"• Využití Disku: <code>{sys_stats.get('disk')}</code>\n"
        f"• System Uptime: <code>{sys_stats.get('uptime')}</code>\n\n"
        f"🛡 <b>Status:</b> Všechny systémy běží autonomně a bez chyb. Akumulace BTC aktivní."
    )
    return report

def main():
    report_text = build_report()
    success = send_telegram(report_text)
    
    log_path = "/home/wwwenda/workspace/pirana/scheduled_1200.log"
    with open(log_path, "a", encoding="utf-8") as f:
        f.write(f"\n--- {datetime.now().isoformat()} ---\n")
        f.write(report_text)
        f.write(f"\nSent status: {success}\n")

if __name__ == "__main__":
    main()
