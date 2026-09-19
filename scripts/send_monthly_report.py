#!/usr/bin/env python3
"""
Čáslav :: Executive Monthly Institutional Audit & Report Generator
Runs on the 1st of every month to produce a comprehensive audit of trading performance,
capital accumulation in BTC vault, exchange volume, and system stability for the preceding month.
"""

try:
    from .pirana_report import generate_report_data, format_telegram_html
except ImportError:
    from pirana_report import generate_report_data, format_telegram_html
import html

import sys
import os
import time
import json
import calendar
import argparse
import subprocess
import urllib.request
import urllib.error
import hmac
import hashlib
from datetime import datetime, timezone, timedelta

REPO_DIR = "/home/wwwenda/workspace/pirana"
ENV_FILE = os.path.join(REPO_DIR, ".env")
SNAPSHOT_URL = "http://localhost:80/api/snapshot"

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

def get_snapshot():
    try:
        req = urllib.request.Request(SNAPSHOT_URL, headers={"User-Agent": "Caslav-MonthlyAudit/1.0"})
        with urllib.request.urlopen(req, timeout=10) as resp:
            if resp.status == 200:
                return json.loads(resp.read().decode())
    except Exception as e:
        print(f"[WARN] Failed to fetch snapshot: {e}", file=sys.stderr)
    return None

def calculate_time_window(force_now=False):
    """
    Returns (start_dt, end_dt, start_ms, end_ms, label)
    For standard run on 1st of month: strictly previous calendar month (00:00:00 - 23:59:59 UTC).
    For force_now: past 30 days up to current moment.
    """
    now_utc = datetime.now(timezone.utc)
    if force_now:
        start_dt = now_utc - timedelta(days=30)
        end_dt = now_utc
        label = f"{start_dt.strftime('%d.%m.%Y')} – {end_dt.strftime('%d.%m.%Y')} (Posledních 30 dní)"
    else:
        # First day of current month
        first_of_this_month = datetime(now_utc.year, now_utc.month, 1, 0, 0, 0, tzinfo=timezone.utc)
        # Last day of previous month
        last_of_prev_month = first_of_this_month - timedelta(seconds=1)
        prev_year = last_of_prev_month.year
        prev_month = last_of_prev_month.month
        num_days = calendar.monthrange(prev_year, prev_month)[1]
        
        start_dt = datetime(prev_year, prev_month, 1, 0, 0, 0, tzinfo=timezone.utc)
        end_dt = datetime(prev_year, prev_month, num_days, 23, 59, 59, 999000, tzinfo=timezone.utc)
        label = f"{start_dt.strftime('%d.%m.%Y')} – {end_dt.strftime('%d.%m.%Y')} (Celý kalendářní měsíc)"

    start_ms = int(start_dt.timestamp() * 1000)
    end_ms = int(end_dt.timestamp() * 1000)
    return start_dt, end_dt, start_ms, end_ms, label

def fetch_bitfinex_trades(api_key, api_secret, start_ms, end_ms):
    """Fetches executed trades within the given time window from Bitfinex REST API."""
    if not api_key or not api_secret:
        return []
    
    nonce = str(int(time.time() * 1000000))
    path = "/api/v2/auth/r/trades/hist"
    body = json.dumps({"limit": 1000, "start": start_ms, "end": end_ms})
    sig_payload = f"/api/v2/auth/r/trades/hist{nonce}{body}"
    sig = hmac.new(api_secret.encode(), sig_payload.encode(), hashlib.sha384).hexdigest()

    req = urllib.request.Request(
        "https://api.bitfinex.com/v2/auth/r/trades/hist",
        data=body.encode(),
        headers={
            "bfx-nonce": nonce,
            "bfx-apikey": api_key,
            "bfx-signature": sig,
            "Content-Type": "application/json"
        }
    )

    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return json.loads(resp.read().decode())
    except Exception as e:
        print(f"[WARN] Bitfinex trade query error: {e}", file=sys.stderr)
        return []

def analyze_trades(raw_trades):
    """
    Performs FIFO matching on raw trades to compute round-trip PnL, Win Rate,
    Profit Factor, Payoff Ratio, Volumes and Fees.
    """
    if not raw_trades:
        return {
            "total_roundtrips": 0, "wins": 0, "losses": 0, "be_trades": 0,
            "win_rate": 0.0, "net_pnl": 0.0, "gross_profit": 0.0, "gross_loss": 0.0,
            "profit_factor": 0.0, "payoff_ratio": 0.0, "avg_win": 0.0, "avg_loss": 0.0,
            "max_win_usd": 0.0, "max_win_roi": 0.0, "max_loss_usd": 0.0,
            "total_vol_btc": 0.0, "total_vol_usd": 0.0, "saved_fees_usd": 0.0,
            "month_locked_btc": 0.0, "max_drawdown_pct": 0.0
        }

    # Sort chronologically (oldest first)
    raw_trades_sorted = sorted(raw_trades, key=lambda t: t[2])

    inventory = []
    closed_trades = []
    total_vol_btc = 0.0
    total_vol_usd = 0.0

    for t in raw_trades_sorted:
        tid, pair, mts, oid, amount, price = t[0], t[1], t[2], t[3], t[4], t[5]
        amt_abs = abs(amount)
        total_vol_btc += amt_abs
        total_vol_usd += amt_abs * price

        if amount > 0:
            inventory.append({"mts": mts, "amount": amount, "price": price, "tid": tid})
        elif amount < 0:
            sell_qty = amt_abs
            while sell_qty > 1e-8 and inventory:
                buy = inventory[0]
                matched_qty = min(buy["amount"], sell_qty)
                pnl_usd = matched_qty * (price - buy["price"])
                roi_pct = ((price - buy["price"]) / buy["price"]) * 100.0 if buy["price"] > 0 else 0.0

                closed_trades.append({
                    "buy_time": buy["mts"],
                    "sell_time": mts,
                    "qty": matched_qty,
                    "entry_price": buy["price"],
                    "exit_price": price,
                    "pnl_usd": pnl_usd,
                    "roi_pct": roi_pct
                })

                buy["amount"] -= matched_qty
                sell_qty -= matched_qty
                if buy["amount"] < 1e-8:
                    inventory.pop(0)

    total_roundtrips = len(closed_trades)
    wins = sum(1 for t in closed_trades if t["pnl_usd"] > 1e-6)
    losses = sum(1 for t in closed_trades if t["pnl_usd"] < -1e-6)
    be_trades = total_roundtrips - wins - losses

    win_rate = (wins / total_roundtrips * 100.0) if total_roundtrips > 0 else 0.0
    net_pnl = sum(t["pnl_usd"] for t in closed_trades)
    gross_profit = sum(t["pnl_usd"] for t in closed_trades if t["pnl_usd"] > 0)
    gross_loss = abs(sum(t["pnl_usd"] for t in closed_trades if t["pnl_usd"] < 0))

    profit_factor = (gross_profit / gross_loss) if gross_loss > 0 else (999.99 if gross_profit > 0 else 0.0)
    avg_win = (gross_profit / wins) if wins > 0 else 0.0
    avg_loss = (gross_loss / losses) if losses > 0 else 0.0
    payoff_ratio = (avg_win / avg_loss) if avg_loss > 0 else (999.99 if avg_win > 0 else 0.0)

    max_win_usd = max((t["pnl_usd"] for t in closed_trades), default=0.0)
    max_win_roi = max((t["roi_pct"] for t in closed_trades), default=0.0)
    max_loss_usd = min((t["pnl_usd"] for t in closed_trades), default=0.0)

    # 10% skimmer on profit
    month_locked_btc = sum((t["pnl_usd"] / t["exit_price"] * 0.10) for t in closed_trades if t["pnl_usd"] > 0 and t["exit_price"] > 0)

    # Saved fees on 0% Zero Fee (compared to standard 0.10% taker fee)
    saved_fees_usd = total_vol_usd * 0.0010

    # Max Drawdown calculation over equity curve
    cumulative_pnl = 0.0
    peak_pnl = 0.0
    max_dd_usd = 0.0
    for t in closed_trades:
        cumulative_pnl += t["pnl_usd"]
        if cumulative_pnl > peak_pnl:
            peak_pnl = cumulative_pnl
        dd = peak_pnl - cumulative_pnl
        if dd > max_dd_usd:
            max_dd_usd = dd

    max_drawdown_pct = (max_dd_usd / 393.56 * 100.0) if max_dd_usd > 0 else 0.0

    return {
        "total_roundtrips": total_roundtrips,
        "wins": wins,
        "losses": losses,
        "be_trades": be_trades,
        "win_rate": win_rate,
        "net_pnl": net_pnl,
        "gross_profit": gross_profit,
        "gross_loss": gross_loss,
        "profit_factor": profit_factor,
        "payoff_ratio": payoff_ratio,
        "avg_win": avg_win,
        "avg_loss": avg_loss,
        "max_win_usd": max_win_usd,
        "max_win_roi": max_win_roi,
        "max_loss_usd": max_loss_usd,
        "total_vol_btc": total_vol_btc,
        "total_vol_usd": total_vol_usd,
        "saved_fees_usd": saved_fees_usd,
        "month_locked_btc": month_locked_btc,
        "max_drawdown_pct": max_drawdown_pct
    }

def get_git_status():
    try:
        res = subprocess.run(["git", "status", "-uno"], cwd=REPO_DIR, capture_output=True, text=True, timeout=5)
        if "up to date" in res.stdout or "ahead" in res.stdout:
            return "Plně synchronizováno s origin/main"
        return "Lokální repozitář aktivní"
    except Exception:
        return "Aktivní"

def build_monthly_report(time_label, stats, snapshot):
    """Current projections cannot establish a requested calendar-month return."""
    return (
        "<b>PIRANA — MĚSÍČNÍ REPORT</b>\n"
        f"Období: {html.escape(str(time_label))}\n"
        "Měsíční PnL, poplatky, výnos, uptime a akumulace BTC: NEOVĚŘENO.\n"
        "Chybí kanonická projekce požadovaného měsíce.\n"
        "Následuje aktuální stav, nikoli měsíční výsledek.\n\n"
        + format_telegram_html(generate_report_data(no_api=False, include_runtime=True))
    )

def send_telegram(token, chat_id, text):
    if not token or not chat_id:
        print("[ERROR] Missing Telegram Token or Chat ID", file=sys.stderr)
        return False

    url = f"https://api.telegram.org/bot{token}/sendMessage"
    payload = {
        "chat_id": chat_id,
        "text": text,
        "parse_mode": "HTML",
        "disable_web_page_preview": True
    }
    data = json.dumps(payload).encode("utf-8")
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})

    for attempt in range(1, 4):
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                if resp.status == 200:
                    print(f"[OK] Monthly Report successfully delivered to Telegram on attempt {attempt}.")
                    return True
        except Exception as e:
            print(f"[WARN] Telegram delivery attempt {attempt} failed: {type(e).__name__}", file=sys.stderr)
            time.sleep(3)
    return False

def main():
    parser = argparse.ArgumentParser(description="Executive Monthly Report Generator for Caslav")
    parser.add_argument("--dry-run", action="store_true", help="Print report to stdout without sending to Telegram")
    parser.add_argument("--force-now", action="store_true", help="Generate and send report immediately for the last 30 days")
    args = parser.parse_args()

    env = load_env()
    tg_token = env.get("TELEGRAM_BOT_TOKEN") or env.get("CASLAV_TELEGRAM_TOKEN")
    tg_chat_id = env.get("TELEGRAM_CHAT_ID") or env.get("CASLAV_ALLOWED_USER_ID")
    _, _, _, _, time_label = calculate_time_window(force_now=args.force_now)
    report_text = build_monthly_report(time_label, None, None)

    if args.dry_run:
        print("\n==================== [MONTHLY REPORT DRY RUN] ====================")
        print(report_text)
        print("==================================================================\n")
        return 0

    success = send_telegram(tg_token, tg_chat_id, report_text)
    
    # Save log
    log_file = os.path.join(REPO_DIR, "monthly_audit.log")
    with open(log_file, "a", encoding="utf-8") as f:
        f.write(f"\n--- [{datetime.now().isoformat()}] ---\n")
        f.write(report_text)
        f.write(f"\nSent status: {success}\n")

    return 0 if success else 1

if __name__ == "__main__":
    sys.exit(main())
