#!/usr/bin/env python3
"""
Scheduled Institutional Report Generator for Vládce Čáslav 👑
Sends detailed system & trading status directly to Václav on Telegram.
"""

import sys
try:
    from .pirana_report import generate_report_data, format_telegram_html
except ImportError:  # Direct script invocation
    from pirana_report import generate_report_data, format_telegram_html

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
        print(f"Failed to send Telegram message: {type(e).__name__}", file=sys.stderr)
        return False

def build_report():
    """Report only validated account-scoped accounting, with unknowns preserved."""
    return format_telegram_html(generate_report_data(no_api=False, include_runtime=True))

def main():
    report_text = build_report()
    success = send_telegram(report_text)
    
    log_path = "/home/wwwenda/workspace/pirana/scheduled_1200.log"
    with open(log_path, "a", encoding="utf-8") as f:
        f.write(f"\n--- {datetime.now().isoformat()} ---\n")
        f.write(report_text)
        f.write(f"\nSent status: {success}\n")

    return 0 if success else 1

if __name__ == "__main__":
    sys.exit(main())
