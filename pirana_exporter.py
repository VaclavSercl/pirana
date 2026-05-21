import time
import urllib.request
import json
from http.server import BaseHTTPRequestHandler, HTTPServer

class MetricsHandler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/metrics':
            try:
                req = urllib.request.urlopen("http://localhost:8080/api/snapshot", timeout=2)
                data = json.loads(req.read())
                
                metrics = []
                metrics.append("# HELP pirana_btc_price Current BTC Price")
                metrics.append("# TYPE pirana_btc_price gauge")
                metrics.append(f"pirana_btc_price {data.get('btc_price', 0.0)}")
                
                metrics.append("# HELP pirana_trades_today_total Total trades today")
                metrics.append("# TYPE pirana_trades_today_total counter")
                metrics.append(f"pirana_trades_today_total {data.get('trades_today', 0)}")
                
                metrics.append("# HELP pirana_daily_pnl_usd Daily PnL")
                metrics.append("# TYPE pirana_daily_pnl_usd gauge")
                metrics.append(f"pirana_daily_pnl_usd {data.get('daily_pnl', 0.0)}")
                
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

print("Pirana Prometheus Exporter starting on port 9091...")
server = HTTPServer(('0.0.0.0', 9091), MetricsHandler)
server.serve_forever()
