import os
from decimal import Decimal, InvalidOperation
import urllib.request
import json
from scripts.pirana_report import generate_report_data


def metric_sample(value, *, count=False):
    """Prometheus numeric sample; missing/invalid measurements remain unknown."""
    if isinstance(value, bool) or not isinstance(value, (int, float, str)):
        return "NaN"
    try:
        number = Decimal(str(value))
        if not number.is_finite():
            return "NaN"
        if count and (number < 0 or number != number.to_integral_value()):
            return "NaN"
        return str(number)
    except (InvalidOperation, ValueError):
        return "NaN"


def accounting_metrics():
    accounting = generate_report_data(no_api=True)["accounting"]
    complete = accounting["status"] == "complete"
    value = accounting["daily"].get("net_pnl_usd") if complete else None
    period = accounting.get("active_period")
    period_complete = isinstance(period, dict) and period.get("status") == "complete"
    period_value = period.get("net_pnl_usd") if period_complete else None
    return [
        "# HELP pirana_period_complete Selected accounting period completeness",
        "# TYPE pirana_period_complete gauge",
        f"pirana_period_complete {int(period_complete)}",
        "# HELP pirana_period_net_pnl_usd Account BTC/USD selected period net realized PnL; NaN when unverified",
        "# TYPE pirana_period_net_pnl_usd gauge",
        f"pirana_period_net_pnl_usd {metric_sample(period_value)}",
        "# HELP pirana_accounting_complete Canonical account accounting completeness",
        "# TYPE pirana_accounting_complete gauge",
        f"pirana_accounting_complete {int(complete)}",
        "# HELP pirana_daily_pnl_usd Account BTC/USD daily net realized PnL; NaN when unverified",
        "# TYPE pirana_daily_pnl_usd gauge",
        f"pirana_daily_pnl_usd {metric_sample(value)}",
    ]
from http.server import BaseHTTPRequestHandler, HTTPServer

class MetricsHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/metrics':
            try:
                req = urllib.request.urlopen(
                    os.environ.get("PIRANA_SNAPSHOT_URL", "http://127.0.0.1:8080/api/snapshot"),
                    timeout=2,
                )
                data = json.loads(req.read())
                
                metrics = []
                metrics.append("# HELP pirana_btc_price Current BTC Price")
                metrics.append("# TYPE pirana_btc_price gauge")
                metrics.append(f"pirana_btc_price {metric_sample(data.get('btc_price'))}")
                
                metrics.append("# HELP pirana_trades_today_total Total trades today")
                metrics.append("# TYPE pirana_trades_today_total gauge")
                metrics.append(f"pirana_trades_today_total {metric_sample(data.get('trades_today'), count=True)}")
                
                metrics.extend(accounting_metrics())

                self.send_response(200)
                self.send_header("Content-type", "text/plain")
                self.end_headers()
                self.wfile.write("\n".join(metrics).encode('utf-8') + b"\n")
            except Exception as e:
                self.send_response(500)
                self.end_headers()
                self.wfile.write(str(e).encode('utf-8'))
        else:
            self.send_response(404)
            self.end_headers()

    def log_message(self, format, *args):
        # Suppress periodic HTTP request logging to journalctl
        pass

if __name__ == "__main__":
    bind = os.environ.get("PIRANA_EXPORTER_BIND", "127.0.0.1")
    port = int(os.environ.get("PIRANA_EXPORTER_PORT", "9091"))
    print(f"Pirana Prometheus Exporter starting on {bind}:{port}...")
    server = HTTPServer((bind, port), MetricsHandler)
    server.serve_forever()
