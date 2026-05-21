sudo tee /etc/systemd/system/pirana-exporter.service <<EOF
[Unit]
Description=Pirana Prometheus Exporter
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=wwwenda
Group=wwwenda
WorkingDirectory=/home/wwwenda/workspace/pirana
ExecStart=/usr/bin/python3 /home/wwwenda/workspace/pirana/pirana_exporter.py
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now pirana-exporter.service
sudo systemctl status pirana-exporter.service --no-pager
