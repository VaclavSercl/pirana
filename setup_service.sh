sudo tee /etc/systemd/system/pirana.service <<EOF
[Unit]
Description=Pirana HFT Bot
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=wwwenda
Group=wwwenda
WorkingDirectory=/home/wwwenda/workspace/pirana
EnvironmentFile=/home/wwwenda/workspace/pirana/.env
ExecStart=/home/wwwenda/workspace/pirana/target/release/pirana
Restart=always
RestartSec=3
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target
EOF

pkill pirana

sudo systemctl daemon-reload
sudo systemctl enable pirana.service
sudo systemctl start pirana.service
