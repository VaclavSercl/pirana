import re
import subprocess
import json
from datetime import datetime

def parse_logs():
    try:
        # Get all journalctl logs for the pirana service
        print("Fetching journalctl logs...")
        logs = subprocess.check_output(
            ["journalctl", "-u", "pirana.service"],
            text=True,
            errors="ignore"
        )
    except Exception as e:
        print(f"Error fetching logs: {e}")
        return

    lines = logs.splitlines()
    print(f"Loaded {len(lines)} log lines.")

    real_trades = []
    paper_trades = []
    errors = []
    warnings = []
    
    # Regex patterns
    # Real trade close: Position closed asynchronously successfully. PnL: -0.03 USD
    real_pnl_pat = re.compile(r"Position closed asynchronously successfully\.\s+PnL:\s+([\d.-]+)\s+USD")
    
    # Paper trade close: [PAPER TRADING] TP/SL Hit! Closed stínovou position (entry price: 60602, side: Buy). Realized PnL: 0.11 USD
    paper_pnl_pat = re.compile(r"\[PAPER TRADING\]\s+TP/SL Hit!\s+Closed stínovou position\s+\(entry price:\s+(\d+),\s+side:\s+(\w+)\)\.\s+Realized PnL:\s+([\d.-]+)\s+USD")

    # Order submissions & rejections
    order_sub_pat = re.compile(r"Order submitted successfully:")
    order_rej_pat = re.compile(r"Order rejected:")
    risk_rej_pat = re.compile(r"Trade rejected by Risk Engine:")
    inv_limit_pat = re.compile(r"inventory reached.*skipping")

    # Time bounds
    first_timestamp = None
    last_timestamp = None

    # Parse line by line
    for line in lines:
        # Extract log timestamp (e.g. Jun 05 17:02:05 or 2026-06-05T15:02:05.132432Z)
        # We can try to extract the ISO timestamp in the message body first
        timestamp_match = re.search(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?Z)", line)
        ts = None
        if timestamp_match:
            ts = timestamp_match.group(1)
            if not first_timestamp:
                first_timestamp = ts
            last_timestamp = ts

        # Real trades
        m_real = real_pnl_pat.search(line)
        if m_real:
            pnl_val = float(m_real.group(1))
            real_trades.append({
                "timestamp": ts or "Unknown",
                "pnl": pnl_val,
                "raw": line
            })
            continue

        # Paper trades
        m_paper = paper_pnl_pat.search(line)
        if m_paper:
            entry = float(m_paper.group(1))
            side = m_paper.group(2)
            pnl_val = float(m_paper.group(3))
            paper_trades.append({
                "timestamp": ts or "Unknown",
                "entry_price": entry,
                "side": side,
                "pnl": pnl_val,
                "raw": line
            })
            continue

        # Error tracking
        if "ERROR" in line or "error" in line.lower() or order_rej_pat.search(line):
            errors.append({
                "timestamp": ts or "Unknown",
                "msg": line
            })
        elif "WARN" in line or risk_rej_pat.search(line) or inv_limit_pat.search(line):
            warnings.append({
                "timestamp": ts or "Unknown",
                "msg": line
            })

    # Summarize stats
    def calculate_stats(trades):
        if not trades:
            return {
                "count": 0, "total_pnl": 0.0, "wins": 0, "losses": 0,
                "win_rate": 0.0, "avg_pnl": 0.0, "best": 0.0, "worst": 0.0,
                "profit_factor": 0.0
            }
        
        pnls = [t["pnl"] for t in trades]
        count = len(pnls)
        total_pnl = sum(pnls)
        wins = sum(1 for p in pnls if p > 0)
        losses = sum(1 for p in pnls if p <= 0)
        win_rate = (wins / count) * 100.0 if count > 0 else 0.0
        avg_pnl = total_pnl / count if count > 0 else 0.0
        best = max(pnls)
        worst = min(pnls)
        
        gross_profits = sum(p for p in pnls if p > 0)
        gross_losses = abs(sum(p for p in pnls if p < 0))
        profit_factor = gross_profits / gross_losses if gross_losses > 0 else (gross_profits if gross_profits > 0 else 1.0)

        return {
            "count": count,
            "total_pnl": total_pnl,
            "wins": wins,
            "losses": losses,
            "win_rate": win_rate,
            "avg_pnl": avg_pnl,
            "best": best,
            "worst": worst,
            "profit_factor": profit_factor
        }

    real_stats = calculate_stats(real_trades)
    paper_stats = calculate_stats(paper_trades)

    # Fetch snapshot from API
    api_snapshot = {}
    try:
        import urllib.request
        with urllib.request.urlopen("http://localhost:80/api/snapshot", timeout=5) as response:
            api_snapshot = json.loads(response.read().decode('utf-8'))
    except Exception as e:
        print(f"Could not reach local API: {e}")

    results = {
        "time_range": {
            "start": first_timestamp,
            "end": last_timestamp
        },
        "real_trades_stats": real_stats,
        "paper_trades_stats": paper_stats,
        "recent_real_trades": real_trades[-10:],
        "recent_paper_trades": paper_trades[-10:],
        "api_snapshot": api_snapshot,
        "total_errors": len(errors),
        "total_warnings": len(warnings),
        "recent_errors": [e["msg"] for e in errors[-5:]],
        "recent_warnings": [w["msg"] for w in warnings[-5:]]
    }

    # Print results in JSON so it can be parsed
    print("---ANALYSIS_RESULT_START---")
    print(json.dumps(results, indent=2))
    print("---ANALYSIS_RESULT_END---")

if __name__ == "__main__":
    parse_logs()
