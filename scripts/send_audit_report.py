#!/usr/bin/env python3
"""
Čáslav :: Aktuální kanonický a provozní report na Telegram.
Rozdělí report na části podle limitu Telegramu (4096 znaků) a odešle je v pořadí.
"""

import argparse
import html
import json
import subprocess
import os
import sys
import time
import urllib.request
import urllib.parse

ENV_FILE = "/home/wwwenda/workspace/pirana/.env"
MAX_LEN = 3800  # rezerva pod limit 4096


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


def probe(argv):
    try:
        result = subprocess.run(argv, capture_output=True, text=True, timeout=8, check=False)
        return result.stdout.strip() if result.returncode == 0 else None
    except (OSError, subprocess.TimeoutExpired):
        return None


def runtime_evidence():
    head = probe(["git", "-C", "/home/wwwenda/workspace/pirana", "rev-parse", "HEAD"])
    args = ["systemctl", "show", "pirana.service", "--property=MainPID,ActiveState,NRestarts,ExecMainStartTimestamp"]
    before = probe(args)
    fields = dict(line.split("=", 1) for line in (before or "").splitlines() if "=" in line)
    digest = None
    pid = fields.get("MainPID", "")
    if pid.isascii() and pid.isdecimal() and int(pid) > 0:
        digest = probe(["sha256sum", "/proc/" + pid + "/exe"])
        digest = digest.split()[0] if digest else None
        if not digest or len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
            digest = None
    if probe(args) != before:
        digest = None
    return "\n".join([
        "Checkout HEAD: " + (head or "NEOVĚŘENO"),
        "Binárka SHA-256: " + (digest or "NEOVĚŘENO"),
        "Služba: " + fields.get("ActiveState", "NEOVĚŘENO") + "; PID: " + fields.get("MainPID", "NEOVĚŘENO"),
        "Start: " + fields.get("ExecMainStartTimestamp", "NEOVĚŘENO") + "; NRestarts: " + fields.get("NRestarts", "NEOVĚŘENO"),
        "HEAD a hash jsou samostatné údaje; shoda sestavení není doložena."])


def chunks(text):
    return [html.escape(text[i:i+3000]) for i in range(0, len(text), 3000)]


def build_report():
    from datetime import datetime, timezone
    try:
        from scripts.pirana_report import generate_report_data, format_text_report
    except ModuleNotFoundError:
        from pirana_report import generate_report_data, format_text_report
    now = datetime.now(timezone.utc)
    data = generate_report_data(include_runtime=True, now_arg=now.isoformat())
    return ("Aktuální kontrola Pirana\nUTC: " + now.isoformat() + "\n"
            + runtime_evidence() + "\n" + format_text_report(data)
            + "\nTýdenní výkonnost, drawdown a skluz: NEOVĚŘENO pro přesné týdenní období."
            + "\nTesty a audit kódu nebyly touto kontrolou spuštěny.")


def send(token, chat_id, text, idx, total):
    url = f"https://api.telegram.org/bot{token}/sendMessage"
    payload = urllib.parse.urlencode({
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
        "disable_web_page_preview": "true",
    }).encode("utf-8")
    req = urllib.request.Request(url, data=payload, method="POST")
    with urllib.request.urlopen(req, timeout=20) as resp:
        ok = resp.status == 200 and json.loads(resp.read(65536)).get("ok") is True
        print(f"[{'OK' if ok else 'FAIL'}] část {idx}/{total} "
              f"({len(text)} znaků) status={resp.status}")
        return ok


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    report = build_report()
    if args.dry_run:
        print(report)
        return 0
    env = load_env()
    token = os.environ.get("TELEGRAM_BOT_TOKEN", env.get("TELEGRAM_BOT_TOKEN"))
    chat_id = os.environ.get("TELEGRAM_CHAT_ID", env.get("TELEGRAM_CHAT_ID"))

    if not token or not chat_id:
        print("[ERROR] TELEGRAM_BOT_TOKEN není v .env", file=sys.stderr)
        return 1

    parts = chunks(report)
    total = len(parts)
    sent = 0
    for i, part in enumerate(parts, start=1):
        if len(part) > 4096:
            print(f"[WARN] část {i} má {len(part)} znaků — nad limit!",
                  file=sys.stderr)
        try:
            if send(token, chat_id, part, i, total):
                sent += 1
            else:
                return 1
        except Exception as e:
            print(f"[ERROR] část {i}: delivery failed or uncertain", file=sys.stderr)
            return 1
        time.sleep(1.2)  # rate limit Telegramu

    print(f"\nOdesláno {sent}/{total} částí na chat_id={chat_id}")
    return 0 if sent == total else 1


if __name__ == "__main__":
    sys.exit(main())
