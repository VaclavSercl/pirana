sudo tee /etc/grafana/provisioning/datasources/prometheus.yaml <<EOF
apiVersion: 1
datasources:
  - name: Prometheus
    type: prometheus
    url: http://localhost:9090
    access: proxy
    isDefault: true
EOF

sudo tee /etc/grafana/provisioning/dashboards/dashboards.yaml <<EOF
apiVersion: 1
providers:
  - name: 'Pirana'
    orgId: 1
    folder: 'Pirana HFT'
    type: file
    disableDeletion: false
    editable: true
    options:
      path: /etc/grafana/provisioning/dashboards
EOF

sudo mkdir -p /etc/grafana/provisioning/dashboards

sudo tee /etc/grafana/provisioning/dashboards/pirana.json <<EOF
{
  "title": "Pirana HFT Live Dashboard",
  "timezone": "browser",
  "panels": [
    {
      "type": "timeseries",
      "title": "BTC/USD Price",
      "gridPos": { "h": 8, "w": 24, "x": 0, "y": 0 },
      "targets": [
        {
          "expr": "pirana_btc_price",
          "legendFormat": "Price"
        }
      ]
    },
    {
      "type": "stat",
      "title": "Trades Today",
      "gridPos": { "h": 4, "w": 12, "x": 0, "y": 8 },
      "targets": [
        {
          "expr": "pirana_trades_today_total",
          "legendFormat": "Trades"
        }
      ]
    },
    {
      "type": "timeseries",
      "title": "Daily PnL (USD)",
      "gridPos": { "h": 8, "w": 24, "x": 0, "y": 12 },
      "targets": [
        {
          "expr": "pirana_daily_pnl_usd",
          "legendFormat": "PnL"
        }
      ]
    }
  ]
}
EOF

if ! grep -q "pirana" /etc/prometheus/prometheus.yml; then
sudo tee -a /etc/prometheus/prometheus.yml <<EOF
  - job_name: 'pirana'
    static_configs:
      - targets: ['localhost:9091']
EOF
fi

sudo systemctl restart prometheus
sudo systemctl restart grafana-server
